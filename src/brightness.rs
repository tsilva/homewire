//! One immutable cycle target and durable per-device execution policy.
use crate::{
    Result,
    bulb::Change,
    config::{read_bounded, save_private},
    error,
    light_schedule::Config,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Instant,
};

const OVERRIDE_HOURS: i64 = 2;
#[derive(Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct OverrideMemory {
    identity: String,
    paused_until: Option<i64>,
    observed: Option<u8>,
    pending: Option<u8>,
    #[serde(default)]
    resume_requested: bool,
}
struct Shared {
    memory: OverrideMemory,
    revision: u64,
}
/// A tiny identity-bound sidecar shared with manual controls; no cached snapshot.
#[derive(Clone)]
pub struct Control {
    shared: Arc<Mutex<Shared>>,
    path: Arc<PathBuf>,
    wake: Arc<AtomicBool>,
}
impl Control {
    fn open(path: &Path, identity: &str) -> Result<Self> {
        let path = path.with_extension("override.json");
        let mut memory: OverrideMemory = match read_bounded(&path, "Brightness override") {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|_| error("Invalid brightness override memory"))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => OverrideMemory::default(),
            Err(e) => return Err(e.into()),
        };
        if memory.identity != identity {
            memory = OverrideMemory {
                identity: identity.into(),
                ..OverrideMemory::default()
            };
        }
        if memory.observed.is_some_and(|v| v > 100)
            || memory.pending.is_some_and(|v| v > 100)
            || memory
                .paused_until
                .is_some_and(|t| DateTime::<Utc>::from_timestamp(t, 0).is_none())
        {
            return Err(error("Invalid brightness override values"));
        }
        Ok(Self {
            wake: Arc::new(AtomicBool::new(memory.resume_requested)),
            shared: Arc::new(Mutex::new(Shared {
                memory,
                revision: 0,
            })),
            path: Arc::new(path),
        })
    }
    fn edit<T>(&self, f: impl FnOnce(&mut OverrideMemory, &mut u64) -> Result<T>) -> Result<T> {
        let mut shared = self
            .shared
            .lock()
            .map_err(|_| error("Brightness override unavailable"))?;
        let mut memory = shared.memory.clone();
        let mut revision = shared.revision;
        let result = f(&mut memory, &mut revision)?;
        if memory != shared.memory {
            save_private(&self.path, &memory)?;
        }
        shared.memory = memory;
        shared.revision = revision;
        Ok(result)
    }
    pub fn manual(&self, level: u8, now: DateTime<Utc>) -> Result<()> {
        if level > 100 {
            return Err(error("Invalid manual brightness"));
        }
        self.edit(|m, r| {
            m.paused_until = Some(now.timestamp() + OVERRIDE_HOURS * 3600);
            m.pending = Some(level);
            m.resume_requested = false;
            *r = r.wrapping_add(1);
            Ok(())
        })
    }
    pub fn resume(&self) -> Result<()> {
        self.edit(|m, r| {
            m.paused_until = None;
            m.observed = None;
            m.pending = None;
            m.resume_requested = true;
            *r = r.wrapping_add(1);
            Ok(())
        })?;
        self.wake.store(true, Ordering::Release);
        Ok(())
    }
    pub fn status(&self, now: DateTime<Utc>) -> Result<serde_json::Value> {
        self.edit(|m, _| {
            Ok(serde_json::json!({
                "paused":m.paused_until.is_some_and(|t| t > now.timestamp()), "paused_until":m.paused_until.and_then(|t| DateTime::<Utc>::from_timestamp(t,0)).map(|t| t.to_rfc3339()),
                "resume_requested":m.resume_requested, "manual_override_seconds":7200
            }))
        })
    }
    pub(crate) fn wake_requested(&self) -> bool {
        self.wake.load(Ordering::Acquire)
    }
    pub(crate) fn take_wake(&self) {
        self.wake.store(false, Ordering::Release);
    }
    fn observe(&self, level: Option<u8>, now: DateTime<Utc>) -> Result<(bool, bool)> {
        self.edit(|m, r| {
            if let Some(level) = level {
                let ours = m.pending == Some(level);
                if !m.resume_requested && !ours && m.observed.is_some_and(|v| v != level) {
                    m.paused_until = Some(now.timestamp() + OVERRIDE_HOURS * 3600);
                    *r = r.wrapping_add(1);
                }
                m.observed = Some(level);
                m.pending = None;
            }
            if m.paused_until.is_some_and(|t| t <= now.timestamp()) {
                m.paused_until = None;
                m.resume_requested = true;
            }
            Ok((
                m.paused_until.is_some_and(|t| t > now.timestamp()),
                m.resume_requested,
            ))
        })
    }
    fn begin(&self, level: u8, now: DateTime<Utc>) -> Result<Option<Permit>> {
        self.edit(|m, r| {
            if m.paused_until.is_some_and(|t| t > now.timestamp()) {
                return Ok(None);
            }
            m.pending = Some(level);
            m.resume_requested = false;
            Ok(Some(Permit {
                control: self.clone(),
                revision: *r,
            }))
        })
    }
    fn matched(&self) -> Result<()> {
        self.edit(|m, _| {
            m.resume_requested = false;
            Ok(())
        })
    }
}

const STATUS: crate::device::Tool = crate::device::Tool {
    name: "brightness_schedule_status",
    description: "Read a scheduled device's persistent manual brightness override and expiry without device I/O.",
    input: crate::device::Input::Named {
        key: "entity",
        extra: None,
        error: "Expected only scheduled entity",
    },
    hints: crate::device::Hints::LIST,
};
const RESUME: crate::device::Tool = crate::device::Tool {
    name: "brightness_schedule_resume",
    description: "Clear this device's manual override and request a fresh current-target assessment now. Does not wake an off device or enable disabled scheduling. No replay of uncertain controls.",
    input: crate::device::Input::Named {
        key: "entity",
        extra: None,
        error: "Expected only scheduled entity",
    },
    hints: crate::device::Hints {
        idempotent: false,
        ..crate::device::Hints::CONTROL
    },
};
pub(crate) fn tools(
    selection: crate::device::Selection,
) -> impl Iterator<Item = &'static crate::device::Tool> + Clone {
    let s = selection.support();
    let enabled =
        s.contains(crate::entity::Support::LIGHT) || s.contains(crate::entity::Support::MEDIA);
    [enabled.then_some(&STATUS), enabled.then_some(&RESUME)]
        .into_iter()
        .flatten()
}
pub(crate) fn call(
    devices: &crate::device::Devices,
    name: &str,
    args: &serde_json::Value,
) -> Result<serde_json::Value> {
    let id = args["entity"]
        .as_str()
        .ok_or_else(|| error("Expected scheduled entity"))?;
    let entity = crate::entity::resolve(devices, id)?;
    if entity.field.is_some() {
        return Err(error("Expected a scheduled physical-device entity"));
    }
    let control = devices
        .brightness_control(entity.device)
        .ok_or_else(|| error("Device has no configured brightness schedule"))?;
    if name == RESUME.name {
        control.resume()?;
    }
    Ok(serde_json::json!({"entity":id,"brightness_schedule":control.status(Utc::now())?}))
}
/// Revoked by a manual request even if an automatic preflight is already running.
pub struct Permit {
    control: Control,
    revision: u64,
}
#[derive(Debug)]
struct Interrupted;
impl std::fmt::Display for Interrupted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Manual brightness override; scheduled control cancelled")
    }
}
impl std::error::Error for Interrupted {}
impl Permit {
    pub fn check(&self, level: u8) -> Result<()> {
        let valid = self.control.edit(|m, r| {
            if *r != self.revision {
                return Ok(false);
            }
            if m.observed.is_some_and(|v| v != level) {
                m.paused_until = Some(Utc::now().timestamp() + OVERRIDE_HOURS * 3600);
                m.observed = Some(level);
                m.pending = None;
                *r = r.wrapping_add(1);
                return Ok(false);
            }
            Ok(m.paused_until.is_none())
        })?;
        if valid {
            Ok(())
        } else {
            Err(Box::new(Interrupted))
        }
    }
    fn confirmed(&self, level: u8) -> Result<()> {
        self.control.edit(|m, r| {
            if *r == self.revision {
                m.observed = Some(level);
                m.pending = None;
            }
            Ok(())
        })
    }
}

#[derive(Clone)]
pub struct Cycle {
    pub at: DateTime<Utc>,
    pub target: Change,
    pub sunset: Option<DateTime<Utc>>,
    pub date: String,
    pub deadline: Instant,
}
impl Cycle {
    pub fn new(config: &Config, at: DateTime<Utc>, deadline: Instant) -> Result<Self> {
        Ok(Self {
            at,
            target: config.target(at)?,
            sunset: config.sunset(at)?,
            date: at
                .with_timezone(&config.validate()?)
                .date_naive()
                .to_string(),
            deadline,
        })
    }
    pub fn fresh(&self) -> bool {
        Instant::now() < self.deadline
    }
}
pub struct Observation {
    pub power: bool,
    pub brightness: u8,
    pub temperature: Option<u16>,
}
pub trait Io {
    type Applied;
    fn read(&mut self) -> Result<Observation>;
    fn apply(&mut self, change: &Change, permit: &Permit) -> Result<Self::Applied>;
}
#[derive(Default, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Memory {
    pub identity: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sunset_attempt: Option<String>,
    pub last_attempt_minute: Option<i64>,
}
pub struct State {
    pub memory: Memory,
    pub control: Control,
    path: PathBuf,
    last_check: DateTime<Utc>,
    last_power: Option<bool>,
    primary: bool,
}
impl State {
    pub fn open(path: &Path, identity: &str, primary: bool, now: DateTime<Utc>) -> Result<Self> {
        let mut memory: Memory = match read_bounded(path, "Brightness schedule memory") {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|_| error("Invalid brightness schedule memory"))?,
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
            control: Control::open(path, identity)?,
            memory,
            path: path.to_owned(),
            last_check: now,
            last_power: None,
            primary,
        })
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn tick<D: Io>(&mut self, cycle: &Cycle, device: &mut D) -> Result<Option<D::Applied>> {
        if !cycle.fresh() {
            return Ok(None);
        }
        let sunset_due = self.primary
            && cycle.sunset.is_some_and(|t| {
                self.last_check < t
                    && cycle.at >= t
                    && cycle.at - t <= chrono::Duration::seconds(60)
            })
            && self.memory.sunset_attempt.as_deref() != Some(&cycle.date);
        let previous_minute = self.last_check.timestamp().div_euclid(60);
        let minute = cycle.at.timestamp().div_euclid(60);
        self.last_check = cycle.at;
        if sunset_due {
            let mut memory = self.memory.clone();
            memory.sunset_attempt = Some(cycle.date.clone());
            save_private(&self.path, &memory)?;
            self.memory = memory;
        }
        let status = match device.read() {
            Ok(s) => s,
            Err(e) => {
                self.last_power = None;
                return Err(e);
            }
        };
        let startup_or_on = self.last_power != Some(true) && status.power;
        self.last_power = Some(status.power);
        let (paused, resume) = self
            .control
            .observe(status.power.then_some(status.brightness), cycle.at)?;
        if paused {
            return Ok(None);
        }
        if !sunset_due
            && (!status.power
                || !resume
                    && (!startup_or_on && minute == previous_minute
                        || self.memory.last_attempt_minute == Some(minute)))
        {
            return Ok(None);
        }
        let mut target = cycle.target.clone();
        if !self.primary {
            target.color_temperature_k = None;
        }
        if sunset_due {
            target.power = Some(true);
        }
        let matching = target.power.is_none_or(|v| v == status.power)
            && target.brightness_percent == Some(status.brightness)
            && target
                .color_temperature_k
                .is_none_or(|v| Some(v) == status.temperature);
        if matching || !cycle.fresh() {
            if matching {
                self.control.matched()?;
            }
            return Ok(None);
        }
        let mut memory = self.memory.clone();
        memory.last_attempt_minute = Some(minute);
        save_private(&self.path, &memory)?;
        self.memory = memory;
        if !cycle.fresh() {
            return Ok(None);
        }
        let level = status.brightness;
        let wanted = target
            .brightness_percent
            .ok_or_else(|| error("Missing brightness target"))?;
        let Some(permit) = self.control.begin(wanted, cycle.at)? else {
            return Ok(None);
        };
        let result = match permit
            .check(level)
            .and_then(|_| device.apply(&target, &permit))
        {
            Ok(result) => result,
            Err(e) if e.downcast_ref::<Interrupted>().is_some() => return Ok(None),
            Err(e) => return Err(e),
        };
        permit.confirmed(wanted)?;
        Ok(Some(result))
    }
}
