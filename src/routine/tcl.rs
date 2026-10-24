//! TCL power sequencing with a shared outlet and verified shutdown before cutoff.
use crate::{
    Result,
    config::{Source, save_private},
    device::{Devices, Family},
    entity::Device,
    error,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Settings {
    target: String,
    plug: String,
    startup_seconds: u64,
    ready_seconds: u64,
}
#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Memory {
    attempt: Option<i64>,
}
pub struct Manager {
    settings: Settings,
    memory: Memory,
    path: PathBuf,
    _lock: crate::config::FileLock,
}
impl Manager {
    pub(crate) fn open(source: &Source<'_>) -> Result<Self> {
        let settings: Settings = source.decode()?;
        if settings.target != "tcl"
            || !crate::config::valid_unique_labels(
                std::iter::once(settings.plug.as_str()),
                32,
                b"_-",
            )
            || !(1..=60).contains(&settings.startup_seconds)
            || !(1..=120).contains(&settings.ready_seconds)
        {
            return Err(error(
                "TCL routine requires target tcl, a named outlet, startup 1–60 seconds and readiness 1–120 seconds",
            ));
        }
        let lock = crate::config::lock_configuration(source.path())?;
        let path = source.path().with_extension("state.json");
        let memory = if path.exists() {
            Source::file(&path).decode()?
        } else {
            Memory::default()
        };
        Ok(Self {
            settings,
            memory,
            path,
            _lock: lock,
        })
    }
    pub(crate) fn owns_power(&self, devices: &Devices, device: Device) -> bool {
        device.family == Family::Tcl
            || device.family == Family::Plugs
                && devices
                    .plugs_ref()
                    .and_then(|p| p.config.plugs.get(device.index))
                    .is_some_and(|p| p.name == self.settings.plug)
    }
    pub(crate) fn owns_command(&self, name: &str, args: &Value) -> bool {
        name == "plug_apply" && args["plug"].as_str() == Some(&self.settings.plug)
    }
    pub(crate) fn validate(&self, devices: &Devices) -> Result<()> {
        if devices.tcl.is_none()
            || devices.tv.is_some()
            || !devices
                .plugs_ref()
                .is_some_and(|p| p.config.plugs.iter().any(|p| p.name == self.settings.plug))
        {
            return Err(error(
                "TCL routine requires TCL and its configured outlet, without a second TV routine",
            ));
        }
        if let Some(plugs) = devices.plugs_ref() {
            for (index, plug) in plugs.config.plugs.iter().enumerate() {
                if plug.name == self.settings.plug
                    && devices.computer_owns_power(Device {
                        family: Family::Plugs,
                        index,
                    })
                {
                    return Err(error("Computer and TV cannot own the same outlet"));
                }
            }
        }
        Ok(())
    }
    pub fn preview(&self, power: bool) -> Value {
        json!({"target":"tcl","power":power,"live_state_checked":false,"provisional":true,
            "startup_seconds":self.settings.startup_seconds,"ready_seconds":self.settings.ready_seconds,
            "steps":if power { vec!["read_outlet","outlet_on_if_needed","startup_wait_if_changed","wait_for_pinned_tcl_ready_if_outlet_changed","tcl_on_verify"] }
            else { vec!["read_outlet","if_outlet_on_tcl_off_verify","outlet_off_verify"] }})
    }
    pub(crate) fn run(&mut self, devices: &mut Devices, power: bool) -> Result<Value> {
        self.validate(devices)?;
        let now = chrono::Utc::now().timestamp();
        if self
            .memory
            .attempt
            .is_some_and(|last| now < last.saturating_add(180))
        {
            return Err(error(
                "Recent TV workflow attempt; replay blocked for 180 seconds, including after restart",
            ));
        }
        self.memory.attempt = Some(now);
        save_private(&self.path, &self.memory)?;
        sequence(
            &mut Equipment {
                devices,
                plug: &self.settings.plug,
            },
            power,
            self.settings.startup_seconds,
            self.settings.ready_seconds,
        )?;
        self.memory.attempt = None;
        save_private(&self.path, &self.memory)?;
        Ok(json!({"completed":true,"power":power,"target":"tcl","verified":true}))
    }
}
trait Control {
    fn outlet(&mut self) -> Result<bool>;
    fn set_outlet(&mut self, power: bool) -> Result<()>;
    fn ready(&mut self, seconds: u64) -> Result<()>;
    fn tv(&mut self, power: bool) -> Result<()>;
    fn wait(&mut self, seconds: u64);
}
struct Equipment<'a> {
    devices: &'a mut Devices,
    plug: &'a str,
}
impl Control for Equipment<'_> {
    fn outlet(&mut self) -> Result<bool> {
        Ok(self
            .devices
            .plugs()
            .ok_or_else(|| error("Missing TV outlet"))?
            .relay_status(self.plug)?
            .power)
    }
    fn set_outlet(&mut self, power: bool) -> Result<()> {
        self.devices
            .plugs()
            .ok_or_else(|| error("Missing TV outlet"))?
            .apply(self.plug, power)?;
        Ok(())
    }
    fn ready(&mut self, seconds: u64) -> Result<()> {
        let until = Instant::now() + Duration::from_secs(seconds);
        loop {
            // A failed pinned read sends no power command. No controls are replayed.
            if self
                .devices
                .tcl
                .as_ref()
                .ok_or_else(|| error("Missing TCL"))?
                .status()
                .is_ok()
            {
                return Ok(());
            }
            if Instant::now() >= until {
                return Err(error("TCL readiness not confirmed; outlet left on"));
            }
            self.wait(1);
        }
    }
    fn tv(&mut self, power: bool) -> Result<()> {
        require_verified(
            &self
                .devices
                .tcl
                .as_mut()
                .ok_or_else(|| error("Missing TCL"))?
                .apply(power)?,
        )
    }
    fn wait(&mut self, seconds: u64) {
        std::thread::sleep(Duration::from_secs(seconds));
    }
}
fn require_verified(result: &Value) -> Result<()> {
    if result["verified"] != true {
        return Err(error(
            "TCL power was not verified; outlet left on; do not replay the control",
        ));
    }
    Ok(())
}
fn sequence(control: &mut impl Control, power: bool, startup: u64, ready: u64) -> Result<()> {
    let outlet = control.outlet()?;
    if power {
        if !outlet {
            control.set_outlet(true)?;
            control.wait(startup);
            control.ready(ready)?;
        }
        control.tv(true)?;
    } else if outlet {
        control.tv(false)?;
        control.set_outlet(false)?;
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    struct Fake {
        outlet: bool,
        calls: Vec<String>,
        fail: Option<&'static str>,
    }
    impl Fake {
        fn step(&mut self, name: &str) -> Result<()> {
            self.calls.push(name.into());
            if self.fail == Some(name) {
                return Err(error("injected failure"));
            }
            Ok(())
        }
    }
    impl Control for Fake {
        fn outlet(&mut self) -> Result<bool> {
            self.step("outlet_status")?;
            Ok(self.outlet)
        }
        fn set_outlet(&mut self, power: bool) -> Result<()> {
            self.step(if power { "outlet_on" } else { "outlet_off" })
        }
        fn ready(&mut self, _: u64) -> Result<()> {
            self.step("ready")
        }
        fn tv(&mut self, power: bool) -> Result<()> {
            self.step(if power { "tv_on" } else { "tv_off" })
        }
        fn wait(&mut self, _: u64) {
            self.calls.push("wait".into());
        }
    }
    #[test]
    fn ordering_and_failures() {
        for (power, outlet, expected) in [
            (
                true,
                false,
                vec!["outlet_status", "outlet_on", "wait", "ready", "tv_on"],
            ),
            (true, true, vec!["outlet_status", "tv_on"]),
            (false, true, vec!["outlet_status", "tv_off", "outlet_off"]),
            (false, false, vec!["outlet_status"]),
        ] {
            let mut fake = Fake {
                outlet,
                calls: vec![],
                fail: None,
            };
            sequence(&mut fake, power, 5, 120).expect("successful sequence");
            assert_eq!(fake.calls, expected);
            for (index, step) in expected
                .iter()
                .enumerate()
                .filter(|(_, step)| **step != "wait")
            {
                let mut fake = Fake {
                    outlet,
                    calls: vec![],
                    fail: Some(step),
                };
                assert!(sequence(&mut fake, power, 5, 120).is_err());
                assert_eq!(fake.calls, expected[..=index]);
            }
        }
    }
    #[test]
    fn uncertain_success_never_allows_cutoff() {
        for result in [
            json!({"applied":true}),
            json!({"verified":false,"outcome":"unknown"}),
            json!({"verified":null}),
        ] {
            assert!(require_verified(&result).is_err());
        }
        assert!(require_verified(&json!({"verified":true})).is_ok());
    }
}
