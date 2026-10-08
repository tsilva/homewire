//! Deterministic household dehumidifier routines over the Meaco device interface.
use crate::{
    Result,
    config::Memory,
    error,
    meaco::{Action, Device, State},
};
use serde::{Deserialize, Serialize};
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
