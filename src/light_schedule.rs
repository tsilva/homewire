//! Optional light curves and sunset rules; no agent/model calls.
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

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub light: String,
    #[serde(default)]
    pub brightness_lights: Vec<String>,
    pub timezone: String,
    pub latitude: f64,
    pub longitude: f64,
    pub curve: Vec<(u16, u8, u16)>,
    pub outside_curve: (u8, u16),
}
impl Config {
    pub fn validate(&self) -> Result<Tz> {
        if self.brightness_lights.len() > 7
            || !crate::config::valid_unique_labels(
                std::iter::once(self.light.as_str())
                    .chain(self.brightness_lights.iter().map(String::as_str)),
                32,
                b"_-",
            )
        {
            return Err(error(
                "Configure up to seven distinct brightness follower lights",
            ));
        }
        let valid_target = |b: u8, k: u16| (1..=100).contains(&b) && (2500..=9000).contains(&k);
        if !(2..=16).contains(&self.curve.len())
            || !valid_target(self.outside_curve.0, self.outside_curve.1)
            || self
                .curve
                .iter()
                .any(|&(m, b, k)| m >= 1440 || !valid_target(b, k))
            || self.curve.windows(2).any(|p| p[0].0 >= p[1].0)
        {
            return Err(error(
                "Invalid light curve: 2–16 ordered minute/brightness/temperature points required",
            ));
        }
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
        let (brightness, temperature) = curve(&self.curve, self.outside_curve, minute);
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
            json!({"light":self.light,"brightness_lights":self.brightness_lights,"timezone":self.timezone,"at":now.to_rfc3339(),"target":self.target(now)?,"sunset":self.sunset(now)?.map(|t|t.to_rfc3339()),"curve":self.curve,"outside_curve":self.outside_curve,"note":"Preview only. Primary light follows brightness/temperature while on and turns on at sunset. Followers change only brightness while on. Schedule is opt-in."}),
        )
    }
}
pub fn curve(points: &[(u16, u8, u16)], outside: (u8, u16), minute: f64) -> (u8, u16) {
    for pair in points.windows(2) {
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
    outside
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
    brightness_only: bool,
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
            brightness_only: false,
        })
    }
    pub fn open_brightness(
        mut config: Config,
        light: &str,
        path: &Path,
        identity: &str,
        now: DateTime<Utc>,
    ) -> Result<Self> {
        config.light = light.to_owned();
        config.brightness_lights.clear();
        let mut controller = Self::open(config, path, identity, now)?;
        controller.brightness_only = true;
        Ok(controller)
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
        let sunset_due = !self.brightness_only
            && sunset.is_some_and(|t| {
                self.last_check < t && now >= t && now - t <= chrono::Duration::seconds(60)
            })
            && self.memory.sunset_attempt.as_deref() != Some(&date);
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
        if self.brightness_only {
            target.color_temperature_k = None;
        }
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
