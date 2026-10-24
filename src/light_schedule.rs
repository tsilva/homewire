//! Optional light curves and sunset rules; no agent/model calls.
use crate::{
    Result,
    bulb::{self, Change, Device},
    error, solar,
};
use chrono::{DateTime, Timelike, Utc};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    path::Path,
    time::{Duration, Instant},
};

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub light: String,
    #[serde(default)]
    pub brightness_lights: Vec<String>,
    #[serde(default)]
    pub follow_tv_backlight: bool,
    #[serde(default)]
    pub brightness_entities: Vec<String>,
    pub timezone: String,
    pub latitude: f64,
    pub longitude: f64,
    pub curve: Vec<(u16, u8, u16)>,
    pub outside_curve: (u8, u16),
}
impl Config {
    pub fn validate(&self) -> Result<Tz> {
        if self.brightness_entities.len() > 8
            || self.brightness_entities.iter().any(|s| {
                s.len() > 80
                    || !s.contains('.')
                    || s.bytes()
                        .any(|c| !c.is_ascii_alphanumeric() && !b"_.-".contains(&c))
            })
        {
            return Err(error("Invalid brightness entity followers"));
        }
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
            json!({"light":self.light,"brightness_lights":self.brightness_lights,"follow_tv_backlight":self.follow_tv_backlight,"brightness_entities":self.brightness_entities,"timezone":self.timezone,"at":now.to_rfc3339(),"target":self.target(now)?,"sunset":self.sunset(now)?.map(|t|t.to_rfc3339()),"curve":self.curve,"outside_curve":self.outside_curve,"note":"Preview only. Primary light follows brightness/temperature while on and turns on at sunset. Followers change only brightness while on. Schedule is opt-in."}),
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
pub use crate::brightness::Memory;
/// Compatibility facade over the common policy; runtime orchestration owns the curve once.
pub struct Controller {
    pub config: Config,
    state: crate::brightness::State,
}
impl Controller {
    pub fn open(config: Config, path: &Path, identity: &str, now: DateTime<Utc>) -> Result<Self> {
        config.validate()?;
        Ok(Self {
            config,
            state: crate::brightness::State::open(path, identity, true, now)?,
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
        let mut value = Self::open(config, path, identity, now)?;
        value.state = crate::brightness::State::open(path, identity, false, now)?;
        Ok(value)
    }
    pub fn state_path(&self) -> &Path {
        self.state.path()
    }
    pub fn tick(
        &mut self,
        now: DateTime<Utc>,
        device: &mut impl Device,
    ) -> Result<Option<bulb::Applied>> {
        let cycle = crate::brightness::Cycle::new(
            &self.config,
            now,
            Instant::now() + Duration::from_secs(60),
        )?;
        self.state.tick(&cycle, &mut bulb::ScheduleIo(device))
    }
}
