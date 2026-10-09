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
/// Typed, optional presets. Missing presets never fall back to household choices.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Preset {
    pub mode: String,
    #[serde(default)]
    pub target_humidity: Option<u8>,
    #[serde(default)]
    pub sleep: bool,
}
impl Preset {
    pub fn validate(&self) -> Result<()> {
        if !matches!(self.mode.as_str(), "manual" | "purify")
            || self.target_humidity.is_some_and(|h| {
                !crate::meaco::HUMIDITY_SETTING.accepts(&serde_json::Value::from(h))
            })
        {
            return Err(error(
                "Invalid Meaco preset mode or humidity (35–70 in steps of 5)",
            ));
        }
        Ok(())
    }
}
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Presets {
    pub day: Option<Preset>,
    pub night: Option<Preset>,
}
impl Presets {
    pub fn validate(&self) -> Result<()> {
        for p in [&self.day, &self.night].into_iter().flatten() {
            p.validate()?;
        }
        Ok(())
    }
    pub fn configured(&self, operation: Operation) -> bool {
        match operation {
            Operation::Off => true,
            Operation::Day => self.day.is_some(),
            Operation::Night => self.night.is_some(),
        }
    }
    fn get(&self, operation: Operation) -> Result<Option<&Preset>> {
        let p = match operation {
            Operation::Off => return Ok(None),
            Operation::Day => &self.day,
            Operation::Night => &self.night,
        };
        let p = p
            .as_ref()
            .ok_or_else(|| error("Configure the requested Meaco preset first"))?;
        p.validate()?;
        Ok(Some(p))
    }
}
fn already_correct(state: &State, preset: Option<&Preset>, memory: &Memory) -> bool {
    let Some(p) = preset else {
        return !state.power;
    };
    state.power
        && p.target_humidity.is_none_or(|h| state.target_humidity == h)
        && if p.sleep {
            state.mode == "sleep" && memory.base_mode.as_deref() == Some(p.mode.as_str())
        } else {
            state.mode == p.mode
        }
}

pub fn plan(
    state: &State,
    operation: Operation,
    memory: &Memory,
    presets: &Presets,
) -> Result<Vec<Action>> {
    let preset = presets.get(operation)?;
    if already_correct(state, preset, memory) {
        return Ok(vec![]);
    }
    let Some(p) = preset else {
        return Ok(vec![Action::Power(false)]);
    };
    let mut steps = vec![];
    if !state.power {
        steps.push(Action::Power(true));
    }
    if state.mode == "sleep" {
        steps.push(Action::Mode("sleep".into()));
    }
    if let Some(h) = p.target_humidity.filter(|h| state.target_humidity != *h) {
        steps.push(Action::Humidity(h));
    }
    // Clearing sleep reveals an unknown underlying mode; the plan is provisional.
    if state.mode != p.mode {
        steps.push(Action::Mode(p.mode.clone()));
    }
    if p.sleep {
        steps.push(Action::Mode("sleep".into()));
    }
    Ok(steps)
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
    presets: &Presets,
    mut persist: impl FnMut(&Memory) -> Result<()>,
) -> Result<Applied> {
    let preset = presets.get(operation)?;
    let mut state = device.read()?;
    if already_correct(&state, preset, memory) {
        return Ok(Applied {
            operation,
            changed: false,
            state,
        });
    }
    // Invalidate before I/O; an ambiguous result must never restore a sleep base.
    memory.base_mode = None;
    persist(memory)?;
    let Some(p) = preset else {
        state = send_verified(device, &Action::Power(false))?;
        if state.power {
            return Err(error("Meaco did not remain off"));
        }
        return Ok(Applied {
            operation,
            changed: true,
            state,
        });
    };
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
    if let Some(h) = p.target_humidity.filter(|h| state.target_humidity != *h) {
        state = send_verified(device, &Action::Humidity(h))?;
        powered(&state)?;
        if state.target_humidity != h {
            return Err(error("Humidity target did not persist"));
        }
    }
    if state.mode != p.mode {
        state = send_verified(device, &Action::Mode(p.mode.clone()))?;
    }
    powered(&state)?;
    if state.mode != p.mode
        || p.target_humidity
            .is_some_and(|h| state.target_humidity != h)
    {
        return Err(error("Underlying mode or humidity target did not persist"));
    }
    if p.sleep {
        state = send_verified(device, &Action::Mode("sleep".into()))?;
        powered(&state)?;
        if state.mode != "sleep" {
            return Err(error(
                "Night mode did not persist; refusing to toggle again",
            ));
        }
    }
    memory.base_mode = Some(p.mode.clone());
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
