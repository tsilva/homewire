//! Explicit, bounded shutdown lists. No discovery-driven expansion or power-on rollback.
use crate::{
    Result,
    config::{Source, save_private},
    device::{Devices, Family},
    entity::{Device, Domain},
    error,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::PathBuf;

#[derive(Clone, Deserialize, Serialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
enum Step {
    Routine { name: String },
    Outlet { entity: String },
    Printer { entity: String, outlet: String },
    Light { entity: String },
    Group { group: String },
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Settings {
    steps: Vec<Step>,
    #[serde(default)]
    optional_pc: Vec<Step>,
    #[serde(default)]
    optional_kitchen: Vec<Step>,
}
#[derive(Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Memory {
    attempt: Option<i64>,
}
pub struct Manager {
    settings: Settings,
    path: PathBuf,
    memory: Memory,
    _lock: crate::config::FileLock,
}
impl Manager {
    pub(crate) fn open(source: &Source<'_>, name: &str) -> Result<Self> {
        let settings: Settings = source.decode()?;
        validate_settings(&settings, name)?;
        let lock = crate::config::lock_configuration(source.path())?;
        let path = source.path().with_extension("state.json");
        let memory = if path.exists() {
            Source::file(&path).decode()?
        } else {
            Memory::default()
        };
        Ok(Self {
            settings,
            path,
            memory,
            _lock: lock,
        })
    }
    fn steps(&self, pc: bool, kitchen: bool) -> impl Iterator<Item = &Step> {
        self.settings
            .steps
            .iter()
            .chain(self.settings.optional_pc.iter().filter(move |_| pc))
            .chain(
                self.settings
                    .optional_kitchen
                    .iter()
                    .filter(move |_| kitchen),
            )
    }
    pub(crate) fn validate(&self, devices: &Devices) -> Result<()> {
        let mut targets = Vec::new();
        let mut routines = Vec::new();
        for step in self.steps(true, true) {
            match step {
                Step::Routine { name } => {
                    if routines.contains(name)
                        || match name.as_str() {
                            "tv_off" => devices.tcl_routine.is_none(),
                            "computer_off" => devices.computer_routine.is_none(),
                            _ => true,
                        }
                    {
                        return Err(error(
                            "Shutdown list requires unique configured verified TV/PC routines",
                        ));
                    }
                    if name == "tv_off" {
                        devices
                            .tcl_routine
                            .as_ref()
                            .ok_or_else(|| error("Missing TV routine"))?
                            .validate(devices)?;
                    } else {
                        devices
                            .computer_routine
                            .as_ref()
                            .ok_or_else(|| error("Missing computer routine"))?
                            .validate(devices)?;
                    }
                    routines.push(name.clone());
                }
                Step::Outlet { entity } => add_target(&mut targets, outlet(devices, entity)?)?,
                Step::Light { entity } => {
                    add_target(&mut targets, crate::group::light_target(devices, entity)?)?
                }
                Step::Group { group } => {
                    for (_, device) in devices.groups.targets(devices, group)? {
                        add_target(&mut targets, device)?;
                    }
                }
                Step::Printer { entity, outlet: id } => {
                    let printer = crate::entity::resolve(devices, entity)?;
                    if printer.domain != Domain::Printer
                        || printer.device.family != Family::Printers
                        || printer.field.is_some()
                    {
                        return Err(error(
                            "Printer shutdown requires a read-only IPP printer entity",
                        ));
                    }
                    add_target(&mut targets, printer.device)?;
                    add_target(&mut targets, outlet(devices, id)?)?;
                }
            }
        }
        Ok(())
    }
    fn check_options(&self, pc: bool, kitchen: bool) -> Result<()> {
        if pc && self.settings.optional_pc.is_empty()
            || kitchen && self.settings.optional_kitchen.is_empty()
        {
            return Err(error("Requested optional shutdown group is not configured"));
        }
        Ok(())
    }
    pub(crate) fn preview(&self, pc: bool, kitchen: bool) -> Result<Value> {
        self.check_options(pc, kitchen)?;
        Ok(
            json!({"live_state_checked":false,"provisional":true,"execution":"concurrent_best_effort",
            "steps":self.steps(pc,kitchen).collect::<Vec<_>>(),
            "optional_groups_selected":{"pc":pc,"kitchen":kitchen},
            "printer_policy":"fresh_idle_and_zero_jobs_before_cutoff; no graceful shutdown command",
            "failure_policy":"leave dependent outlet on; continue independent steps; never replay automatically"}),
        )
    }
    pub(crate) fn run(&mut self, devices: &mut Devices, pc: bool, kitchen: bool) -> Result<Value> {
        self.check_options(pc, kitchen)?;
        self.validate(devices)?;
        begin(&mut self.memory, &self.path, chrono::Utc::now().timestamp())?;
        let equipment = super::parallel::Equipment::new(devices);
        let result = sequence(self.steps(pc, kitchen), |step| {
            run_step(step, &mut Equipment(&equipment))
        });
        // Retain the durable guard on partial completion, including a lost response.
        if result["completed"] == true {
            self.memory.attempt = None;
            save_private(&self.path, &self.memory)?;
        }
        Ok(result)
    }
}
fn validate_settings(settings: &Settings, name: &str) -> Result<()> {
    let all = settings
        .steps
        .iter()
        .chain(&settings.optional_pc)
        .chain(&settings.optional_kitchen);
    if !matches!(name, "leave_house" | "sleep")
        || settings.steps.is_empty()
        || all.clone().count() > 16
        || name == "sleep"
            && (!settings.optional_pc.is_empty() || !settings.optional_kitchen.is_empty())
        || all.clone().any(|step| match step {
            Step::Routine { name } => !matches!(name.as_str(), "tv_off" | "computer_off"),
            Step::Outlet { entity } => entity.is_empty() || entity.len() > 96,
            Step::Light { entity } => entity.is_empty() || entity.len() > 96,
            Step::Group { group } => group.is_empty() || group.len() > 96,
            Step::Printer { entity, outlet } => {
                entity.is_empty() || entity.len() > 96 || outlet.is_empty() || outlet.len() > 96
            }
        })
        || name == "leave_house"
            && (settings
                .steps
                .iter()
                .chain(&settings.optional_kitchen)
                .any(|s| matches!(s,Step::Routine{name} if name=="computer_off"))
                || settings
                    .optional_pc
                    .iter()
                    .any(|s| !matches!(s,Step::Routine{name} if name=="computer_off")))
    {
        return Err(error(
            "Invalid explicit shutdown list: 1–16 off-only steps; leave_house PC must be optional_pc; sleep has no optional lists",
        ));
    }
    Ok(())
}
fn add_target(targets: &mut Vec<Device>, device: Device) -> Result<()> {
    if targets.contains(&device) {
        return Err(error("Duplicate physical device in shutdown list"));
    }
    targets.push(device);
    Ok(())
}
fn outlet(devices: &Devices, id: &str) -> Result<Device> {
    let entity = crate::entity::resolve(devices, id)?;
    if entity.domain != Domain::Switch
        || entity.device.family != Family::Plugs
        || entity.field.is_some()
        || devices.tv_owns_power(entity.device)
        || devices.computer_owns_power(entity.device)
    {
        return Err(error(
            "Shutdown outlet must be an unowned plug switch; use TV/PC routines for owned power",
        ));
    }
    Ok(entity.device)
}
fn begin(memory: &mut Memory, path: &std::path::Path, now: i64) -> Result<()> {
    if memory
        .attempt
        .is_some_and(|last| now < last.saturating_add(180))
    {
        return Err(error(
            "Recent household shutdown attempt; replay blocked for 180 seconds, including after restart",
        ));
    }
    memory.attempt = Some(now);
    save_private(path, memory)
}
trait Control {
    fn routine(&mut self, name: &str) -> Result<Value>;
    fn outlet(&mut self, entity: &str) -> Result<bool>;
    fn cutoff(&mut self, entity: &str) -> Result<()>;
    fn printer(&mut self, entity: &str) -> Result<Value>;
    fn light(&mut self, _entity: &str) -> Result<Value> {
        Err(error("Light power unavailable"))
    }
    fn group(&mut self, _group: &str) -> Result<Value> {
        Err(error("Grouped power unavailable"))
    }
}
struct Equipment<'a, 'b>(&'a super::parallel::Equipment<'b>);
impl Equipment<'_, '_> {
    fn plug_name(&self, id: &str) -> Result<String> {
        let device = outlet(self.0.devices(), id)?;
        Ok(self
            .0
            .devices()
            .plugs_ref()
            .ok_or_else(|| error("Missing plugs"))?
            .config
            .plugs[device.index]
            .name
            .clone())
    }
}
impl Control for Equipment<'_, '_> {
    fn light(&mut self, entity: &str) -> Result<Value> {
        let target = crate::group::light_target(self.0.devices(), entity)?;
        let result = self.0.devices().power_device(target, false)?;
        Ok(json!({"verified":true,"power":false,"result":result}))
    }
    fn group(&mut self, group: &str) -> Result<Value> {
        crate::group::apply(self.0.devices(), group, false)
    }
    fn routine(&mut self, name: &str) -> Result<Value> {
        self.0.routine(name)
    }
    fn outlet(&mut self, entity: &str) -> Result<bool> {
        let name = self.plug_name(entity)?;
        Ok(self
            .0
            .devices()
            .plugs_ref()
            .ok_or_else(|| error("Missing plugs"))?
            .relay_status(&name)?
            .power)
    }
    fn cutoff(&mut self, entity: &str) -> Result<()> {
        let name = self.plug_name(entity)?;
        self.0
            .devices()
            .plugs_ref()
            .ok_or_else(|| error("Missing plugs"))?
            .apply(&name, false)?;
        Ok(())
    }
    fn printer(&mut self, entity: &str) -> Result<Value> {
        let devices = self.0.devices();
        let device = crate::entity::resolve(devices, entity)?.device;
        let printers = devices
            .printers
            .as_ref()
            .ok_or_else(|| error("Missing printers"))?;
        let endpoint = printers
            .config
            .printers
            .get(device.index)
            .ok_or_else(|| error("Unknown printer"))?;
        Ok(serde_json::to_value(printers.status(&endpoint.name)?)?)
    }
}
fn run_step(step: &Step, control: &mut impl Control) -> Result<Value> {
    match step {
        Step::Light { entity } => control.light(entity),
        Step::Group { group } => control.group(group),
        Step::Routine { name } => {
            let reply = control.routine(name)?;
            let confirmed = if name == "computer_off" {
                reply["result"]["completed"] == true && reply["result"]["outlet_power"] == false
            } else {
                reply["result"]["verified"] == true
            };
            if !confirmed {
                return Err(error(
                    "Device shutdown was not verified; its outlet remains governed by the child routine",
                ));
            }
            Ok(reply)
        }
        Step::Outlet { entity } => {
            if control.outlet(entity)? {
                control.cutoff(entity)?;
            }
            Ok(json!({"verified":true,"power":false}))
        }
        Step::Printer { entity, outlet } => {
            if !control.outlet(outlet)? {
                return Ok(json!({"verified":true,"power":false,"already_off":true}));
            }
            let status = control.printer(entity)?;
            if status["state"] != "idle" || status["queued_jobs"].as_u64() != Some(0) {
                return Err(error(
                    "Fresh printer check did not confirm idle and zero queued jobs; outlet left on",
                ));
            }
            control.cutoff(outlet)?;
            Ok(
                json!({"verified":true,"power":false,"idle_and_zero_jobs":true,"graceful_shutdown":false}),
            )
        }
    }
}
fn sequence<'a>(
    steps: impl Iterator<Item = &'a Step>,
    control: impl Fn(&Step) -> Result<Value> + Sync,
) -> Value {
    let steps = steps.collect::<Vec<_>>();
    let results = super::parallel::run(&steps, &|step| control(step));
    super::parallel::report(steps.into_iter(), results)
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fake {
        events: Vec<String>,
        status: Value,
        powered: bool,
        tv_fails: bool,
        cutoff_fails: bool,
    }
    impl Default for Fake {
        fn default() -> Self {
            Self {
                events: vec![],
                status: json!({"state":"idle","queued_jobs":0}),
                powered: true,
                tv_fails: false,
                cutoff_fails: false,
            }
        }
    }
    impl Control for Fake {
        fn group(&mut self, group: &str) -> Result<Value> {
            self.events.push(format!("group:{group}"));
            Ok(
                json!({"completed":false,"verified":false,"results":[{"verified":false,"error":"synthetic uncertain outcome"}]}),
            )
        }
        fn routine(&mut self, name: &str) -> Result<Value> {
            self.events.push(name.into());
            if self.tv_fails {
                return Err(error("Unconfirmed TV; child left outlet on"));
            }
            Ok(json!({"result":{"verified":true,"completed":true,"outlet_power":false}}))
        }
        fn outlet(&mut self, entity: &str) -> Result<bool> {
            self.events.push(format!("read:{entity}"));
            Ok(self.powered)
        }
        fn cutoff(&mut self, entity: &str) -> Result<()> {
            self.events.push(format!("cut:{entity}"));
            if self.cutoff_fails {
                Err(error("Uncertain relay response"))
            } else {
                Ok(())
            }
        }
        fn printer(&mut self, entity: &str) -> Result<Value> {
            self.events.push(format!("ipp:{entity}"));
            Ok(self.status.clone())
        }
    }
    fn fake_sequence<'a>(steps: impl Iterator<Item = &'a Step>, fake: &mut Fake) -> Value {
        let control = std::sync::Mutex::new(fake);
        sequence(steps, |step| {
            run_step(step, &mut **control.lock().expect("control"))
        })
    }
    fn steps() -> Vec<Step> {
        vec![
            Step::Routine {
                name: "tv_off".into(),
            },
            Step::Printer {
                entity: "printer.office".into(),
                outlet: "switch.printer".into(),
            },
            Step::Outlet {
                entity: "switch.heater".into(),
            },
        ]
    }
    #[test]
    fn partial_group_retains_member_results_and_does_not_skip_independent_steps() {
        let steps = [
            Step::Group {
                group: "group.sample_lights".into(),
            },
            Step::Outlet {
                entity: "switch.sample".into(),
            },
        ];
        let mut fake = Fake::default();
        let result = fake_sequence(steps.iter(), &mut fake);
        assert_eq!(result["completed"], false);
        assert_eq!(
            result["steps"][0]["result"]["results"][0]["verified"],
            false
        );
        let read = fake
            .events
            .iter()
            .position(|e| e == "read:switch.sample")
            .expect("read");
        let cut = fake
            .events
            .iter()
            .position(|e| e == "cut:switch.sample")
            .expect("cutoff");
        assert!(read < cut);
        fake.events.sort();
        assert_eq!(
            fake.events,
            vec![
                "cut:switch.sample",
                "group:group.sample_lights",
                "read:switch.sample"
            ]
        );
    }
    #[test]
    fn fresh_printer_check_precedes_cutoff_and_independent_steps_continue() {
        let mut fake = Fake {
            tv_fails: true,
            ..Fake::default()
        };
        let result = fake_sequence(steps().iter(), &mut fake);
        assert_eq!(result["completed"], false);
        assert_eq!(result["steps"][1]["completed"], true);
        let position = |event| fake.events.iter().position(|e| e == event).expect("event");
        assert!(position("read:switch.printer") < position("ipp:printer.office"));
        assert!(position("ipp:printer.office") < position("cut:switch.printer"));
        assert!(position("read:switch.heater") < position("cut:switch.heater"));
        assert!(fake.events.contains(&"tv_off".into()));
        assert_eq!(fake.events.len(), 6);
    }
    #[test]
    fn independent_shutdown_steps_overlap_and_replies_keep_configuration_order() {
        let steps = steps();
        let (release, receive) = std::sync::mpsc::sync_channel(1);
        let receive = std::sync::Mutex::new(receive);
        let result = sequence(steps.iter(), |step| match step {
            Step::Routine { .. } => {
                receive
                    .lock()
                    .expect("receiver")
                    .recv_timeout(std::time::Duration::from_secs(5))?;
                Err(error("Synthetic unconfirmed shutdown"))
            }
            Step::Outlet { .. } => {
                release.send(())?;
                Ok(json!({"verified":true}))
            }
            _ => Ok(json!({"verified":true})),
        });
        assert_eq!(result["completed"], false);
        assert_eq!(result["steps"][0]["completed"], false);
        assert_eq!(result["steps"][1]["completed"], true);
        assert_eq!(result["steps"][2]["completed"], true);
        assert_eq!(result["steps"][0]["step"]["name"], "tv_off");
    }
    #[test]
    fn busy_stopped_unknown_missing_or_nonzero_jobs_never_cut_printer_power() {
        for status in [
            json!({"state":"printing","queued_jobs":0}),
            json!({"state":"stopped","queued_jobs":0}),
            json!({"state":"unknown","queued_jobs":0}),
            json!({"state":"idle"}),
            json!({"state":"idle","queued_jobs":1}),
            json!({"queued_jobs":0}),
        ] {
            let mut fake = Fake {
                status,
                ..Fake::default()
            };
            let result = fake_sequence(steps().iter(), &mut fake);
            assert_eq!(result["completed"], false);
            assert!(!fake.events.contains(&"cut:switch.printer".into()));
            assert!(fake.events.contains(&"cut:switch.heater".into()));
        }
    }
    #[test]
    fn already_off_printer_needs_no_unreachable_ipp_and_uncertain_cutoff_is_not_retried() {
        let mut fake = Fake {
            powered: false,
            ..Fake::default()
        };
        assert_eq!(fake_sequence(steps().iter(), &mut fake)["completed"], true);
        assert!(
            !fake
                .events
                .iter()
                .any(|s| s.starts_with("ipp:") || s.starts_with("cut:"))
        );
        let mut fake = Fake {
            cutoff_fails: true,
            ..Fake::default()
        };
        assert_eq!(fake_sequence(steps().iter(), &mut fake)["completed"], false);
        assert_eq!(
            fake.events
                .iter()
                .filter(|s| s.as_str() == "cut:switch.printer")
                .count(),
            1
        );
    }
    #[test]
    fn off_only_no_recursion_and_optional_pc_cannot_become_mandatory() -> Result<()> {
        for name in ["sleep", "leave_house", "computer_on", "tv_on", "meaco_off"] {
            let settings: Settings =
                serde_json::from_value(json!({"steps":[{"action":"routine","name":name}]}))?;
            assert!(validate_settings(&settings, "sleep").is_err());
        }
        let settings: Settings =
            serde_json::from_value(json!({"steps":[{"action":"routine","name":"computer_off"}]}))?;
        assert!(validate_settings(&settings, "leave_house").is_err());
        assert!(validate_settings(&settings, "sleep").is_ok());
        assert!(
            serde_json::from_value::<Settings>(
                json!({"steps":[{"action":"outlet","entity":"switch.a","power":true}]})
            )
            .is_err()
        );
        Ok(())
    }
    #[test]
    fn persisted_attempt_blocks_replay_after_restart_and_clock_rollback() -> Result<()> {
        let mut nonce = [0; 8];
        getrandom::getrandom(&mut nonce)?;
        let path =
            std::env::temp_dir().join(format!("hw-household-{:x}.json", u64::from_ne_bytes(nonce)));
        let mut memory = Memory::default();
        begin(&mut memory, &path, 1000)?;
        let mut reloaded: Memory = Source::file(&path).decode()?;
        assert!(begin(&mut reloaded, &path, 1001).is_err());
        assert!(begin(&mut reloaded, &path, 900).is_err());
        std::fs::remove_file(path)?;
        Ok(())
    }
}
