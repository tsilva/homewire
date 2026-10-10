//! Explicit shared-outlet workflow. Device protocols remain in their adapters.
use crate::{
    Result, androidtv,
    config::{lock_configuration, read_bounded, save_private},
    device::{Adapter, Hints, Input, Tool},
    error, media, plug, samsung,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs::File,
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    samsung: PathBuf,
    digi: PathBuf,
    plugs: PathBuf,
    #[serde(flatten)]
    settings: Settings,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Settings {
    plug: String,
    shutdown_policy: String,
    timings: Timings,
}
#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Timings {
    startup_seconds: u64,
    shutdown_seconds: u64,
    ready_seconds: u64,
}
impl Timings {
    fn validate(&self) -> Result<()> {
        if !(1..=60).contains(&self.startup_seconds)
            || !(1..=60).contains(&self.shutdown_seconds)
            || !(1..=120).contains(&self.ready_seconds)
        {
            return Err(error(
                "TV timings require startup/shutdown 1–60 seconds and readiness 1–120 seconds",
            ));
        }
        Ok(())
    }
}
#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Memory {
    attempt: Option<u64>,
}
struct Equipment {
    samsung: samsung::Manager,
    digi: androidtv::Manager,
    plugs: plug::Manager,
    plug: String,
}
pub struct Manager {
    equipment: Equipment,
    path: PathBuf,
    memory: Memory,
    timings: Timings,
    _lock: File,
}
// Internal seam for real I/O and simulated sequences; no dynamic dispatch.
trait Control {
    fn outlet(&mut self) -> Result<bool>;
    fn set_outlet(&mut self, power: bool) -> Result<()>;
    fn samsung(&mut self, power: bool) -> Result<()>;
    fn ready_digi(&mut self, seconds: u64) -> Result<()>;
    fn digi(&mut self, power: bool) -> Result<()>;
    fn wait(&mut self, seconds: u64);
}
impl Control for Equipment {
    fn outlet(&mut self) -> Result<bool> {
        Ok(self.plugs.status(&self.plug)?.power)
    }
    fn set_outlet(&mut self, power: bool) -> Result<()> {
        self.plugs.apply(&self.plug, power)?;
        Ok(())
    }
    fn samsung(&mut self, power: bool) -> Result<()> {
        let result = self.samsung.apply(power)?;
        if power && result["verified"] != true {
            return Err(error("Samsung wake was not verified"));
        }
        Ok(())
    }
    fn ready_digi(&mut self, seconds: u64) -> Result<()> {
        let until = Instant::now() + Duration::from_secs(seconds);
        loop {
            if self.digi.status().is_ok() {
                return Ok(());
            }
            if Instant::now() >= until {
                return Err(error(
                    "DIGI did not become available within cold-boot wait; outlet left on",
                ));
            }
            self.wait(1);
        }
    }
    fn digi(&mut self, power: bool) -> Result<()> {
        self.digi.apply(power)?;
        Ok(())
    }
    fn wait(&mut self, seconds: u64) {
        std::thread::sleep(Duration::from_secs(seconds));
    }
}
fn sequence(equipment: &mut impl Control, power: bool, timings: Timings) -> Result<()> {
    let outlet = equipment.outlet()?;
    if power {
        if !outlet {
            equipment.set_outlet(true)?;
            equipment.wait(timings.startup_seconds);
        }
        equipment.samsung(true)?;
        equipment.ready_digi(timings.ready_seconds)?;
        equipment.digi(true)?;
    } else if outlet {
        // Confirm DIGI is reachable before requesting either device's shutdown.
        // No cold-boot wait on off: unknown state aborts and leaves outlet on.
        equipment.digi(false)?;
        equipment.samsung(false)?;
        // Explicit fixed-delay cutoff is not proof that the TV is off.
        equipment.wait(timings.shutdown_seconds);
        equipment.set_outlet(false)?;
    }
    Ok(())
}
impl Manager {
    pub(crate) fn owns_power(&self, device: crate::entity::Device) -> bool {
        use crate::device::Family;
        match device.family {
            Family::Samsung | Family::Digi => true,
            Family::Plugs => self
                .equipment
                .plugs
                .config
                .plugs
                .get(device.index)
                .is_some_and(|p| p.name == self.equipment.plug),
            _ => false,
        }
    }
    pub(crate) fn owns_command(&self, name: &str, args: &Value) -> bool {
        matches!(name, "samsung_apply" | "digi_apply")
            || name == "plug_apply" && args["plug"].as_str() == Some(&self.equipment.plug)
    }
    pub(crate) fn child_entities<'a>(&'a self, visit: &mut impl FnMut(crate::entity::Entity<'a>)) {
        let mut owned = |mut entity: crate::entity::Entity<'a>| {
            if self.owns_power(entity.device) && entity.domain != crate::entity::Domain::Sensor {
                entity.owner = Some("tv_on/tv_off");
            }
            visit(entity);
        };
        self.equipment.samsung.entities(&mut owned);
        self.equipment.digi.entities(&mut owned);
        self.equipment.plugs.entities(&mut owned);
    }
    pub(crate) fn child_wake_targets<'a>(&'a self, visit: &mut impl FnMut(crate::wol::Target<'a>)) {
        self.equipment.samsung.wake_targets(visit);
    }
    pub(crate) fn samsung(&mut self) -> &mut samsung::Manager {
        &mut self.equipment.samsung
    }
    pub(crate) fn digi(&mut self) -> &mut androidtv::Manager {
        &mut self.equipment.digi
    }
    pub(crate) fn plugs(&mut self) -> &mut plug::Manager {
        &mut self.equipment.plugs
    }
    pub(crate) fn plugs_ref(&self) -> &plug::Manager {
        &self.equipment.plugs
    }

    pub fn open(path: &Path) -> Result<Self> {
        Self::from_source(&crate::config::Source::file(path))
    }
    pub fn from_source(source: &crate::config::Source<'_>) -> Result<Self> {
        let config: Config = source.decode()?;
        let parent = source.path().parent().unwrap_or(Path::new("."));
        let resolve = |p: PathBuf| if p.is_absolute() { p } else { parent.join(p) };
        let samsung = samsung::Manager::open(&resolve(config.samsung))?;
        let digi = androidtv::Manager::open(&resolve(config.digi))?;
        let plugs = plug::Manager::open(&resolve(config.plugs))?;
        Self::assemble(source.path(), config.settings, samsung, digi, plugs)
    }
    pub(crate) fn from_children(
        source: &crate::config::Source<'_>,
        samsung: samsung::Manager,
        digi: androidtv::Manager,
        plugs: plug::Manager,
    ) -> Result<Self> {
        Self::assemble(source.path(), source.decode()?, samsung, digi, plugs)
    }
    fn assemble(
        path: &Path,
        config: Settings,
        samsung: samsung::Manager,
        digi: androidtv::Manager,
        plugs: plug::Manager,
    ) -> Result<Self> {
        if config.shutdown_policy != "fixed_delay"
            || config.plug.is_empty()
            || config.plug.len() > 32
        {
            return Err(error(
                "TV requires a configured plug and explicit shutdown_policy fixed_delay",
            ));
        }
        config.timings.validate()?;
        let lock = lock_configuration(path)?;
        let equipment = Equipment {
            samsung,
            digi,
            plugs,
            plug: config.plug,
        };
        if !equipment
            .plugs
            .config
            .plugs
            .iter()
            .any(|p| p.name == equipment.plug)
        {
            return Err(error("TV plug is absent from the plugs configuration"));
        }
        let state_path = path.with_extension("state.json");
        let memory = match read_bounded(&state_path, "TV workflow state") {
            Ok(b) => serde_json::from_slice(&b).map_err(|_| error("Invalid TV workflow state"))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Memory::default(),
            Err(e) => return Err(e.into()),
        };
        Ok(Self {
            equipment,
            path: state_path,
            memory,
            timings: config.timings,
            _lock: lock,
        })
    }
    pub fn preview(&self, power: bool) -> Value {
        json!({"power":power,"timings":self.timings,"live_state_checked":false,"provisional":true,"shutdown_policy":"fixed_delay","physical_samsung_off_verified":false,"steps":if power {vec!["outlet_on_if_needed","configured_startup_wait_if_changed","samsung_wake_verify","digi_ready_with_configured_deadline","digi_wake_verify"]} else {vec!["if_outlet_on_digi_sleep_verify","samsung_shutdown_once","configured_shutdown_wait","outlet_off_verify"]}})
    }
    pub fn status(&self) -> Result<Value> {
        let outlet = self.equipment.plugs.status(&self.equipment.plug)?;
        // Device errors remain explicit; an unpowered/unreachable device is not off.
        let samsung = self.equipment.samsung.status();
        let digi = self.equipment.digi.status();
        Ok(
            json!({"outlet":outlet,"samsung":samsung.as_ref().ok(),"samsung_error":samsung.err().map(|e|e.to_string()),"digi":digi.as_ref().ok(),"digi_error":digi.err().map(|e|e.to_string())}),
        )
    }
    pub fn apply(&mut self, power: bool) -> Result<Value> {
        let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
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
        sequence(&mut self.equipment, power, self.timings)?;
        self.memory.attempt = None;
        save_private(&self.path, &self.memory)?;
        Ok(
            json!({"completed":true,"power":power,"shutdown_policy":"fixed_delay","physical_samsung_off_verified":false}),
        )
    }
}
impl Adapter for Manager {
    fn next_background(&self) -> Option<Instant> {
        self.equipment.plugs.next_background()
    }
    fn background_tick(&mut self) -> Result<()> {
        self.equipment.plugs.background_tick()
    }
    fn reconcile_alerts(&self, alerts: &mut crate::alerts::Manager, now: i64) -> Result<()> {
        self.equipment.plugs.reconcile_alerts(alerts, now)
    }

    const CONFIG_SECTION: &'static str = "routines";
    const SUPPORT: crate::entity::Support = crate::entity::Support::ROUTINES;
    fn entities<'a>(&'a self, _visit: &mut impl FnMut(crate::entity::Entity<'a>)) {}

    const TOOLS: &'static [Tool] = &[
        Tool {
            name: "tv_status",
            description: "Read shared outlet and both media device states; unknown is explicit",
            input: Input::Empty,
            hints: Hints::READ,
        },
        Tool {
            name: "tv_apply",
            description: "Run configured outlet-first startup or explicit fixed-delay outlet-last shutdown",
            input: media::POWER,
            hints: Hints {
                idempotent: false,
                destructive: Some(true),
                ..Hints::CONTROL
            },
        },
    ];
    fn from_source(source: &crate::config::Source<'_>) -> Result<Self> {
        Self::from_source(source)
    }
    fn execute(&mut self, name: &str, args: &Value) -> Result<Value> {
        match name {
            "tv_status" => self.status(),
            "tv_apply" => self.apply(
                args["power"]
                    .as_bool()
                    .ok_or_else(|| error("Expected boolean power"))?,
            ),
            _ => Err(error("Unknown TV workflow")),
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn timings() -> Timings {
        Timings {
            startup_seconds: 5,
            shutdown_seconds: 5,
            ready_seconds: 120,
        }
    }
    struct Fake {
        outlet: bool,
        fail: Option<&'static str>,
        calls: Vec<String>,
        ready_timeout: Option<u64>,
    }
    impl Fake {
        fn step(&mut self, name: &str) -> Result<()> {
            self.calls.push(name.into());
            if self.fail == Some(name) {
                return Err(error("injected device failure"));
            }
            Ok(())
        }
    }
    impl Control for Fake {
        fn outlet(&mut self) -> Result<bool> {
            self.step("outlet_status")?;
            Ok(self.outlet)
        }
        fn set_outlet(&mut self, p: bool) -> Result<()> {
            self.step(if p { "outlet_on" } else { "outlet_off" })?;
            self.outlet = p;
            Ok(())
        }
        fn samsung(&mut self, p: bool) -> Result<()> {
            self.step(if p { "samsung_on" } else { "samsung_off" })
        }
        fn ready_digi(&mut self, seconds: u64) -> Result<()> {
            self.ready_timeout = Some(seconds);
            self.step("digi_ready")
        }
        fn digi(&mut self, p: bool) -> Result<()> {
            self.step(if p { "digi_on" } else { "digi_off" })
        }
        fn wait(&mut self, s: u64) {
            self.calls.push(format!("wait{s}"));
        }
    }
    #[test]
    fn startup_and_shutdown_order_and_short_circuits() -> Result<()> {
        let mut f = Fake {
            outlet: false,
            fail: None,
            calls: vec![],
            ready_timeout: None,
        };
        sequence(&mut f, true, timings())?;
        assert_eq!(
            f.calls,
            [
                "outlet_status",
                "outlet_on",
                "wait5",
                "samsung_on",
                "digi_ready",
                "digi_on"
            ]
        );
        f.calls.clear();
        sequence(&mut f, false, timings())?;
        assert_eq!(
            f.calls,
            [
                "outlet_status",
                "digi_off",
                "samsung_off",
                "wait5",
                "outlet_off"
            ]
        );
        f.calls.clear();
        sequence(&mut f, false, timings())?;
        assert_eq!(f.calls, ["outlet_status"]);
        f.outlet = true;
        f.calls.clear();
        sequence(&mut f, true, timings())?;
        assert_eq!(
            f.calls,
            ["outlet_status", "samsung_on", "digi_ready", "digi_on"]
        );
        Ok(())
    }
    #[test]
    fn failures_never_cut_the_shared_outlet_or_continue_writes() {
        for fail in ["outlet_status", "digi_off", "samsung_off"] {
            let mut f = Fake {
                outlet: true,
                fail: Some(fail),
                calls: vec![],
                ready_timeout: None,
            };
            assert!(sequence(&mut f, false, timings()).is_err());
            assert!(f.outlet);
            assert!(!f.calls.iter().any(|s| s == "outlet_off"));
        }
        for fail in ["outlet_on", "samsung_on", "digi_ready"] {
            let mut f = Fake {
                outlet: false,
                fail: Some(fail),
                calls: vec![],
                ready_timeout: None,
            };
            assert!(sequence(&mut f, true, timings()).is_err());
            assert!(!f.calls.iter().any(|s| s == "digi_on"));
        }
    }
    #[test]
    fn custom_timings_drive_every_sequence_wait() -> Result<()> {
        let custom = Timings {
            startup_seconds: 7,
            shutdown_seconds: 9,
            ready_seconds: 45,
        };
        custom.validate()?;
        let mut f = Fake {
            outlet: false,
            fail: None,
            calls: vec![],
            ready_timeout: None,
        };
        sequence(&mut f, true, custom)?;
        assert!(f.calls.iter().any(|s| s == "wait7"));
        assert_eq!(f.ready_timeout, Some(45));
        f.calls.clear();
        sequence(&mut f, false, custom)?;
        assert!(f.calls.iter().any(|s| s == "wait9"));
        for bad in [
            Timings {
                startup_seconds: 0,
                ..custom
            },
            Timings {
                shutdown_seconds: 61,
                ..custom
            },
            Timings {
                ready_seconds: 121,
                ..custom
            },
        ] {
            assert!(bad.validate().is_err());
        }
        Ok(())
    }
}
