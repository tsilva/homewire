use chrono::{DateTime, Duration, Utc};
use homewire::{
    Result,
    brightness::{Control, Cycle, Io, Observation, Permit, State},
    bulb::Change,
};
use serde_json::json;
use std::time::{Duration as StdDuration, Instant};
#[path = "support/mod.rs"]
mod support;
struct Device {
    level: u8,
    power: bool,
    writes: usize,
    fail_read: bool,
    uncertain: bool,
    change_during_preflight: Option<u8>,
    manual_during_preflight: Option<Control>,
}
impl Device {
    fn new() -> Self {
        Self {
            level: 10,
            power: true,
            writes: 0,
            fail_read: false,
            uncertain: false,
            change_during_preflight: None,
            manual_during_preflight: None,
        }
    }
}
impl Io for Device {
    type Applied = ();
    fn read(&mut self) -> Result<Observation> {
        if self.fail_read {
            return Err(homewire::error("Unknown state"));
        }
        Ok(Observation {
            power: self.power,
            brightness: self.level,
            temperature: None,
        })
    }
    fn apply(&mut self, c: &Change, permit: &Permit) -> Result<()> {
        if let Some(control) = self.manual_during_preflight.take() {
            control.manual(self.level, Utc::now())?;
        }
        if let Some(level) = self.change_during_preflight.take() {
            self.level = level;
        }
        permit.check(self.level)?;
        self.writes += 1;
        self.level = c.brightness_percent.expect("brightness target");
        if self.uncertain {
            return Err(homewire::error("Response lost"));
        }
        Ok(())
    }
}
fn now() -> DateTime<Utc> {
    "2026-10-23T10:00:00Z".parse().expect("fixture")
}
fn cycle(at: DateTime<Utc>, level: u8) -> Cycle {
    Cycle {
        at,
        target: Change {
            brightness_percent: Some(level),
            ..Change::default()
        },
        sunset: None,
        date: at.date_naive().to_string(),
        deadline: Instant::now() + StdDuration::from_secs(10),
    }
}
fn expiry(control: &Control, at: DateTime<Utc>) -> Result<DateTime<Utc>> {
    Ok(control.status(at)?["paused_until"]
        .as_str()
        .expect("expiry")
        .parse()?)
}
#[test]
fn changes_pause_only_one_device_extend_the_timer_and_survive_restart() -> Result<()> {
    let dir = support::Directory::new()?;
    let path = dir.0.join("a.json");
    let at = now();
    let mut a = State::open(&path, "a", false, at)?;
    let mut b = State::open(&dir.0.join("b.json"), "b", false, at)?;
    let mut da = Device::new();
    let mut db = Device::new();
    a.tick(&cycle(at, 50), &mut da)?;
    b.tick(&cycle(at, 50), &mut db)?;
    da.level = 20;
    let changed = at + Duration::seconds(10);
    assert!(a.tick(&cycle(changed, 70), &mut da)?.is_none());
    assert_eq!(expiry(&a.control, changed)?, changed + Duration::hours(2));
    assert_eq!(a.control.status(changed)?["paused"], true);
    b.tick(&cycle(at + Duration::minutes(1), 70), &mut db)?;
    assert_eq!(db.level, 70);
    da.level = 30;
    let changed = at + Duration::minutes(40);
    a.tick(&cycle(changed, 70), &mut da)?;
    let until = changed + Duration::hours(2);
    assert_eq!(expiry(&a.control, changed)?, until);
    let mut a = State::open(&path, "a", false, changed + Duration::minutes(20))?;
    a.tick(&cycle(until - Duration::seconds(1), 80), &mut da)?;
    assert_eq!(
        expiry(&a.control, changed)?,
        until,
        "unchanged readings do not extend override"
    );
    assert_eq!(da.level, 30);
    a.tick(&cycle(until, 90), &mut da)?;
    assert_eq!(
        da.level, 90,
        "expiry applies the current target, including within the same minute"
    );
    assert_eq!(da.writes, 2);
    Ok(())
}
#[test]
fn resume_is_persistent_immediate_and_does_not_wake_an_off_device() -> Result<()> {
    let dir = support::Directory::new()?;
    let path = dir.0.join("tv.json");
    let at = now();
    let mut state = State::open(&path, "tv", false, at)?;
    let mut d = Device::new();
    state.tick(&cycle(at, 50), &mut d)?;
    state.control.manual(20, at + Duration::seconds(1))?;
    d.level = 20;
    state.tick(&cycle(at + Duration::seconds(2), 50), &mut d)?;
    state.control.resume()?;
    let mut state = State::open(&path, "tv", false, at)?;
    assert_eq!(state.control.status(at)?["resume_requested"], true);
    d.power = false;
    state.tick(&cycle(at + Duration::seconds(3), 80), &mut d)?;
    assert_eq!(d.writes, 1);
    assert!(!d.power);
    d.power = true;
    state.tick(&cycle(at + Duration::seconds(4), 80), &mut d)?;
    assert_eq!(d.level, 80);
    assert_eq!(d.writes, 2);
    assert_eq!(state.control.status(at)?["resume_requested"], false);
    Ok(())
}
#[test]
fn manual_requests_reset_countdown_even_if_the_level_is_unchanged() -> Result<()> {
    let dir = support::Directory::new()?;
    let s = State::open(&dir.0.join("tv.json"), "tv", false, now())?;
    s.control.manual(10, now())?;
    let changed = now() + Duration::minutes(30);
    s.control.manual(10, changed)?;
    assert_eq!(expiry(&s.control, changed)?, changed + Duration::hours(2));
    Ok(())
}
#[test]
fn failed_reads_and_our_own_curve_changes_do_not_trigger_an_override() -> Result<()> {
    let dir = support::Directory::new()?;
    let at = now();
    let mut state = State::open(&dir.0.join("tv.json"), "tv", false, at)?;
    let mut d = Device::new();
    state.tick(&cycle(at, 50), &mut d)?;
    d.fail_read = true;
    assert!(
        state
            .tick(&cycle(at + Duration::minutes(1), 60), &mut d)
            .is_err()
    );
    assert_eq!(state.control.status(at)?["paused"], false);
    d.fail_read = false;
    state.tick(&cycle(at + Duration::minutes(2), 70), &mut d)?;
    assert_eq!(d.level, 70);
    assert_eq!(state.control.status(at)?["paused"], false);
    state.control.manual(d.level, at)?;
    d.fail_read = true;
    assert!(
        state
            .tick(&cycle(at + Duration::minutes(3), 80), &mut d)
            .is_err()
    );
    assert_eq!(expiry(&state.control, at)?, at + Duration::hours(2));
    Ok(())
}
#[test]
fn uncertain_automatic_result_is_not_mistaken_for_a_manual_change_after_restart() -> Result<()> {
    let dir = support::Directory::new()?;
    let path = dir.0.join("tv.json");
    let at = now();
    let mut s = State::open(&path, "tv", false, at)?;
    let mut d = Device::new();
    d.uncertain = true;
    assert!(s.tick(&cycle(at, 50), &mut d).is_err());
    let mut s = State::open(&path, "tv", false, at)?;
    d.uncertain = false;
    assert!(
        s.tick(&cycle(at + Duration::seconds(10), 50), &mut d)?
            .is_none()
    );
    assert_eq!(s.control.status(at)?["paused"], false);
    assert_eq!(d.writes, 1);
    d.level = 25;
    s.tick(&cycle(at + Duration::minutes(1), 70), &mut d)?;
    assert_eq!(s.control.status(at)?["paused"], true);
    assert_eq!(d.writes, 1);
    Ok(())
}
#[test]
fn manual_interference_during_preflight_revokes_the_automatic_write() -> Result<()> {
    for direct in [false, true] {
        let dir = support::Directory::new()?;
        let at = now();
        let mut s = State::open(&dir.0.join("tv.json"), "tv", false, at)?;
        let mut d = Device::new();
        s.tick(&cycle(at, 50), &mut d)?;
        if direct {
            d.change_during_preflight = Some(20);
        } else {
            d.manual_during_preflight = Some(s.control.clone());
        }
        assert!(
            s.tick(&cycle(at + Duration::minutes(1), 70), &mut d)?
                .is_none()
        );
        assert_eq!(d.writes, 1);
        assert_eq!(s.control.status(Utc::now())?["paused"], true);
    }
    Ok(())
}
#[test]
fn paused_primary_never_runs_sunset_and_identity_changes_discard_old_overrides() -> Result<()> {
    let dir = support::Directory::new()?;
    let path = dir.0.join("strip.json");
    let at = now();
    let mut s = State::open(&path, "strip", true, at)?;
    let mut d = Device::new();
    d.power = false;
    s.control.manual(d.level, at)?;
    let mut sunset = cycle(at + Duration::seconds(1), 80);
    sunset.sunset = Some(sunset.at);
    s.tick(&sunset, &mut d)?;
    assert_eq!(d.writes, 0);
    assert!(!d.power);
    let replacement = State::open(&path, "replacement", true, at)?;
    assert_eq!(replacement.control.status(at)?["paused"], false);
    assert_eq!(replacement.control.status(at)?["paused_until"], json!(null));
    Ok(())
}
