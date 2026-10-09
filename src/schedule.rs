use crate::meaco::Operation;
use crate::{Result, error};
use chrono::{DateTime, Timelike, Utc};
use chrono_tz::Tz;
use serde::Deserialize;
use std::collections::BTreeMap;

/// Minute-resolution clock rules need one wall-clock check per minute, not per second.
/// Recomputed after every request/job so delayed work never replays a missed rule.
pub fn until_next_minute(now: DateTime<Utc>) -> std::time::Duration {
    std::time::Duration::from_nanos(
        60_000_000_000 - u64::from(now.second()) * 1_000_000_000 - u64::from(now.nanosecond()),
    )
}

/// Explicit clock configuration; no household schedule is enabled by omission.
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(deserialize_with = "timezone")]
    pub timezone: Tz,
    #[serde(deserialize_with = "minute")]
    pub day: u16,
    #[serde(deserialize_with = "minute")]
    pub night: u16,
}
fn timezone<'de, D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Tz, D::Error> {
    String::deserialize(d)?
        .parse()
        .map_err(|_| serde::de::Error::custom("Invalid schedule timezone"))
}
fn minute<'de, D: serde::Deserializer<'de>>(d: D) -> std::result::Result<u16, D::Error> {
    let s = String::deserialize(d)?;
    let bytes = s.as_bytes();
    if bytes.len() != 5
        || bytes[2] != b':'
        || ![bytes[0], bytes[1], bytes[3], bytes[4]]
            .iter()
            .all(u8::is_ascii_digit)
    {
        return Err(serde::de::Error::custom("Schedule times must be HH:MM"));
    }
    let hour = u16::from(bytes[0] - b'0') * 10 + u16::from(bytes[1] - b'0');
    let minute = u16::from(bytes[3] - b'0') * 10 + u16::from(bytes[4] - b'0');
    if hour > 23 || minute > 59 {
        return Err(serde::de::Error::custom("Invalid schedule time"));
    }
    Ok(hour * 60 + minute)
}
impl Config {
    pub fn validate(&self) -> Result<()> {
        if self.day >= 1440 || self.night >= 1440 || self.day == self.night {
            return Err(error("Schedule times must be valid and distinct"));
        }
        Ok(())
    }
}
/// No late catch-up or startup writes. Persisted attempts prevent DST replay.
pub fn due(
    config: &Config,
    now: DateTime<Utc>,
    attempts: &BTreeMap<String, String>,
) -> Option<(Operation, String, String)> {
    let local = now.with_timezone(&config.timezone);
    let minute = (local.hour() * 60 + local.minute()) as u16;
    let (operation, name) = if minute == config.day {
        (Operation::Day, "day")
    } else if minute == config.night {
        (Operation::Night, "night")
    } else {
        return None;
    };
    let date = local.date_naive().to_string();
    if attempts.get(name) == Some(&date) {
        return None;
    }
    Some((operation, name.into(), date))
}
