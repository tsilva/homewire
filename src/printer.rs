//! Named printers, read-only status, and durable fault transition monitoring.
use crate::device::{Adapter, Hints, Input, Tool};
use crate::{
    Result,
    config::valid_unique_labels,
    error, fault,
    ipp::{self, Attributes},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    net::{IpAddr, SocketAddr},
    path::Path,
    time::Duration,
};

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Endpoint {
    pub name: String,
    pub host: IpAddr,
    #[serde(default = "port")]
    pub port: u16,
    #[serde(default = "path")]
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connect: Option<SocketAddr>,
    #[serde(default = "supply_threshold", skip_serializing_if = "Option::is_none")]
    pub supply_warning_percent: Option<u8>,
}
fn port() -> u16 {
    631
}
fn path() -> String {
    "/ipp/print".into()
}
fn timeout() -> u64 {
    8
}
fn interval() -> u64 {
    30
}
fn supply_threshold() -> Option<u8> {
    Some(15)
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub printers: Vec<Endpoint>,
    #[serde(default = "timeout")]
    pub timeout_seconds: u64,
    #[serde(default = "interval")]
    pub poll_seconds: u64,
}
impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        Self::from_source(&crate::config::Source::file(path))
    }
    pub fn from_source(source: &crate::config::Source<'_>) -> Result<Self> {
        let config: Self = source.decode()?;
        config.validate()?;
        Ok(config)
    }
    pub fn validate(&self) -> Result<()> {
        if self.printers.is_empty()
            || self.printers.len() > 8
            || !(1..=30).contains(&self.timeout_seconds)
            || !(5..=300).contains(&self.poll_seconds)
        {
            return Err(error(
                "Configure 1–8 printers; timeout 1–30 seconds; poll interval 5–300 seconds",
            ));
        }
        if !valid_unique_labels(self.printers.iter().map(|p| p.name.as_str()), 32, b"-_") {
            return Err(error(
                "Printer names must be unique, with 1–32 letters, digits, hyphens or underscores",
            ));
        }
        for printer in &self.printers {
            if printer.port == 0
                || printer.supply_warning_percent.is_some_and(|v| v > 100)
                || printer.connect.is_some_and(|a| a.port() == 0)
                || !printer.path.starts_with('/')
                || printer.path.len() > 256
                || !printer
                    .path
                    .bytes()
                    .all(|b| b.is_ascii_graphic() && !matches!(b, b'#' | b'?' | b'\\'))
            {
                return Err(error("Invalid printer port or path"));
            }
        }
        Ok(())
    }
}
impl Endpoint {
    pub fn authority(&self) -> String {
        SocketAddr::new(self.host, self.port).to_string()
    }
    pub fn identity(&self) -> String {
        format!("{}{}", self.authority(), self.path)
    }
    pub fn read(&self, timeout: Duration) -> Result<Status> {
        let attributes = ipp::query(
            self.connect
                .unwrap_or(SocketAddr::new(self.host, self.port)),
            &self.authority(),
            &self.path,
            timeout,
        )?;
        Status::from_attributes(&self.name, &attributes)
    }
}
#[derive(Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    Idle,
    Printing,
    Stopped,
    Unknown,
}
#[derive(Clone, Serialize)]
pub struct Supply {
    pub name: Option<String>,
    pub kind: Option<String>,
    pub color: Option<String>,
    pub percent: Option<u8>,
    pub reported_level: Option<i32>,
}
#[derive(Clone, Serialize)]
pub struct Status {
    pub printer: String,
    pub model: Option<String>,
    pub state: State,
    pub reasons: Vec<String>,
    pub message: Option<String>,
    pub accepting_jobs: Option<bool>,
    pub queued_jobs: Option<u32>,
    pub supplies: Vec<Supply>,
    pub faults_known: bool,
    pub fault_active: Option<bool>,
    pub faults: Vec<String>,
}
impl Status {
    pub fn from_attributes(printer: &str, attributes: &Attributes) -> Result<Self> {
        let state = match integer(attributes, "printer-state", 0) {
            Some(3) => State::Idle,
            Some(4) => State::Printing,
            Some(5) => State::Stopped,
            _ => State::Unknown,
        };
        let mut reasons = Vec::new();
        if let Some(values) = attributes.get("printer-state-reasons") {
            for value in values {
                let Some(reason) = value.as_str() else {
                    return Err(error("Invalid IPP printer-state-reasons"));
                };
                if reason.len() > 128 {
                    return Err(error("Printer reason exceeds 128 bytes"));
                }
                reasons.push(reason.trim().to_ascii_lowercase());
            }
        }
        reasons.sort();
        reasons.dedup();
        let (faults_known, faults) = classify(state, &reasons);
        let supply_count = [
            "marker-names",
            "marker-levels",
            "marker-colors",
            "marker-types",
        ]
        .iter()
        .filter_map(|key| attributes.get(*key).map(Vec::len))
        .max()
        .unwrap_or(0);
        if supply_count > 16 {
            return Err(error("Too many printer supplies"));
        }
        let supplies = (0..supply_count)
            .map(|i| {
                let level = integer(attributes, "marker-levels", i);
                Supply {
                    name: text(attributes, "marker-names", i),
                    kind: text(attributes, "marker-types", i),
                    color: text(attributes, "marker-colors", i),
                    percent: level.filter(|n| (0..=100).contains(n)).map(|n| n as u8),
                    reported_level: level,
                }
            })
            .collect();
        Ok(Self {
            printer: printer.into(),
            model: text(attributes, "printer-make-and-model", 0),
            state,
            reasons,
            message: text(attributes, "printer-state-message", 0),
            accepting_jobs: attributes
                .get("printer-is-accepting-jobs")
                .and_then(|a| a.first())
                .and_then(Value::as_bool),
            queued_jobs: integer(attributes, "queued-job-count", 0)
                .and_then(|n| u32::try_from(n).ok()),
            supplies,
            faults_known,
            fault_active: faults_known.then_some(!faults.is_empty()),
            faults,
        })
    }
}
fn text(attributes: &Attributes, key: &str, index: usize) -> Option<String> {
    attributes.get(key)?.get(index)?.as_str().map(str::to_owned)
}
fn integer(attributes: &Attributes, key: &str, index: usize) -> Option<i32> {
    attributes
        .get(key)?
        .get(index)?
        .as_i64()
        .and_then(|n| i32::try_from(n).ok())
}
pub fn classify(state: State, reasons: &[String]) -> (bool, Vec<String>) {
    if state == State::Unknown || reasons.is_empty() {
        return (false, vec![]);
    }
    let mut faults = BTreeSet::new();
    let mut unclassified = false;
    for reason in reasons {
        let base = ["-error", "-warning", "-report"]
            .iter()
            .find_map(|suffix| reason.strip_suffix(suffix))
            .unwrap_or(reason);
        if matches!(
            base,
            "none"
                | "paused"
                | "moving-to-paused"
                | "shutdown"
                | "power-save"
                | "sleep"
                | "offline"
                | "connecting-to-device"
        ) {
            continue;
        }
        if matches!(
            base,
            "media-empty"
                | "media-jam"
                | "toner-empty"
                | "toner-low"
                | "marker-supply-empty"
                | "marker-supply-low"
                | "cover-open"
                | "door-open"
                | "input-tray-missing"
                | "output-tray-missing"
                | "output-area-full"
                | "output-area-almost-full"
                | "marker-failure"
                | "fuser-over-temp"
                | "fuser-under-temp"
                | "opc-life-over"
                | "interpreter-resource-unavailable"
        ) || reason.ends_with("-error")
        {
            faults.insert(reason.clone());
        } else {
            unclassified = true;
        }
    }
    let known = !faults.is_empty() || (state != State::Stopped && !unclassified);
    (known, faults.into_iter().collect())
}

pub use crate::fault::{Memory, Monitor, Saved};
#[derive(Serialize)]
pub struct Event {
    pub printer: String,
    pub event: &'static str,
    pub faults: Vec<String>,
    pub previous_faults: Vec<String>,
}
impl From<fault::Event> for Event {
    fn from(event: fault::Event) -> Self {
        Self {
            printer: event.device,
            event: event.event,
            faults: event.faults,
            previous_faults: event.previous_faults,
        }
    }
}
impl Monitor {
    pub fn observe(
        &mut self,
        endpoint: &Endpoint,
        status: Option<&Status>,
        now: u64,
    ) -> Option<Event> {
        self.observe_faults(
            &endpoint.name,
            &endpoint.identity(),
            status
                .filter(|s| s.faults_known)
                .map(|s| s.faults.as_slice()),
            now,
        )
        .map(Event::from)
    }
}

pub struct Manager {
    pub config: Config,
    pub monitor: Monitor,
    store: fault::Store,
    warnings: Monitor,
    warning_path: std::path::PathBuf,
    started: std::time::Instant,
}
#[derive(Serialize)]
pub struct Check {
    pub events: Vec<Event>,
    pub statuses: Vec<Status>,
    pub unavailable: Vec<Unavailable>,
    pub active_faults: std::collections::BTreeMap<String, Vec<String>>,
    pub supply_events: Vec<SupplyEvent>,
    pub active_supply_warnings: std::collections::BTreeMap<String, Vec<String>>,
}
#[derive(Serialize)]
pub struct SupplyEvent {
    pub printer: String,
    pub supply: usize,
    pub event: &'static str,
    pub warnings: Vec<String>,
    pub previous_warnings: Vec<String>,
}
#[derive(Serialize)]
#[serde(untagged)]
pub enum WatchEvent {
    Fault(Event),
    Supply(SupplyEvent),
}
#[derive(Serialize)]
pub struct Unavailable {
    pub printer: String,
    pub error: String,
}
impl Manager {
    pub fn open(path: &Path) -> Result<Self> {
        Self::from_source(&crate::config::Source::file(path))
    }
    pub fn from_source(source: &crate::config::Source<'_>) -> Result<Self> {
        let path = source.path();
        let config = Config::from_source(source)?;
        let (store, monitor) = fault::Store::open(path, &config.printers)?;
        let warning_path = path.with_extension("supply-state.json");
        let mut identities = std::collections::BTreeMap::new();
        for p in &config.printers {
            for i in 0..16 {
                identities.insert(format!("{}_supply_{i}", p.name), p.identity());
            }
        }
        let warnings = Monitor::load(&warning_path, &identities)?;
        Ok(Self {
            config,
            monitor,
            store,
            warnings,
            warning_path,
            started: std::time::Instant::now(),
        })
    }
    pub fn status(&self, name: &str) -> Result<Status> {
        let printer = self
            .config
            .printers
            .iter()
            .find(|p| p.name == name)
            .ok_or_else(|| error("Unknown configured printer"))?;
        printer.read(Duration::from_secs(self.config.timeout_seconds))
    }
    pub fn check(&mut self) -> Result<Check> {
        let check = self.store.check(
            &mut self.monitor,
            &self.config.printers,
            Duration::from_secs(self.config.timeout_seconds),
        )?;
        let supply_events =
            self.observe_supplies(&check.statuses, self.started.elapsed().as_secs())?;
        Ok(Check {
            events: check.events,
            statuses: check.statuses,
            unavailable: check.unavailable,
            active_faults: check.active_faults,
            supply_events,
            active_supply_warnings: self.warnings.active_faults(),
        })
    }
    pub fn watch(&mut self) -> Result<fault::Watch<Unavailable, WatchEvent>> {
        let check = self.check()?;
        Ok(fault::Watch {
            events: check
                .events
                .into_iter()
                .map(WatchEvent::Fault)
                .chain(check.supply_events.into_iter().map(WatchEvent::Supply))
                .collect(),
            unavailable: check.unavailable,
        })
    }
    fn observe_supplies(&mut self, statuses: &[Status], now: u64) -> Result<Vec<SupplyEvent>> {
        let mut warnings = self.warnings.clone();
        let mut events = Vec::new();
        for p in &self.config.printers {
            let status = statuses.iter().find(|s| s.printer == p.name);
            for i in 0..16 {
                let key = format!("{}_supply_{i}", p.name);
                let observation = status
                    .and_then(|s| s.supplies.get(i))
                    .and_then(|s| s.percent)
                    .zip(p.supply_warning_percent)
                    .map(|(percent, threshold)| {
                        if percent <= threshold {
                            vec![format!("low_supply_at_or_below_{threshold}_percent")]
                        } else {
                            vec![]
                        }
                    });
                if let Some(event) =
                    warnings.observe_faults(&key, &p.identity(), observation.as_deref(), now)
                {
                    events.push(SupplyEvent {
                        printer: p.name.clone(),
                        supply: i,
                        event: if event.faults.is_empty() {
                            "supply_recovered"
                        } else {
                            "supply_warning"
                        },
                        warnings: event.faults,
                        previous_warnings: event.previous_faults,
                    });
                }
            }
        }
        if !events.is_empty() {
            warnings.save(&self.warning_path)?;
        }
        self.warnings = warnings;
        Ok(events)
    }
}

impl fault::Target for Endpoint {
    type Status = Status;
    type Unavailable = Unavailable;
    type Event = Event;
    fn name(&self) -> &str {
        &self.name
    }
    fn identity(&self) -> String {
        Endpoint::identity(self)
    }
    fn read(&self, timeout: Duration) -> Result<Status> {
        Endpoint::read(self, timeout)
    }
    fn faults(status: &Status) -> fault::Faults<'_> {
        if status.faults_known {
            fault::Faults::Many(&status.faults)
        } else {
            fault::Faults::Unknown
        }
    }
    fn unavailable(&self, error: String) -> Unavailable {
        Unavailable {
            printer: self.name.clone(),
            error,
        }
    }
}

impl Adapter for Manager {
    const SUPPORT: crate::entity::Support = crate::entity::Support::READ;
    fn entities<'a>(&'a self, visit: &mut impl FnMut(crate::entity::Entity<'a>)) {
        use crate::{
            device::Family,
            entity::{Capabilities, Domain, Entity},
        };
        for (index, endpoint) in self.config.printers.iter().enumerate() {
            let base = Entity::new(
                Family::Printers,
                index,
                Domain::Printer,
                &endpoint.name,
                "unknown",
                Capabilities::Read,
            );
            visit(base);
            for (suffix, field, unit) in [
                ("_state", "state", None),
                ("_queued_jobs", "queued_jobs", Some("jobs")),
                ("_accepting_jobs", "accepting_jobs", None),
                ("_fault_active", "fault_active", None),
            ] {
                let mut sensor = base.sensor(suffix, field, unit.unwrap_or(""));
                sensor.unit = unit;
                visit(sensor);
            }
        }
    }
    fn read_device(&mut self, index: usize) -> Result<Value> {
        let name = &self
            .config
            .printers
            .get(index)
            .ok_or_else(|| error("Unknown printer"))?
            .name;
        Ok(serde_json::to_value(self.status(name)?)?)
    }

    const TOOLS: &'static [Tool] = &[
        Tool {
            name: "printer_list",
            description: "List configured printers without network queries.",
            input: Input::Empty,
            hints: Hints::LIST,
        },
        Tool {
            name: "printer_check",
            description: "Poll all printers and return debounced hardware fault and separate low-supply warning/recovery events plus retained alerts. Unavailable readings never clear alerts. Use repeatedly in this server for monitoring.",
            input: Input::Empty,
            hints: Hints::CHECK,
        },
        Tool {
            name: "printer_status",
            description: "Read live printer state, reported fault reasons, job count, and ink/toner levels using IPP. Sends no print jobs or controls.",
            input: Input::Named {
                key: "printer",
                extra: None,
                error: "Expected only a configured printer name",
            },
            hints: Hints::READ,
        },
    ];
    fn from_source(source: &crate::config::Source<'_>) -> Result<Self> {
        Self::from_source(source)
    }
    fn execute(&mut self, name: &str, args: &Value) -> Result<Value> {
        match name {
            "printer_list" => Ok(json!({"printers":self.config.printers})),
            "printer_status" => Ok(serde_json::to_value(
                self.status(
                    args.get("printer")
                        .and_then(Value::as_str)
                        .ok_or_else(|| error("Missing printer name"))?,
                )?,
            )?),
            "printer_check" => Ok(serde_json::to_value(self.check()?)?),
            _ => Err(error("Unknown operation")),
        }
    }
}

impl crate::device::Monitored for Manager {
    fn active_faults(&self, visit: &mut impl FnMut(&str, &[String])) {
        for (name, saved) in &self.monitor.memory.devices {
            visit(name, &saved.faults);
        }
        // Supply percentages are warnings, separate from IPP hardware faults.
        for (name, saved) in &self.warnings.memory.devices {
            visit(name, &saved.faults);
        }
    }

    type Unavailable = Unavailable;
    type Event = WatchEvent;
    fn poll_seconds(&self) -> u64 {
        self.config.poll_seconds
    }
    fn watch(&mut self) -> Result<fault::Watch<Self::Unavailable, Self::Event>> {
        Manager::watch(self)
    }
}

#[cfg(test)]
#[path = "../tests/support/media.rs"]
#[allow(dead_code)]
mod test_support;

#[cfg(test)]
mod tests {
    use super::*;

    fn temporary() -> Result<test_support::Directory> {
        let path = std::env::temp_dir().join(format!(
            "homewire-printer-unit-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_nanos()
        ));
        std::fs::create_dir(&path)?;
        Ok(test_support::Directory(path))
    }

    fn open_manager(temp: &test_support::Directory) -> Result<Manager> {
        Manager::open(&temp.config(
            "printers.json",
            &json!({"printers":[{"name":"printer","host":"192.0.2.1"}]}),
        )?)
    }
    fn supply(level: Option<i32>) -> Result<Status> {
        let mut attributes = json!({"printer-state":[3],"printer-state-reasons":["none"]});
        if let Some(level) = level {
            attributes["marker-levels"] = json!([level]);
        }
        Status::from_attributes("printer", &serde_json::from_value(attributes)?)
    }

    #[test]
    fn unknown_or_missing_supplies_interrupt_recovery_without_clearing_warnings() -> Result<()> {
        let temp = temporary()?;
        let mut manager = open_manager(&temp)?;
        let low = supply(Some(15))?;
        let full = supply(Some(80))?;
        assert_eq!(low.fault_active, Some(false));
        assert!(
            manager
                .observe_supplies(std::slice::from_ref(&low), 0)?
                .is_empty()
        );
        let events = manager.observe_supplies(&[low], 3)?;
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event, "supply_warning");
        assert!(
            manager
                .observe_supplies(std::slice::from_ref(&full), 10)?
                .is_empty()
        );
        assert!(
            manager
                .observe_supplies(&[supply(Some(-2))?], 39)?
                .is_empty()
        );
        assert_eq!(manager.warnings.active_faults().len(), 1);
        assert!(
            manager
                .observe_supplies(std::slice::from_ref(&full), 40)?
                .is_empty()
        );
        assert!(manager.observe_supplies(&[supply(None)?], 69)?.is_empty());
        assert!(
            manager
                .observe_supplies(std::slice::from_ref(&full), 70)?
                .is_empty()
        );
        assert!(manager.observe_supplies(&[], 99)?.is_empty());
        assert!(
            manager
                .observe_supplies(std::slice::from_ref(&full), 100)?
                .is_empty()
        );
        assert!(
            manager
                .observe_supplies(std::slice::from_ref(&full), 129)?
                .is_empty()
        );
        let events = manager.observe_supplies(std::slice::from_ref(&full), 130)?;
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event, "supply_recovered");
        assert!(events[0].warnings.is_empty());
        assert_eq!(
            events[0].previous_warnings,
            vec!["low_supply_at_or_below_15_percent"]
        );
        assert!(manager.warnings.active_faults().is_empty());
        let json = serde_json::to_value(WatchEvent::Supply(
            events.into_iter().next().expect("event"),
        ))?;
        assert_eq!(json["printer"], "printer");
        assert_eq!(json["supply"], 0);
        assert!(
            !json
                .as_object()
                .expect("supply event")
                .contains_key("faults")
        );
        drop(manager);
        let mut restored = open_manager(&temp)?;
        assert!(restored.observe_supplies(&[full], 0)?.is_empty());
        Ok(())
    }

    #[test]
    fn supply_persistence_failure_does_not_publish_or_lose_confirmed_transition() -> Result<()> {
        let temp = temporary()?;
        let mut manager = open_manager(&temp)?;
        let low = supply(Some(5))?;
        assert!(
            manager
                .observe_supplies(std::slice::from_ref(&low), 0)?
                .is_empty()
        );
        std::fs::create_dir(&manager.warning_path)?;
        assert!(
            manager
                .observe_supplies(std::slice::from_ref(&low), 3)
                .is_err()
        );
        assert!(manager.warnings.active_faults().is_empty());
        std::fs::remove_dir(&manager.warning_path)?;
        assert_eq!(
            manager
                .observe_supplies(std::slice::from_ref(&low), 4)?
                .len(),
            1
        );
        assert_eq!(manager.warnings.active_faults().len(), 1);
        assert!(manager.observe_supplies(&[low], 5)?.is_empty());
        Ok(())
    }
}
