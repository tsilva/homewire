//! Outlet-first startup; signed shutdown followed by corroborated standby cutoff.
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
    plug: String,
    startup_seconds: u64,
    ready_seconds: u64,
    shutdown_policy: String,
    #[serde(default)]
    shutdown_verification: Option<Verification>,
}
#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Verification {
    standby_power_w: f64,
    stable_seconds: u64,
    timeout_seconds: u64,
    poll_seconds: u64,
}
impl Verification {
    fn validate(self) -> Result<()> {
        if !self.standby_power_w.is_finite()
            || !(0.05..=5.0).contains(&self.standby_power_w)
            || !(10..=60).contains(&self.stable_seconds)
            || !(30..=300).contains(&self.timeout_seconds)
            || self.timeout_seconds < self.stable_seconds + 20
            || !(1..=5).contains(&self.poll_seconds)
        {
            return Err(error(
                "Standby cutoff requires a calibrated 0.05–5 W threshold, stable 10–60 seconds, timeout 30–300 seconds at least 20 seconds longer, and polling 1–5 seconds",
            ));
        }
        Ok(())
    }
}
#[derive(Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Memory {
    attempt: Option<i64>,
    #[serde(default)]
    shutdown_accepted: Option<i64>,
    #[serde(default)]
    shutdown_identity: Option<String>,
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
        if !crate::config::valid_unique_labels(std::iter::once(settings.plug.as_str()), 32, b"_-")
            || !(1..=60).contains(&settings.startup_seconds)
            || !(1..=120).contains(&settings.ready_seconds)
        {
            return Err(error(
                "Computer requires a named outlet and bounded startup/readiness times",
            ));
        }
        match (
            settings.shutdown_policy.as_str(),
            settings.shutdown_verification,
        ) {
            ("leave_on", None) => (),
            ("metered_standby", Some(verification)) => verification.validate()?,
            _ => {
                return Err(error(
                    "Use leave_on without verification or metered_standby with explicit verification settings",
                ));
            }
        }
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
            _lock: crate::config::lock_configuration(source.path())?,
        })
    }
    pub(crate) fn owns_power(&self, devices: &Devices, device: Device) -> bool {
        device.family == Family::Computer
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
        let outlet = devices
            .plugs_ref()
            .and_then(|p| p.config.plugs.iter().find(|p| p.name == self.settings.plug))
            .ok_or_else(|| error("Computer routine requires its configured outlet"))?;
        if devices.computer.is_none() {
            return Err(error("Computer routine requires a computer"));
        }
        if self.settings.shutdown_verification.is_some() && outlet.model != crate::plug::Model::P110
        {
            return Err(error("Metered standby cutoff requires a P110 outlet"));
        }
        if let Some(tv) = &devices.tv {
            let p = devices.plugs_ref().ok_or_else(|| error("Missing plugs"))?;
            for (index, plug) in p.config.plugs.iter().enumerate() {
                if plug.name == self.settings.plug
                    && tv.owns_power(Device {
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
        if power {
            json!({"steps":["read_outlet","turn_outlet_on_if_needed","wait_for_standby_power","send_wake_once_if_unreachable","wait_for_authenticated_running_service"],"startup_seconds":self.settings.startup_seconds,"ready_seconds":self.settings.ready_seconds})
        } else if let Some(verification) = self.settings.shutdown_verification {
            json!({"steps":["read_outlet_and_verify_running_consumption","persist_attempt","send_authenticated_shutdown_once","persist_acceptance","wait_for_unreachable_service_and_sustained_standby_consumption","persist_cutoff_attempt","turn_outlet_off_and_verify"],"shutdown_policy":"metered_standby","shutdown_verification":verification,"outlet_cutoff_enabled":true,"evidence":"accepted_shutdown_plus_unreachable_service_plus_calibrated_standby","physical_off_verified":false,"resume":"Only verification may resume after persisted acceptance, within 10 minutes; shutdown is never resent"})
        } else {
            json!({"steps":["read_outlet","send_authenticated_shutdown_once"],"shutdown_policy":"leave_on","outlet_cutoff_enabled":false,"physical_off_verified":false,"reason":"Shutdown completion evidence is not configured"})
        }
    }
    pub(crate) fn run(&mut self, devices: &mut Devices, power: bool) -> Result<Value> {
        self.validate(devices)?;
        let identity = shutdown_identity(devices, &self.settings.plug)?;
        if self.memory.shutdown_accepted.is_some()
            && self.memory.shutdown_identity.as_deref() != Some(&identity)
        {
            return Err(error(
                "Saved shutdown acceptance belongs to different equipment or credentials; cutoff aborted",
            ));
        }
        let now = chrono::Utc::now().timestamp();
        let resume = !power
            && self.settings.shutdown_verification.is_some()
            && self
                .memory
                .shutdown_accepted
                .is_some_and(|accepted| now >= accepted && now < accepted.saturating_add(600));
        if !resume
            && self
                .memory
                .attempt
                .is_some_and(|last| now < last.saturating_add(180))
        {
            return Err(error(
                "Recent computer workflow attempt; replay blocked for 180 seconds, including after restart",
            ));
        }
        if self.memory.shutdown_accepted.is_some() && !resume {
            return Err(error(
                "Previous shutdown/cutoff outcome is unresolved; inspect the computer and outlet before explicit maintenance",
            ));
        }
        if !resume {
            self.memory.attempt = Some(now);
            save_private(&self.path, &self.memory)?;
        }
        let mut control = Equipment {
            devices,
            plug: &self.settings.plug,
            started: Instant::now(),
        };
        let result = sequence(
            &mut control,
            power,
            self.settings.startup_seconds,
            self.settings.ready_seconds,
            self.settings.shutdown_verification,
            resume,
            &mut |step| {
                let now = chrono::Utc::now().timestamp();
                match step {
                    Progress::Accepted => {
                        self.memory.shutdown_accepted = Some(now);
                        self.memory.shutdown_identity = Some(identity.clone());
                    }
                    Progress::Cutoff => {
                        if !self.memory.shutdown_accepted.is_some_and(|accepted| {
                            now >= accepted && now < accepted.saturating_add(600)
                        }) {
                            return Err(error(
                                "Shutdown acceptance expired before cutoff; outlet left on",
                            ));
                        }
                        // Consume acceptance and refresh the durable attempt before
                        // any relay write, including a resumed verification.
                        self.memory.shutdown_accepted = None;
                        self.memory.shutdown_identity = None;
                        self.memory.attempt = Some(now);
                    }
                }
                save_private(&self.path, &self.memory)
            },
        )?;
        self.memory = Memory::default();
        save_private(&self.path, &self.memory)?;
        Ok(result)
    }
}
fn shutdown_identity(devices: &Devices, plug: &str) -> Result<String> {
    use sha2::{Digest, Sha256};
    let computer = devices
        .computer
        .as_ref()
        .ok_or_else(|| error("Missing computer"))?;
    let outlet = devices
        .plugs_ref()
        .and_then(|p| p.config.plugs.iter().find(|p| p.name == plug))
        .ok_or_else(|| error("Missing outlet"))?;
    let mut hash = Sha256::new();
    hash.update(computer.shutdown_identity());
    hash.update(outlet.name.as_bytes());
    hash.update([0]);
    hash.update(outlet.device_id.as_bytes());
    hash.update(outlet.host.to_string());
    hash.update(outlet.port.to_be_bytes());
    Ok(hash.finalize().iter().map(|b| format!("{b:02x}")).collect())
}
#[derive(Clone, Copy)]
struct Reading {
    power: bool,
    power_w: Option<f64>,
    auto_off_active: Option<bool>,
}
fn watts(reading: Reading) -> Result<f64> {
    if !reading.power || reading.auto_off_active != Some(false) {
        return Err(error(
            "Computer outlet power/auto-off changed or is unknown; cutoff aborted",
        ));
    }
    reading
        .power_w
        .filter(|w| w.is_finite() && *w >= 0.0)
        .ok_or_else(|| {
            error("Computer standby consumption is unavailable or invalid; outlet left on")
        })
}
trait Control {
    fn outlet(&mut self) -> Result<Reading>;
    fn set_outlet(&mut self, power: bool) -> Result<()>;
    fn running(&mut self) -> bool;
    fn unreachable(&mut self) -> Result<bool>;
    fn wake(&mut self) -> Result<()>;
    fn shutdown(&mut self) -> Result<()>;
    fn ready(&mut self, seconds: u64) -> Result<()>;
    fn wait(&mut self, seconds: u64);
    fn elapsed(&self) -> Duration;
}
struct Equipment<'a> {
    devices: &'a mut Devices,
    plug: &'a str,
    started: Instant,
}
impl Control for Equipment<'_> {
    fn outlet(&mut self) -> Result<Reading> {
        let status = self
            .devices
            .plugs()
            .ok_or_else(|| error("Missing outlet"))?
            .status(self.plug)?;
        Ok(Reading {
            power: status.power,
            power_w: status.energy.and_then(|e| e.power_w),
            auto_off_active: status.auto_off_active,
        })
    }
    fn set_outlet(&mut self, power: bool) -> Result<()> {
        self.devices
            .plugs()
            .ok_or_else(|| error("Missing outlet"))?
            .apply(self.plug, power)?;
        Ok(())
    }
    fn running(&mut self) -> bool {
        self.devices
            .computer
            .as_ref()
            .and_then(|c| c.status().ok())
            .is_some_and(|v| v["state"] == "running")
    }
    fn unreachable(&mut self) -> Result<bool> {
        self.devices
            .computer
            .as_ref()
            .ok_or_else(|| error("Missing computer"))?
            .unreachable()
    }
    fn wake(&mut self) -> Result<()> {
        self.devices
            .computer
            .as_ref()
            .ok_or_else(|| error("Missing computer"))?
            .wake()?;
        Ok(())
    }
    fn shutdown(&mut self) -> Result<()> {
        self.devices
            .computer
            .as_ref()
            .ok_or_else(|| error("Missing computer"))?
            .shutdown()?;
        Ok(())
    }
    fn ready(&mut self, seconds: u64) -> Result<()> {
        let until = Instant::now() + Duration::from_secs(seconds);
        loop {
            if self.running() {
                return Ok(());
            }
            if Instant::now() >= until {
                return Err(error(
                    "Computer readiness unverified; outlet left on; do not automatically replay",
                ));
            }
            self.wait(1);
        }
    }
    fn wait(&mut self, seconds: u64) {
        std::thread::sleep(Duration::from_secs(seconds));
    }
    fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }
}
fn verify_shutdown(control: &mut impl Control, policy: Verification) -> Result<Value> {
    let until = control.elapsed() + Duration::from_secs(policy.timeout_seconds);
    let mut stable_since = None;
    let mut samples = 0u32;
    let mut max_standby_w: f64 = 0.0;
    loop {
        let power_w = watts(control.outlet()?)?;
        let unreachable = control.unreachable()?;
        let now = control.elapsed();
        if now >= until {
            return Err(error(format!(
                "Computer shutdown verification timed out; outlet left on; last_power_w={power_w:.3}; service_unreachable={unreachable}; resume verification only after checking calibrated settings"
            )));
        }
        if unreachable && power_w <= policy.standby_power_w {
            let since = *stable_since.get_or_insert(now);
            samples = samples.saturating_add(1);
            max_standby_w = max_standby_w.max(power_w);
            if samples >= 3
                && now.saturating_sub(since) >= Duration::from_secs(policy.stable_seconds)
            {
                return Ok(
                    json!({"shutdown_verified":true,"verification_policy":"metered_standby","service_unreachable":true,"standby_power_w":power_w,"maximum_standby_power_w":max_standby_w,"stable_seconds":now.saturating_sub(since).as_secs(),"samples":samples,"physical_off_verified":false}),
                );
            }
        } else {
            stable_since = None;
            samples = 0;
            max_standby_w = 0.0;
        }
        control.wait(policy.poll_seconds);
    }
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Progress {
    Accepted,
    Cutoff,
}
fn sequence(
    control: &mut impl Control,
    power: bool,
    startup: u64,
    ready: u64,
    verification: Option<Verification>,
    resume: bool,
    progress: &mut impl FnMut(Progress) -> Result<()>,
) -> Result<Value> {
    let outlet = control.outlet()?;
    if power {
        if !outlet.power {
            control.set_outlet(true)?;
            control.wait(startup);
        }
        if !control.running() {
            control.wake()?;
            control.ready(ready)?;
        }
        Ok(json!({"completed":true,"power":true,"verified":true}))
    } else if !outlet.power {
        Ok(
            json!({"completed":true,"outlet_power":false,"physical_off_verified":false,"shutdown_requested":false}),
        )
    } else if let Some(policy) = verification {
        if !resume {
            if watts(outlet)? <= policy.standby_power_w + 10.0 {
                return Err(error(
                    "Running consumption is too close to standby threshold; shutdown/cutoff aborted",
                ));
            }
            control.shutdown()?;
            progress(Progress::Accepted)?;
        }
        let evidence = verify_shutdown(control, policy)?;
        progress(Progress::Cutoff)?;
        control.set_outlet(false)?;
        Ok(
            json!({"completed":true,"power":false,"shutdown_requested":true,"shutdown_verified":true,"outlet_power":false,"outlet_verified":true,"shutdown_policy":"metered_standby","verification":evidence,"resumed_verification":resume}),
        )
    } else {
        control.shutdown()?;
        Ok(
            json!({"shutdown_requested":true,"verified":false,"outlet_left_on":true,"outlet_cutoff_enabled":false,"shutdown_policy":"leave_on"}),
        )
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    struct Fake {
        reading: Reading,
        running: bool,
        unreachable: bool,
        fail_shutdown: bool,
        fail_read: bool,
        fail_auth: bool,
        readings: VecDeque<Option<f64>>,
        reachability: VecDeque<bool>,
        clock: Duration,
        steps: Vec<&'static str>,
    }
    impl Default for Fake {
        fn default() -> Self {
            Self {
                reading: Reading {
                    power: false,
                    power_w: Some(95.0),
                    auto_off_active: Some(false),
                },
                running: false,
                unreachable: true,
                fail_shutdown: false,
                fail_read: false,
                fail_auth: false,
                readings: VecDeque::new(),
                reachability: VecDeque::new(),
                clock: Duration::ZERO,
                steps: Vec::new(),
            }
        }
    }
    impl Control for Fake {
        fn outlet(&mut self) -> Result<Reading> {
            self.steps.push("read");
            if self.fail_read {
                return Err(error("read failed"));
            }
            if let Some(value) = self.readings.pop_front() {
                self.reading.power_w = value;
            }
            Ok(self.reading)
        }
        fn set_outlet(&mut self, power: bool) -> Result<()> {
            self.steps
                .push(if power { "outlet_on" } else { "outlet_off" });
            self.reading.power = power;
            Ok(())
        }
        fn running(&mut self) -> bool {
            self.steps.push("running");
            self.running
        }
        fn unreachable(&mut self) -> Result<bool> {
            self.steps.push("probe");
            if self.fail_auth {
                Err(error("auth"))
            } else {
                Ok(self.reachability.pop_front().unwrap_or(self.unreachable))
            }
        }
        fn wake(&mut self) -> Result<()> {
            self.steps.push("wake");
            Ok(())
        }
        fn shutdown(&mut self) -> Result<()> {
            self.steps.push("shutdown");
            if self.fail_shutdown {
                Err(error("failed"))
            } else {
                Ok(())
            }
        }
        fn ready(&mut self, _: u64) -> Result<()> {
            self.steps.push("ready");
            Ok(())
        }
        fn wait(&mut self, seconds: u64) {
            self.steps.push("wait");
            self.clock += Duration::from_secs(seconds)
        }
        fn elapsed(&self) -> Duration {
            self.clock
        }
    }
    fn policy() -> Verification {
        Verification {
            standby_power_w: 1.0,
            stable_seconds: 10,
            timeout_seconds: 30,
            poll_seconds: 2,
        }
    }
    fn off(c: &mut Fake, p: Option<Verification>, resume: bool) -> Result<Value> {
        sequence(c, false, 5, 120, p, resume, &mut |_| Ok(()))
    }
    fn on(c: &mut Fake) -> Result<Value> {
        sequence(c, true, 5, 120, None, false, &mut |_| Ok(()))
    }
    fn powered() -> Fake {
        Fake {
            reading: Reading {
                power: true,
                power_w: Some(95.0),
                auto_off_active: Some(false),
            },
            ..Fake::default()
        }
    }
    #[test]
    fn outlet_precedes_one_wake_and_readiness() {
        let mut c = Fake::default();
        on(&mut c).expect("startup");
        assert_eq!(
            c.steps,
            ["read", "outlet_on", "wait", "running", "wake", "ready"]
        );
    }
    #[test]
    fn leave_on_remains_supported() {
        let mut c = powered();
        c.running = true;
        on(&mut c).expect("already on");
        assert_eq!(c.steps, ["read", "running"]);
        c.steps.clear();
        assert_eq!(
            off(&mut c, None, false).expect("shutdown")["outlet_left_on"],
            true
        );
        assert_eq!(c.steps, ["read", "shutdown"]);
    }
    #[test]
    fn cutoff_follows_shutdown_and_stable_external_evidence() {
        let mut c = powered();
        c.readings = VecDeque::from([Some(95.0), Some(0.5)]);
        let result = off(&mut c, Some(policy()), false).expect("cutoff");
        assert_eq!(c.steps.iter().filter(|s| **s == "shutdown").count(), 1);
        assert_eq!(c.steps.last(), Some(&"outlet_off"));
        assert!(c.clock >= Duration::from_secs(10));
        assert_eq!(result["shutdown_verified"], true);
        assert_eq!(result["verification"]["physical_off_verified"], false);
    }
    #[test]
    fn high_consumption_or_reachable_service_never_cut_power() {
        for reachable in [true, false] {
            let mut c = powered();
            if reachable {
                c.readings = VecDeque::from([Some(95.0), Some(0.5)]);
                c.unreachable = false;
            }
            assert!(off(&mut c, Some(policy()), false).is_err());
            assert!(!c.steps.contains(&"outlet_off"));
        }
    }
    #[test]
    fn unstable_measurement_resets_window() {
        let mut c = powered();
        c.readings = VecDeque::from([Some(95.0), Some(0.5), Some(0.5), Some(2.0), Some(0.5)]);
        off(&mut c, Some(policy()), false).expect("eventually stable");
        assert!(c.clock >= Duration::from_secs(16));
    }
    #[test]
    fn missing_invalid_meter_auto_off_or_authentication_abort_cutoff() {
        for value in [None, Some(f64::NAN), Some(-1.0)] {
            let mut c = powered();
            c.readings = VecDeque::from([Some(95.0), value]);
            assert!(off(&mut c, Some(policy()), false).is_err());
            assert!(!c.steps.contains(&"outlet_off"));
        }
        for auto_off in [None, Some(true)] {
            let mut c = powered();
            c.reading.auto_off_active = auto_off;
            assert!(off(&mut c, Some(policy()), false).is_err());
            assert!(!c.steps.contains(&"shutdown"));
        }
        let mut c = powered();
        c.fail_auth = true;
        c.readings = VecDeque::from([Some(95.0), Some(0.5)]);
        assert!(off(&mut c, Some(policy()), false).is_err());
        assert!(!c.steps.contains(&"outlet_off"));
    }
    #[test]
    fn failed_shutdown_or_persistence_never_cut_power() {
        let mut c = powered();
        c.fail_shutdown = true;
        assert!(off(&mut c, Some(policy()), false).is_err());
        assert!(!c.steps.contains(&"outlet_off"));
        let mut c = powered();
        assert!(
            sequence(&mut c, false, 5, 120, Some(policy()), false, &mut |_| Err(
                error("save failed")
            ))
            .is_err()
        );
        assert!(!c.steps.contains(&"outlet_off"));
        let mut c = powered();
        c.readings = VecDeque::from([Some(95.0), Some(0.5)]);
        assert!(
            sequence(
                &mut c,
                false,
                5,
                120,
                Some(policy()),
                false,
                &mut |step| if step == Progress::Cutoff {
                    Err(error("save failed"))
                } else {
                    Ok(())
                }
            )
            .is_err()
        );
        assert!(!c.steps.contains(&"outlet_off"));
    }
    #[test]
    fn accepted_shutdown_can_resume_verification_without_resending() {
        let mut c = powered();
        c.reading.power_w = Some(0.5);
        off(&mut c, Some(policy()), true).expect("resume");
        assert!(!c.steps.contains(&"shutdown"));
        assert_eq!(c.steps.last(), Some(&"outlet_off"));
    }
    #[test]
    fn unsafe_threshold_or_already_low_baseline_fail_closed() {
        for standby_power_w in [f64::NAN, 0.0, 5.1] {
            assert!(
                Verification {
                    standby_power_w,
                    ..policy()
                }
                .validate()
                .is_err()
            );
        }
        let mut c = powered();
        c.reading.power_w = Some(2.0);
        assert!(off(&mut c, Some(policy()), false).is_err());
        assert!(!c.steps.contains(&"shutdown"));
    }
}
