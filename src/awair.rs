//! Read-only Awair Element local API. No cloud account or persistent connection.
use crate::{
    Result,
    config::{private_permissions, read_bounded},
    error, http,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
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
        private_permissions(path)?;
        let config: Self = serde_json::from_slice(&read_bounded(path, "Awair configuration")?)
            .map_err(|_| error("Invalid Awair configuration"))?;
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
