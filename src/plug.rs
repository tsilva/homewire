//! Tapo P100/P110 relays, local P110 metering, and reported overload events.
use crate::{
    Result,
    config::{private_permissions, read_bounded, valid_unique_labels},
    error, fault, klap,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    net::{IpAddr, SocketAddr},
    path::Path,
    thread,
    time::Duration,
};
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Model {
    P100,
    P110,
}
impl Model {
    fn as_str(self) -> &'static str {
        match self {
            Self::P100 => "P100",
            Self::P110 => "P110",
        }
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Endpoint {
    pub name: String,
    pub model: Model,
    pub host: IpAddr,
    pub device_id: String,
    pub auth_hash: String,
    #[serde(default = "port")]
    pub port: u16,
    #[serde(default)]
    pub connect: Option<SocketAddr>,
}
fn port() -> u16 {
    80
}
fn timeout() -> u64 {
    8
}
fn settle() -> u64 {
    2
}
fn poll() -> u64 {
    30
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub plugs: Vec<Endpoint>,
    #[serde(default = "timeout")]
    pub timeout_seconds: u64,
    #[serde(default = "settle")]
    pub settle_seconds: u64,
    #[serde(default = "poll")]
    pub poll_seconds: u64,
}
impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        private_permissions(path)?;
        let config: Self = serde_json::from_slice(&read_bounded(path, "Plug configuration")?)
            .map_err(|_| error("Invalid plug configuration; use the documented schema"))?;
        config.validate()?;
        Ok(config)
    }
    pub fn validate(&self) -> Result<()> {
        if self.plugs.is_empty()
            || self.plugs.len() > 8
            || !(1..=30).contains(&self.timeout_seconds)
            || self.settle_seconds > 10
            || self.timeout_seconds <= self.settle_seconds
            || !(5..=300).contains(&self.poll_seconds)
        {
            return Err(error(
                "Configure 1–8 plugs; timeout 1–30 seconds and greater than settle (0–10); poll 5–300 seconds",
            ));
        }
        if !valid_unique_labels(self.plugs.iter().map(|d| d.name.as_str()), 32, b"-_")
            || !valid_unique_labels(self.plugs.iter().map(|d| d.device_id.as_str()), 64, b"")
        {
            return Err(error(
                "Invalid or duplicate plug name/device identity, or invalid port",
            ));
        }
        for p in &self.plugs {
            if p.port == 0 || p.connect.is_some_and(|a| a.port() == 0) {
                return Err(error(
                    "Invalid or duplicate plug name/device identity, or invalid port",
                ));
            }
            p.key()?;
        }
        Ok(())
    }
}
impl Endpoint {
    fn key(&self) -> Result<[u8; 32]> {
        klap::auth_hash(&self.auth_hash)
    }
    fn identity(&self) -> String {
        format!(
            "{}:{}:{}:{}",
            self.host,
            self.port,
            self.device_id,
            self.model.as_str()
        )
    }
    fn connect(&self, timeout: Duration, delay: Duration) -> Result<NetworkDevice<'_>> {
        let address = SocketAddr::new(self.host, self.port);
        Ok(NetworkDevice {
            endpoint: self,
            client: klap::Client::connect(
                self.connect.unwrap_or(address),
                address.to_string(),
                &self.key()?,
                timeout,
            )?,
            delay,
        })
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Status {
    pub plug: String,
    pub model: Model,
    pub power: bool,
    pub overloaded: Option<bool>,
    pub auto_off_active: Option<bool>,
    pub auto_off_remaining_seconds: Option<u64>,
    pub energy: Option<Energy>,
}
#[derive(Deserialize)]
struct Info {
    model: String,
    device_id: String,
    device_on: bool,
    power_protection_status: Option<String>,
    auto_off_status: Option<String>,
    auto_off_remain_time: Option<u64>,
}
impl Status {
    pub fn from_info(name: &str, id: &str, model: Model, value: Value) -> Result<Self> {
        let info: Info =
            serde_json::from_value(value).map_err(|_| error("Invalid plug device information"))?;
        if info.model != model.as_str() || info.device_id != id {
            return Err(error(
                "Plug model/device identity does not match the private configuration",
            ));
        }
        let overloaded = if model == Model::P110 {
            match info.power_protection_status.as_deref() {
                Some("normal") => Some(false),
                Some("overloaded") => Some(true),
                _ => None,
            }
        } else {
            None
        };
        let auto_off_active = match info.auto_off_status.as_deref() {
            Some("on") => Some(true),
            Some("off") => Some(false),
            _ => None,
        };
        Ok(Self {
            plug: name.to_owned(),
            model,
            power: info.device_on,
            overloaded,
            auto_off_active,
            auto_off_remaining_seconds: info.auto_off_remain_time,
            energy: None,
        })
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Energy {
    pub power_w: Option<f64>,
    pub voltage_v: Option<f64>,
    pub current_a: Option<f64>,
    pub today_kwh: Option<f64>,
    pub month_kwh: Option<f64>,
    pub total_kwh: Option<f64>,
}
fn scaled(value: &Value, key: &str, divisor: f64) -> Result<Option<f64>> {
    match value.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v.as_u64().map(|n| Some(n as f64 / divisor)).ok_or_else(|| {
            error("Invalid plug energy measurement; expected non-negative integer raw units")
        }),
    }
}
impl Energy {
    pub fn from_readings(usage: Value, meter: Value) -> Result<Self> {
        if !usage.is_object() || !meter.is_object() {
            return Err(error("Invalid plug energy response"));
        }
        let result = Self {
            power_w: scaled(&meter, "power_mw", 1000.)?.or(scaled(&usage, "current_power", 1000.)?),
            voltage_v: scaled(&meter, "voltage_mv", 1000.)?,
            current_a: scaled(&meter, "current_ma", 1000.)?,
            today_kwh: scaled(&usage, "today_energy_mwh", 1_000_000.)?.or(scaled(
                &usage,
                "today_energy",
                1000.,
            )?),
            month_kwh: scaled(&usage, "month_energy_mwh", 1_000_000.)?.or(scaled(
                &usage,
                "month_energy",
                1000.,
            )?),
            total_kwh: scaled(&meter, "energy_mwh", 1_000_000.)?.or(scaled(
                &meter,
                "energy_wh",
                1000.,
            )?),
        };
        if result.power_w.is_none()
            && result.voltage_v.is_none()
            && result.current_a.is_none()
            && result.today_kwh.is_none()
            && result.month_kwh.is_none()
            && result.total_kwh.is_none()
        {
            return Err(error("Missing plug energy measurements"));
        }
        Ok(result)
    }
}
pub trait Device {
    fn read(&mut self) -> Result<Status>;
    fn set_power(&mut self, power: bool) -> Result<()>;
    fn settle(&mut self) -> Result<()>;
}
struct NetworkDevice<'a> {
    endpoint: &'a Endpoint,
    client: klap::Client,
    delay: Duration,
}
impl Device for NetworkDevice<'_> {
    fn read(&mut self) -> Result<Status> {
        Status::from_info(
            &self.endpoint.name,
            &self.endpoint.device_id,
            self.endpoint.model,
            self.client.read_info()?,
        )
    }
    fn set_power(&mut self, power: bool) -> Result<()> {
        self.client
            .query("set_device_info", Some(json!({"device_on":power})))?;
        Ok(())
    }
    fn settle(&mut self) -> Result<()> {
        thread::sleep(self.delay);
        Ok(())
    }
}
impl NetworkDevice<'_> {
    fn status(&mut self) -> Result<Status> {
        let mut status = self.read()?;
        if status.model == Model::P110 {
            status.energy = Some(Energy::from_readings(
                self.client.read("get_energy_usage")?,
                self.client.read("get_emeter_data")?,
            )?);
        }
        Ok(status)
    }
}
#[derive(Serialize)]
pub struct Applied {
    pub applied: bool,
    pub status: Status,
}
pub fn apply(device: &mut impl Device, power: bool) -> Result<Applied> {
    let before = device
        .read()
        .map_err(|e| error(format!("Plug pre-control read failed: {e}")))?;
    if before.power == power {
        return Ok(Applied {
            applied: false,
            status: before,
        });
    }
    if power && before.model == Model::P110 && before.overloaded != Some(false) {
        return Err(error(
            "Plug overload status is active or unknown; power-on is blocked",
        ));
    }
    device
        .set_power(power)
        .map_err(|e| error(format!("Plug control failed; no automatic retry: {e}")))?;
    device.settle()?;
    let after = device.read().map_err(|e| {
        error(format!(
            "Plug verification read failed; outcome uncertain, no control retry: {e}"
        ))
    })?;
    if after.power != power
        || power && after.model == Model::P110 && after.overloaded != Some(false)
    {
        return Err(error(
            "Plug verification failed; outcome uncertain, no automatic retry",
        ));
    }
    Ok(Applied {
        applied: true,
        status: after,
    })
}
pub struct Manager {
    pub config: Config,
    pub monitor: fault::Monitor,
    store: fault::Store,
}
pub type Check = fault::Check<Status, Unavailable>;
#[derive(Serialize)]
pub struct Unavailable {
    pub plug: String,
    pub error: String,
}
impl Manager {
    pub fn open(path: &Path) -> Result<Self> {
        let config = Config::load(path)?;
        let (store, monitor) = fault::Store::open(path, &config.plugs)?;
        Ok(Self {
            config,
            monitor,
            store,
        })
    }
    fn endpoint(&self, name: &str) -> Result<&Endpoint> {
        self.config
            .plugs
            .iter()
            .find(|p| p.name == name)
            .ok_or_else(|| error("Unknown configured plug"))
    }
    pub fn list(&self) -> Value {
        json!({"plugs":self.config.plugs.iter().map(|p|json!({"name":p.name,"model":p.model,"host":p.host})).collect::<Vec<_>>()})
    }
    pub fn status(&self, name: &str) -> Result<Status> {
        self.endpoint(name)?
            .connect(
                Duration::from_secs(self.config.timeout_seconds),
                Duration::ZERO,
            )?
            .status()
    }
    pub fn apply(&self, name: &str, power: bool) -> Result<Applied> {
        let mut device = self.endpoint(name)?.connect(
            Duration::from_secs(self.config.timeout_seconds),
            Duration::from_secs(self.config.settle_seconds),
        )?;
        apply(&mut device, power)
    }
    pub fn check(&mut self) -> Result<Check> {
        self.store.check(
            &mut self.monitor,
            &self.config.plugs,
            Duration::from_secs(self.config.timeout_seconds),
        )
    }
    pub fn watch(&mut self) -> Result<fault::Watch<Unavailable>> {
        self.store.watch(
            &mut self.monitor,
            &self.config.plugs,
            Duration::from_secs(self.config.timeout_seconds),
        )
    }
}

impl fault::Target for Endpoint {
    type Status = Status;
    type Unavailable = Unavailable;
    type Event = fault::Event;
    fn name(&self) -> &str {
        &self.name
    }
    fn identity(&self) -> String {
        Endpoint::identity(self)
    }
    fn read(&self, timeout: Duration) -> Result<Status> {
        self.connect(timeout, Duration::ZERO)?.read()
    }
    fn faults(status: &Status) -> fault::Faults<'_> {
        fault::Faults::flag(status.overloaded, "overloaded")
    }
    fn unavailable(&self, error: String) -> Unavailable {
        Unavailable {
            plug: self.name.clone(),
            error,
        }
    }
}
