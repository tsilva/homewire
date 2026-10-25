//! Explicit startup lists with identity-resolved exclusions and guarded child routines.
use crate::{
    Result,
    config::{Source, save_private},
    device::Devices,
    entity::{Capabilities, Device, Domain},
    error,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::PathBuf;

#[derive(Deserialize, Serialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
enum Step {
    Routine { name: String },
    PowerOn { entity: String },
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Settings {
    steps: Vec<Step>,
    excluded_entities: Vec<String>,
}
#[derive(Default, Deserialize, Serialize)]
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
        if settings.steps.is_empty()
            || settings.steps.len() > 16
            || settings.excluded_entities.is_empty()
            || settings.excluded_entities.len() > 16
            || settings.excluded_entities.iter().any(|id| !valid_id(id))
            || settings.steps.iter().any(|step| match step {
                Step::Routine { name } => !matches!(name.as_str(), "tv_on" | "computer_on"),
                Step::PowerOn { entity } => !valid_id(entity),
            })
        {
            return Err(error(
                "wake_up requires 1–16 explicit on-only steps and 1–16 excluded entities",
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
    pub(crate) fn validate(&self, devices: &Devices) -> Result<()> {
        let mut excluded = Vec::new();
        for id in &self.settings.excluded_entities {
            let entity = crate::entity::resolve(devices, id)?;
            if entity.field.is_some() {
                return Err(error("Exclude a whole physical device, not a sensor field"));
            }
            add_target(&mut excluded, entity.device)?;
        }
        let mut targets = Vec::new();
        for step in &self.settings.steps {
            match step {
                Step::PowerOn { entity } => {
                    let (device, _) = power_target(devices, entity)?;
                    include(&mut targets, &excluded, device)?;
                }
                Step::Routine { name } => {
                    if match name.as_str() {
                        "tv_on" => devices.tcl_routine.is_none(),
                        "computer_on" => devices.computer_routine.is_none(),
                        _ => true,
                    } {
                        return Err(error(
                            "wake_up child routine is not configured or unsupported",
                        ));
                    }
                    let mut owned = Vec::new();
                    devices.entities(&mut |entity| {
                        let owns = if name == "tv_on" {
                            devices.tv_owns_power(entity.device)
                        } else {
                            devices.computer_owns_power(entity.device)
                        };
                        if owns && !owned.contains(&entity.device) {
                            owned.push(entity.device);
                        }
                    });
                    if owned.is_empty() {
                        return Err(error("Startup child routine has no resolved devices"));
                    }
                    for device in owned {
                        include(&mut targets, &excluded, device)?;
                    }
                }
            }
        }
        Ok(())
    }
    pub(crate) fn preview(&self) -> Value {
        json!({"steps":self.settings.steps,"excluded_entities":self.settings.excluded_entities,
            "live_state_checked":false,"provisional":true,
            "power_only":true,"preserve_other_settings":true,
            "ordering":"child TV/PC routines power their outlets before starting devices",
            "outlet_verification":"confirms relay power, not attached appliance readiness",
            "failure_policy":"continue independent steps; never replay automatically"})
    }
    pub(crate) fn run(&mut self, devices: &mut Devices) -> Result<Value> {
        self.validate(devices)?;
        begin(&mut self.memory, &self.path, chrono::Utc::now().timestamp())?;
        let result = sequence(&self.settings.steps, &mut Equipment(devices));
        if result["completed"] == true {
            clear(&mut self.memory, &self.path)?;
        }
        Ok(result)
    }
}
fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 96
}
fn add_target(targets: &mut Vec<Device>, device: Device) -> Result<()> {
    if targets.contains(&device) {
        return Err(error("Duplicate physical device in wake_up configuration"));
    }
    targets.push(device);
    Ok(())
}
fn include(targets: &mut Vec<Device>, excluded: &[Device], device: Device) -> Result<()> {
    if excluded.contains(&device) {
        return Err(error(
            "wake_up step conflicts with an excluded physical device",
        ));
    }
    add_target(targets, device)
}
fn power_target(devices: &Devices, id: &str) -> Result<(Device, &'static str)> {
    let entity = crate::entity::resolve(devices, id)?;
    if entity.field.is_some()
        || devices.tv_owns_power(entity.device)
        || devices.computer_owns_power(entity.device)
    {
        return Err(error(
            "Use the TV/PC child routine for owned power; sensor fields cannot be powered on",
        ));
    }
    let tool = match (entity.domain, entity.capabilities) {
        (Domain::Switch, Capabilities::Power) => "switch_set",
        (Domain::Light, Capabilities::Light { .. }) => "light_set",
        (Domain::Dehumidifier, Capabilities::Dehumidifier)
            if entity
                .settings
                .iter()
                .any(|s| s.name == "power" && s.accepts(&json!(true))) =>
        {
            "entity_configure"
        }
        _ => {
            return Err(error(
                "wake_up target has no supported absolute power-on capability",
            ));
        }
    };
    Ok((entity.device, tool))
}
fn begin(memory: &mut Memory, path: &std::path::Path, now: i64) -> Result<()> {
    if memory
        .attempt
        .is_some_and(|last| now < last.saturating_add(180))
    {
        return Err(error(
            "Recent wake_up attempt; replay blocked for 180 seconds, including after restart",
        ));
    }
    memory.attempt = Some(now);
    save_private(path, memory)
}
fn clear(memory: &mut Memory, path: &std::path::Path) -> Result<()> {
    let cleared = Memory::default();
    save_private(path, &cleared)?;
    *memory = cleared;
    Ok(())
}
trait Control {
    fn routine(&mut self, name: &str) -> Result<Value>;
    fn power_on(&mut self, id: &str) -> Result<Value>;
}
struct Equipment<'a>(&'a mut Devices);
impl Control for Equipment<'_> {
    fn routine(&mut self, name: &str) -> Result<Value> {
        super::call(self.0, "routine_run", &json!({"routine":name}))
    }
    fn power_on(&mut self, id: &str) -> Result<Value> {
        let (_, tool) = power_target(self.0, id)?;
        let args = if tool == "switch_set" {
            json!({"entity":id,"power":true})
        } else {
            json!({"entity":id,"settings":{"power":true}})
        };
        crate::entity::call(self.0, tool, &args)
    }
}
fn step_run(step: &Step, control: &mut impl Control) -> Result<Value> {
    let reply = match step {
        Step::Routine { name } => control.routine(name)?,
        Step::PowerOn { entity } => control.power_on(entity)?,
    };
    let result = &reply["result"];
    let verified = match step {
        Step::Routine { .. } => result["completed"] == true && result["verified"] == true,
        Step::PowerOn { .. } => {
            result["status"]["power"] == true
                || result["verified"] == true && result["state"]["power"] == true
        }
    };
    if !verified {
        return Err(error("Startup step did not confirm power-on; no replay"));
    }
    Ok(reply)
}
fn sequence(steps: &[Step], control: &mut impl Control) -> Value {
    let mut completed = true;
    let results = steps.iter().map(|step|match step_run(step, control) {
        Ok(result) => json!({"step":step,"completed":true,"result":result}),
        Err(e) => { completed=false; json!({"step":step,"completed":false,"error":e.to_string().chars().take(512).collect::<String>()}) }
    }).collect::<Vec<_>>();
    json!({"completed":completed,"verified":completed,"steps":results})
}

#[cfg(test)]
mod tests {
    use super::*;
    #[derive(Default)]
    struct Fake {
        calls: Vec<String>,
        failed: bool,
        unknown: bool,
    }
    impl Control for Fake {
        fn routine(&mut self, name: &str) -> Result<Value> {
            self.calls.push(name.into());
            if self.failed {
                Err(error("Uncertain child result"))
            } else {
                Ok(json!({"result":{"completed":true,"verified":true}}))
            }
        }
        fn power_on(&mut self, id: &str) -> Result<Value> {
            self.calls.push(id.into());
            Ok(if self.unknown {
                json!({"result":{"applied":true}})
            } else {
                json!({"result":{"status":{"power":true}}})
            })
        }
    }
    #[test]
    fn failed_child_is_not_replayed_and_independent_steps_continue() {
        let mut fake = Fake {
            failed: true,
            ..Fake::default()
        };
        let steps = vec![
            Step::Routine {
                name: "tv_on".into(),
            },
            Step::PowerOn {
                entity: "switch.sample".into(),
            },
        ];
        let reply = sequence(&steps, &mut fake);
        assert_eq!(reply["completed"], false);
        assert_eq!(reply["steps"][1]["completed"], true);
        assert_eq!(fake.calls, vec!["tv_on", "switch.sample"]);
    }
    #[test]
    fn request_acceptance_without_power_confirmation_is_incomplete() {
        let mut fake = Fake {
            unknown: true,
            ..Fake::default()
        };
        assert!(
            step_run(
                &Step::PowerOn {
                    entity: "light.sample".into()
                },
                &mut fake
            )
            .is_err()
        );
        assert_eq!(fake.calls.len(), 1);
    }
    #[test]
    fn attempt_is_saved_before_controls_and_blocks_after_restart() -> Result<()> {
        let mut nonce = [0; 8];
        getrandom::getrandom(&mut nonce)?;
        let path =
            std::env::temp_dir().join(format!("hw-startup-{:x}.json", u64::from_ne_bytes(nonce)));
        let mut memory = Memory::default();
        begin(&mut memory, &path, 1000)?;
        let mut restored: Memory = Source::file(&path).decode()?;
        assert!(begin(&mut restored, &path, 1001).is_err());
        assert!(begin(&mut restored, &path, 900).is_err());
        let missing = path.join("missing/state.json");
        assert!(clear(&mut restored, &missing).is_err());
        assert_eq!(restored.attempt, Some(1000));
        assert!(begin(&mut restored, &path, 1001).is_err());
        std::fs::remove_file(path)?;
        Ok(())
    }
    #[test]
    fn dehumidifier_power_requires_verified_state() -> Result<()> {
        struct Humidifier;
        impl Control for Humidifier {
            fn routine(&mut self, _: &str) -> Result<Value> {
                panic!("Unexpected child")
            }
            fn power_on(&mut self, _: &str) -> Result<Value> {
                Ok(json!({"result":{"verified":true,"state":{"power":true}}}))
            }
        }
        assert!(
            step_run(
                &Step::PowerOn {
                    entity: "dehumidifier.sample".into()
                },
                &mut Humidifier
            )
            .is_ok()
        );
        Ok(())
    }
}
