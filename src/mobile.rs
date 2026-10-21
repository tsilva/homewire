//! Native companion telemetry. Ingestion is private API-only, never an MCP tool.
use crate::{
    Result, config,
    device::{Adapter, Monitored, Tool},
    entity::{Capabilities, Domain, Entity, Support},
    error,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub devices: Vec<String>,
    pub latitude: f64,
    pub longitude: f64,
    #[serde(default = "radius")]
    pub home_radius_m: f64,
    #[serde(default = "freshness")]
    pub max_age_seconds: i64,
    #[serde(default)]
    pub persons: Vec<Person>,
    #[serde(default)]
    pub battery_monitor: Option<crate::battery::Config>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Person {
    pub name: String,
    pub device: String,
}
fn radius() -> f64 {
    150.0
}
fn freshness() -> i64 {
    600
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Location {
    pub latitude: f64,
    pub longitude: f64,
    pub accuracy_m: f64,
    pub measured_at: i64,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Report {
    pub battery_percent: u8,
    pub battery_state: String,
    pub charger: String,
    #[serde(default)]
    pub location: Option<Location>,
    #[serde(default)]
    pub received_at: i64,
}
pub struct Manager {
    config: Config,
    state: BTreeMap<String, Report>,
    path: PathBuf,
    _lock: crate::config::FileLock,
    battery_monitor: Option<crate::battery::Monitor>,
}
impl Manager {
    pub fn open(path: &Path) -> Result<Self> {
        Self::from_source(&crate::config::Source::file(path))
    }
    pub fn from_source(source: &crate::config::Source<'_>) -> Result<Self> {
        let path = source.path();
        let mut config: Config = source.decode()?;
        if config.devices.is_empty()
            || config.devices.len() > 8
            || !config::valid_unique_labels(config.devices.iter().map(String::as_str), 32, b"-_")
            || !coordinates(config.latitude, config.longitude)
            || !(25.0..=5000.0).contains(&config.home_radius_m)
            || !(60..=86400).contains(&config.max_age_seconds)
            || config.persons.len() > 8
            || !config::valid_unique_labels(
                config.persons.iter().map(|p| p.name.as_str()),
                32,
                b"-_",
            )
            || config
                .persons
                .iter()
                .any(|p| !config.devices.contains(&p.device))
        {
            return Err(error("Invalid companion configuration"));
        }
        let state_path = path.with_extension("state.json");
        let state: BTreeMap<String, Report> =
            match config::read_bounded_limit(&state_path, "Companion state", 16384) {
                Ok(b) => serde_json::from_slice(&b)?,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
                Err(e) => return Err(e.into()),
            };
        if state.len() > 8
            || state
                .iter()
                .any(|(name, r)| !config.devices.contains(name) || !valid_report(r, i64::MAX / 2))
        {
            return Err(error("Invalid companion state"));
        }
        let battery_monitor = config
            .battery_monitor
            .take()
            .map(|monitor| crate::battery::Monitor::open(path, monitor, &config.devices))
            .transpose()?;
        Ok(Self {
            config,
            state,
            path: state_path,
            _lock: config::lock_configuration(path)?,
            battery_monitor,
        })
    }
    pub fn report(&mut self, name: &str, value: &Value, now: i64) -> Result<Value> {
        if !self.config.devices.iter().any(|s| s == name) {
            return Err(error("Unknown paired companion"));
        }
        let mut report: Report =
            serde_json::from_value(value.clone()).map_err(|_| error("Invalid companion report"))?;
        if !valid_report(&report, now) {
            return Err(error("Invalid companion measurement"));
        }
        report.received_at = now;
        let mut next = self.state.clone();
        next.insert(name.into(), report);
        config::save_private_bounded(&self.path, &next, 16384)?;
        self.state = next;
        if let Some(monitor) = &mut self.battery_monitor {
            monitor.report_received(name);
        }
        Ok(json!({"accepted":true,"received_at":now}))
    }
    fn status(&self, index: usize, now: i64) -> Result<Value> {
        let name = self
            .config
            .devices
            .get(index)
            .ok_or_else(|| error("Unknown companion"))?;
        let report = self
            .state
            .get(name)
            .filter(|r| now >= r.received_at && now - r.received_at <= self.config.max_age_seconds)
            .ok_or_else(|| error("Companion telemetry is missing or stale"))?;
        let location = report
            .location
            .as_ref()
            .filter(|l| now >= l.measured_at && now - l.measured_at <= self.config.max_age_seconds);
        let home = location.and_then(|l| {
            let distance = distance_m(
                self.config.latitude,
                self.config.longitude,
                l.latitude,
                l.longitude,
            );
            if distance + l.accuracy_m <= self.config.home_radius_m {
                Some(true)
            } else if distance - l.accuracy_m > self.config.home_radius_m {
                Some(false)
            } else {
                None
            }
        });
        Ok(
            json!({"device":name,"battery_percent":report.battery_percent,"battery_state":report.battery_state,"charger":report.charger,"location":location,"home":home,"measured_at":report.received_at}),
        )
    }
}
fn coordinates(lat: f64, lon: f64) -> bool {
    lat.is_finite()
        && lon.is_finite()
        && (-90.0..=90.0).contains(&lat)
        && (-180.0..=180.0).contains(&lon)
}
fn valid_report(r: &Report, now: i64) -> bool {
    r.battery_percent <= 100
        && ["charging", "discharging", "full", "unknown"].contains(&r.battery_state.as_str())
        && ["usb", "ac", "wireless", "dock", "none", "unknown"].contains(&r.charger.as_str())
        && r.location.as_ref().is_none_or(|l| {
            coordinates(l.latitude, l.longitude)
                && l.accuracy_m.is_finite()
                && (0.0..=100000.0).contains(&l.accuracy_m)
                && l.measured_at > 0
                && l.measured_at <= now + 60
        })
}
fn distance_m(a: f64, b: f64, c: f64, d: f64) -> f64 {
    let x = ((c - a).to_radians() / 2.0).sin().powi(2)
        + a.to_radians().cos() * c.to_radians().cos() * ((d - b).to_radians() / 2.0).sin().powi(2);
    6371000.0 * 2.0 * x.sqrt().atan2((1.0 - x).max(0.0).sqrt())
}
impl Adapter for Manager {
    fn next_background(&self) -> Option<std::time::Instant> {
        self.battery_monitor
            .as_ref()
            .map(crate::battery::Monitor::next_poll)
    }
    fn background_tick(&mut self) -> Result<()> {
        if let Some(monitor) = self.battery_monitor.as_mut() {
            monitor.background_tick(&self.state, self.config.max_age_seconds)?;
        }
        Ok(())
    }
    fn reconcile_alerts(&self, alerts: &mut crate::alerts::Manager, now: i64) -> Result<()> {
        if let Some(monitor) = &self.battery_monitor {
            monitor.reconcile_alerts(alerts, now)?;
        }
        Ok(())
    }
    const CONFIG_SECTION: &'static str = "services";
    const TOOLS: &'static [Tool] = &[];
    const SUPPORT: Support = Support::READ;
    fn from_source(source: &crate::config::Source<'_>) -> Result<Self> {
        Self::from_source(source)
    }
    fn execute(&mut self, _: &str, _: &Value) -> Result<Value> {
        Err(error("Companion ingestion requires its paired private API"))
    }
    fn entities<'a>(&'a self, visit: &mut impl FnMut(Entity<'a>)) {
        for (i, name) in self.config.devices.iter().enumerate() {
            let base = Entity::new(
                crate::device::Family::Mobile,
                i,
                Domain::DeviceTracker,
                name,
                "Homewire Android companion",
                Capabilities::Read,
            );
            visit(base);
            for (s, f, u) in [
                ("_battery", "battery_percent", "%"),
                ("_battery_state", "battery_state", ""),
                ("_charger", "charger", ""),
                ("_home", "home", ""),
            ] {
                visit(base.sensor(s, f, u));
            }
        }
        for p in &self.config.persons {
            if let Some(i) = self.config.devices.iter().position(|d| d == &p.device) {
                visit(Entity::new(
                    crate::device::Family::Mobile,
                    i,
                    Domain::Person,
                    &p.name,
                    "Homewire person",
                    Capabilities::Read,
                ));
            }
        }
    }
    fn read_device(&mut self, index: usize) -> Result<Value> {
        self.status(index, chrono::Utc::now().timestamp())
    }
}
impl Monitored for Manager {
    type Unavailable = crate::battery::Unavailable;
    type Event = crate::fault::Event;
    fn poll_seconds(&self) -> u64 {
        self.battery_monitor
            .as_ref()
            .map_or(60, crate::battery::Monitor::poll_seconds)
    }
    fn watch(&mut self) -> Result<crate::fault::Watch<Self::Unavailable>> {
        self.battery_monitor
            .as_mut()
            .ok_or_else(|| error("Battery monitoring is not configured"))?
            .watch(
                &self.state,
                self.config.max_age_seconds,
                chrono::Utc::now().timestamp(),
            )
    }
    fn active_faults(&self, visit: &mut impl FnMut(&str, &[String])) {
        if let Some(monitor) = &self.battery_monitor {
            monitor.active_faults(visit);
        }
    }
}
