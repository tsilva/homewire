//! Optional L900 biorhythm and sunset rules; no agent/model calls.
use crate::{
    Result,
    bulb::{self, Change, Device},
    config::{read_bounded, save_private},
    error, solar,
};
use chrono::{DateTime, Timelike, Utc};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

pub const POINTS: &[(u16, u8, u16)] = &[
    (360, 10, 2700),
    (480, 80, 5000),
    (960, 80, 5000),
    (1200, 10, 2700),
];
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub light: String,
    pub timezone: String,
    pub latitude: f64,
    pub longitude: f64,
}
impl Config {
    pub fn validate(&self) -> Result<Tz> {
        if !self.latitude.is_finite()
            || !self.longitude.is_finite()
            || self.latitude.abs() > 89.0
            || self.longitude.abs() > 180.0
        {
            return Err(error("Invalid light schedule location"));
        }
        self.timezone
            .parse()
            .map_err(|_| error("Invalid light schedule timezone"))
    }
    pub fn target(&self, now: DateTime<Utc>) -> Result<Change> {
        let local = now.with_timezone(&self.validate()?);
        let minute =
            f64::from(local.hour() * 60 + local.minute()) + f64::from(local.second()) / 60.0;
        let (brightness, temperature) = curve(minute);
        Ok(Change {
            brightness_percent: Some(brightness),
            color_temperature_k: Some(temperature),
            ..Change::default()
        })
    }
    pub fn sunset(&self, now: DateTime<Utc>) -> Result<Option<DateTime<Utc>>> {
        let date = now.with_timezone(&self.validate()?).date_naive();
        Ok(solar::sunset(date, self.latitude, self.longitude))
    }
    pub fn preview(&self, now: DateTime<Utc>) -> Result<Value> {
        Ok(
            json!({"light":self.light,"timezone":self.timezone,"at":now.to_rfc3339(),"target":self.target(now)?,"sunset":self.sunset(now)?.map(|t|t.to_rfc3339()),"curve":POINTS,"note":"Preview only. Brightness/temperature follow the curve while on; sunset turns on once. Schedule is opt-in."}),
        )
    }
}
pub fn curve(minute: f64) -> (u8, u16) {
    for pair in POINTS.windows(2) {
        let (start, b0, k0) = pair[0];
        let (end, b1, k1) = pair[1];
        if (f64::from(start)..f64::from(end)).contains(&minute) {
            let ratio = (minute - f64::from(start)) / f64::from(end - start);
            return (
                (f64::from(b0) + (f64::from(b1) - f64::from(b0)) * ratio).round_ties_even() as u8,
                (f64::from(k0) + (f64::from(k1) - f64::from(k0)) * ratio).round_ties_even() as u16,
            );
        }
    }
    (10, 2700)
}
#[derive(Default, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Memory {
    pub identity: String,
    pub sunset_attempt: Option<String>,
    pub last_attempt_minute: Option<i64>,
}
pub struct Controller {
    pub config: Config,
    pub memory: Memory,
    path: PathBuf,
    last_check: DateTime<Utc>,
    last_power: Option<bool>,
}
impl Controller {
    pub fn open(config: Config, path: &Path, identity: &str, now: DateTime<Utc>) -> Result<Self> {
        config.validate()?;
        let mut memory: Memory = match read_bounded(path, "Light schedule memory") {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|_| error("Invalid light schedule memory"))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Memory::default(),
            Err(e) => return Err(e.into()),
        };
        if memory.identity != identity {
            memory = Memory {
                identity: identity.to_owned(),
                ..Memory::default()
            };
        }
        Ok(Self {
            config,
            memory,
            path: path.to_owned(),
            last_check: now,
            last_power: None,
        })
    }
    pub fn tick(
        &mut self,
        now: DateTime<Utc>,
        device: &mut impl Device,
    ) -> Result<Option<bulb::Applied>> {
        let sunset = self.config.sunset(now)?;
        let date = now
            .with_timezone(&self.config.validate()?)
            .date_naive()
            .to_string();
        let sunset_due = sunset.is_some_and(|t| {
            self.last_check < t && now >= t && now - t <= chrono::Duration::seconds(60)
        }) && self.memory.sunset_attempt.as_deref() != Some(&date);
        let previous_minute = self.last_check.timestamp().div_euclid(60);
        let minute = now.timestamp().div_euclid(60);
        self.last_check = now;
        // Persist a sunset attempt before even querying: failures cannot replay it.
        if sunset_due {
            let mut memory = self.memory.clone();
            memory.sunset_attempt = Some(date);
            save_private(&self.path, &memory)?;
            self.memory = memory;
        }
        let status = device.read()?;
        let light_on = self.last_power == Some(false) && status.power;
        let startup = self.last_power.is_none();
        self.last_power = Some(status.power);
        if !sunset_due && (!status.power || !startup && !light_on && minute == previous_minute) {
            return Ok(None);
        }
        if !sunset_due && self.memory.last_attempt_minute == Some(minute) {
            return Ok(None);
        }
        let mut target = self.config.target(now)?;
        if sunset_due {
            target.power = Some(true);
        }
        if target.matches(&status) {
            return Ok(None);
        }
        let mut memory = self.memory.clone();
        memory.last_attempt_minute = Some(minute);
        save_private(&self.path, &memory)?;
        self.memory = memory;
        Ok(Some(bulb::apply(device, &target)?))
    }
}
