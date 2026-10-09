//! Read-only Awair Element local API. No cloud account or persistent connection.
use crate::device::{Adapter, Hints, Input, Tool};
use crate::{Result, error, http};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    net::{IpAddr, SocketAddr},
    path::Path,
    time::Duration,
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub host: IpAddr,
    #[serde(default = "port")]
    pub port: u16,
    #[serde(default)]
    pub connect: Option<SocketAddr>,
    #[serde(default = "timeout")]
    pub timeout_seconds: u64,
    #[serde(default = "max_age")]
    pub max_age_seconds: u64,
}
fn port() -> u16 {
    80
}
fn timeout() -> u64 {
    8
}
fn max_age() -> u64 {
    120
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
        if self.port == 0
            || self.connect.is_some_and(|a| a.port() == 0)
            || !(1..=30).contains(&self.timeout_seconds)
            || !(10..=3600).contains(&self.max_age_seconds)
        {
            return Err(error(
                "Awair requires a nonzero port, timeout 1–30 seconds, and max age 10–3600 seconds",
            ));
        }
        Ok(())
    }
    pub fn read(&self) -> Result<Status> {
        self.validate()?;
        let address = SocketAddr::new(self.host, self.port);
        let bytes = http::exchange(
            self.connect.unwrap_or(address),
            &address.to_string(),
            "/air-data/latest",
            Duration::from_secs(self.timeout_seconds),
            "application/json",
            None,
        )?;
        Status::decode(&bytes, Utc::now(), self.max_age_seconds)
    }
}

// Required measurements never default to zero. Ignore estimated/calibration fields
// so CO2 and PM2.5 always come from their actual sensors.
#[derive(Deserialize)]
struct Reading {
    timestamp: String,
    score: u8,
    temp: f64,
    humid: f64,
    co2: f64,
    voc: f64,
    pm25: f64,
}
#[derive(Debug, Serialize)]
pub struct Status {
    pub measured_at: String,
    pub observed_at: String,
    pub age_seconds: f64,
    pub awair_score: u8,
    pub temperature_c: f64,
    pub humidity_percent: f64,
    pub co2_ppm: f64,
    pub tvoc_ppb: f64,
    pub pm25_ug_m3: f64,
}
impl Status {
    pub fn decode(bytes: &[u8], observed: DateTime<Utc>, max_age_seconds: u64) -> Result<Self> {
        if bytes.len() > http::MAX_BODY || !(10..=3600).contains(&max_age_seconds) {
            return Err(error("Invalid Awair response size or maximum age"));
        }
        let reading: Reading = serde_json::from_slice(bytes)
            .map_err(|_| error("Invalid Awair reading; required measurements must be numeric"))?;
        let measured = DateTime::parse_from_rfc3339(&reading.timestamp)
            .map_err(|_| error("Invalid Awair timestamp"))?
            .with_timezone(&Utc);
        let age = observed.signed_duration_since(measured);
        if age > chrono::Duration::seconds(max_age_seconds as i64) {
            return Err(error("Awair reading is stale"));
        }
        if age < chrono::Duration::seconds(-30) {
            return Err(error("Awair device clock is more than 30 seconds ahead"));
        }
        if reading.score > 100
            || !reading.temp.is_finite()
            || !reading.humid.is_finite()
            || !(0.0..=100.0).contains(&reading.humid)
            || [reading.co2, reading.voc, reading.pm25]
                .iter()
                .any(|v| !v.is_finite() || *v < 0.0)
        {
            return Err(error("Invalid Awair measurement range"));
        }
        Ok(Self {
            measured_at: measured.to_rfc3339(),
            observed_at: observed.to_rfc3339(),
            // Retain signed age to expose small clock skew instead of hiding it.
            age_seconds: age.num_milliseconds() as f64 / 1000.0,
            awair_score: reading.score,
            temperature_c: reading.temp,
            humidity_percent: reading.humid,
            co2_ppm: reading.co2,
            tvoc_ppb: reading.voc,
            pm25_ug_m3: reading.pm25,
        })
    }
}

impl Adapter for Config {
    const SUPPORT: crate::entity::Support = crate::entity::Support::READ;
    fn entities<'a>(&'a self, visit: &mut impl FnMut(crate::entity::Entity<'a>)) {
        use crate::{
            device::Family,
            entity::{Capabilities, Domain, Entity},
        };
        let base = Entity::new(
            Family::Awair,
            0,
            Domain::Sensor,
            "awair",
            "Element",
            Capabilities::Read,
        );
        for (suffix, field, unit) in [
            ("_temperature", "temperature_c", "°C"),
            ("_humidity", "humidity_percent", "%"),
            ("_co2", "co2_ppm", "ppm"),
            ("_tvoc", "tvoc_ppb", "ppb"),
            ("_pm25", "pm25_ug_m3", "µg/m³"),
            ("_score", "awair_score", "points"),
        ] {
            visit(base.sensor(suffix, field, unit));
        }
    }
    fn read_device(&mut self, index: usize) -> Result<Value> {
        if index != 0 {
            return Err(error("Unknown Awair device"));
        }
        Ok(serde_json::to_value(self.read()?)?)
    }

    const TOOLS: &'static [Tool] = &[Tool {
        name: "awair_status",
        description: "Read fresh Awair Element measurements: temperature (Celsius), humidity (%), CO2 (ppm), TVOC (ppb), PM2.5 (ug/m3), score, and timestamps. Invalid/stale readings fail. No device controls.",
        input: Input::Empty,
        hints: Hints::READ,
    }];
    fn from_source(source: &crate::config::Source<'_>) -> Result<Self> {
        Self::from_source(source)
    }
    fn execute(&mut self, name: &str, _args: &Value) -> Result<Value> {
        match name {
            "awair_status" => Ok(serde_json::to_value(self.read()?)?),
            _ => Err(error("Unknown Awair operation")),
        }
    }
}
