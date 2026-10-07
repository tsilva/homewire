//! Named printers, read-only status, and durable fault transition monitoring.
use crate::{
    Result,
    config::{private_options, private_permissions, read_bounded},
    error,
    ipp::{self, Attributes},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    net::{IpAddr, SocketAddr},
    path::{Path, PathBuf},
    time::{Duration, Instant},
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
        private_permissions(path)?;
        let config: Self = serde_json::from_slice(&read_bounded(path, "Printer configuration")?)
            .map_err(|_| error("Invalid printer configuration"))?;
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
        let mut names = BTreeSet::new();
        for printer in &self.printers {
            if printer.name.is_empty()
                || printer.name.len() > 32
                || !printer
                    .name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
                || !names.insert(&printer.name)
            {
                return Err(error(
                    "Printer names must be unique, with 1–32 letters, digits, hyphens or underscores",
                ));
            }
            if printer.port == 0
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
        .map(|e| Event {
            printer: e.device,
            event: e.event,
            faults: e.faults,
            previous_faults: e.previous_faults,
        })
    }
}

pub struct Manager {
    pub config: Config,
    pub monitor: Monitor,
    state_path: PathBuf,
    _lock: File,
    started: Instant,
}
#[derive(Serialize)]
pub struct Check {
    pub events: Vec<Event>,
    pub statuses: Vec<Status>,
    pub unavailable: Vec<Unavailable>,
    pub active_faults: BTreeMap<String, Vec<String>>,
}
#[derive(Serialize)]
pub struct Unavailable {
    pub printer: String,
    pub error: String,
}
impl Manager {
    pub fn open(path: &Path) -> Result<Self> {
        let config = Config::load(path)?;
        let lock = private_options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path.with_extension("json.lock"))?;
        lock.try_lock()
            .map_err(|_| error("Another Homewire instance owns this printer configuration"))?;
        let state_path = path.with_extension("state.json");
        let mut memory: Memory = match read_bounded(&state_path, "Printer alert memory") {
            Ok(bytes) => {
                serde_json::from_slice(&bytes).map_err(|_| error("Invalid printer alert memory"))?
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Memory::default(),
            Err(e) => return Err(e.into()),
        };
        memory.devices.retain(|name, saved| {
            config
                .printers
                .iter()
                .any(|p| &p.name == name && p.identity() == saved.endpoint)
        });
        if memory
            .devices
            .values()
            .any(|saved| saved.faults.len() > 64 || saved.faults.iter().any(|f| f.len() > 128))
        {
            return Err(error("Invalid printer alert memory bounds"));
        }
        Ok(Self {
            config,
            monitor: Monitor::from_memory(memory),
            state_path,
            _lock: lock,
            started: Instant::now(),
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
        let original = self.monitor.clone();
        let mut check = Check {
            events: vec![],
            statuses: vec![],
            unavailable: vec![],
            active_faults: BTreeMap::new(),
        };
        for endpoint in &self.config.printers {
            let result = endpoint.read(Duration::from_secs(self.config.timeout_seconds));
            if let Some(event) = self.monitor.observe(
                endpoint,
                result.as_ref().ok(),
                self.started.elapsed().as_secs(),
            ) {
                check.events.push(event);
            }
            match result {
                Ok(status) => check.statuses.push(status),
                Err(e) => check.unavailable.push(Unavailable {
                    printer: endpoint.name.clone(),
                    error: e.to_string(),
                }),
            }
        }
        if !check.events.is_empty()
            && let Err(e) = self.save()
        {
            self.monitor = original;
            return Err(e);
        }
        check.active_faults = self
            .monitor
            .memory
            .devices
            .iter()
            .filter(|(_, saved)| !saved.faults.is_empty())
            .map(|(name, saved)| (name.clone(), saved.faults.clone()))
            .collect();
        Ok(check)
    }
    fn save(&self) -> Result<()> {
        self.monitor.save(&self.state_path)
    }
}
