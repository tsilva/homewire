use crate::{Result, config::Memory, error, tuya::Client};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::{thread, time::Duration};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct State {
    pub power: bool,
    pub mode: String,
    pub target_humidity: u8,
    pub current_humidity: Option<u8>,
    pub fault_code: u32,
    pub tank_full: bool,
    pub defrost: bool,
    pub problem: bool,
}
impl State {
    pub fn from_dps(dps: &Map<String, Value>) -> Result<Self> {
        let power = dps
            .get("1")
            .and_then(Value::as_bool)
            .ok_or_else(|| error("Missing or invalid power state"))?;
        let mode = dps
            .get("4")
            .and_then(Value::as_str)
            .ok_or_else(|| error("Missing or invalid mode"))?
            .to_owned();
        if !matches!(mode.as_str(), "manual" | "purify" | "sleep" | "laundry") {
            return Err(error("Unrecognized Meaco mode"));
        }
        let target_humidity =
            humidity(dps.get("2"))?.ok_or_else(|| error("Missing target humidity"))?;
        let current_humidity = humidity(dps.get("16"))?;
        let fault_code = dps
            .get("19")
            .and_then(Value::as_u64)
            .and_then(|n| u32::try_from(n).ok())
            .ok_or_else(|| error("Missing or invalid fault code"))?;
        Ok(Self {
            power,
            mode,
            target_humidity,
            current_humidity,
            fault_code,
            tank_full: fault_code == 1,
            defrost: fault_code == 2,
            problem: !matches!(fault_code, 0 | 1 | 2 | 128),
        })
    }
}
fn humidity(value: Option<&Value>) -> Result<Option<u8>> {
    value
        .map(|v| {
            v.as_u64()
                .filter(|n| *n <= 100)
                .map(|n| n as u8)
                .ok_or_else(|| error("Invalid humidity reading"))
        })
        .transpose()
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Operation {
    Day,
    Night,
    Off,
}
impl std::str::FromStr for Operation {
    type Err = Box<dyn std::error::Error + Send + Sync>;
    fn from_str(s: &str) -> Result<Self> {
        match s {
            "day" => Ok(Self::Day),
            "night" => Ok(Self::Night),
            "off" => Ok(Self::Off),
            _ => Err(error("Operation must be day, night or off")),
        }
    }
}
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "command", content = "value", rename_all = "snake_case")]
pub enum Action {
    Power(bool),
    Humidity(u8),
    Mode(String),
}
impl Action {
    pub fn dps(&self) -> Map<String, Value> {
        let object = match self {
            Self::Power(v) => json!({"1":v}),
            Self::Humidity(v) => json!({"2":v}),
            Self::Mode(v) => json!({"4":v}),
        };
        object.as_object().cloned().unwrap_or_default()
    }
}
pub trait Device {
    fn read(&mut self) -> Result<State>;
    fn command(&mut self, action: &Action) -> Result<()>;
    fn settle(&mut self);
}
pub struct NetworkDevice {
    pub client: Client,
    pub settle_seconds: u64,
}
impl Device for NetworkDevice {
    fn read(&mut self) -> Result<State> {
        State::from_dps(&self.client.status()?)
    }
    fn command(&mut self, action: &Action) -> Result<()> {
        self.client.set(action.dps())
    }
    fn settle(&mut self) {
        thread::sleep(Duration::from_secs(self.settle_seconds));
    }
}

pub fn already_correct(state: &State, operation: Operation, memory: &Memory) -> bool {
    match operation {
        Operation::Off => !state.power,
        Operation::Day => state.power && state.mode == "manual" && state.target_humidity == 50,
        Operation::Night => {
            state.power && state.mode == "sleep" && memory.base_mode.as_deref() == Some("purify")
        }
    }
}

pub fn plan(state: &State, operation: Operation, memory: &Memory) -> Vec<Action> {
    if already_correct(state, operation, memory) {
        return vec![];
    }
    if operation == Operation::Off {
        return vec![Action::Power(false)];
    }
    let mut steps = vec![];
    if !state.power {
        steps.push(Action::Power(true));
    }
    if state.mode == "sleep" {
        steps.push(Action::Mode("sleep".into()));
    }
    if operation == Operation::Day && state.target_humidity != 50 {
        steps.push(Action::Humidity(50));
    }
    let base = if operation == Operation::Day {
        "manual"
    } else {
        "purify"
    };
    // Clearing sleep reveals an unknown underlying mode; a plan is provisional.
    if state.mode != base {
        steps.push(Action::Mode(base.into()));
    }
    if operation == Operation::Night {
        steps.push(Action::Mode("sleep".into()));
    }
    steps
}

#[derive(Serialize)]
pub struct Applied {
    pub operation: Operation,
    pub changed: bool,
    pub state: State,
}

pub fn apply<D: Device>(
    device: &mut D,
    operation: Operation,
    memory: &mut Memory,
    mut persist: impl FnMut(&Memory) -> Result<()>,
) -> Result<Applied> {
    let mut state = device.read()?;
    if already_correct(&state, operation, memory) {
        return Ok(Applied {
            operation,
            changed: false,
            state,
        });
    }
    // Invalidate the old assumption BEFORE touching the device. If a command
    // outcome is ambiguous, no later invocation assumes a known sleep base.
    memory.base_mode = None;
    persist(memory)?;
    if operation == Operation::Off {
        state = send_verified(device, &Action::Power(false))?;
        if state.power {
            return Err(error("Meaco did not remain off"));
        }
        return Ok(Applied {
            operation,
            changed: true,
            state,
        });
    }
    if !state.power {
        state = send_verified(device, &Action::Power(true))?;
        powered(&state)?;
    }
    if state.mode == "sleep" {
        state = send_verified(device, &Action::Mode("sleep".into()))?;
        powered(&state)?;
        if state.mode == "sleep" {
            return Err(error(
                "Could not confirm sleep cleared; refusing to toggle again",
            ));
        }
    }
    if operation == Operation::Day && state.target_humidity != 50 {
        state = send_verified(device, &Action::Humidity(50))?;
        powered(&state)?;
        if state.target_humidity != 50 {
            return Err(error("Humidity target did not persist"));
        }
    }
    let base = if operation == Operation::Day {
        "manual"
    } else {
        "purify"
    };
    if state.mode != base {
        powered(&state)?;
        state = send_verified(device, &Action::Mode(base.into()))?;
    }
    powered(&state)?;
    if state.mode != base || (operation == Operation::Day && state.target_humidity != 50) {
        return Err(error("Underlying mode or humidity target did not persist"));
    }
    if operation == Operation::Night {
        state = send_verified(device, &Action::Mode("sleep".into()))?;
        powered(&state)?;
        if state.mode != "sleep" {
            return Err(error(
                "Night mode did not persist; refusing to toggle again",
            ));
        }
    }
    memory.base_mode = Some(base.into());
    persist(memory)?;
    Ok(Applied {
        operation,
        changed: true,
        state,
    })
}
fn powered(state: &State) -> Result<()> {
    if !state.power {
        return Err(error("Device switched off during the sequence"));
    }
    Ok(())
}
fn send_verified<D: Device>(device: &mut D, action: &Action) -> Result<State> {
    // Never resend a command after a timeout: mode=sleep is a toggle.
    device.command(action)?;
    device.settle();
    device.read()
}
