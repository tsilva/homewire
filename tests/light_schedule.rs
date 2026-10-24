use chrono::{DateTime, Duration, Utc};
use homewire::{
    Result,
    bulb::{Change, Device, Model, Status},
    light_schedule::{Config, Controller},
    solar,
};
use serde_json::json;
use std::path::PathBuf;
const POINTS: &[(u16, u8, u16)] = &[
    (360, 10, 2700),
    (480, 80, 5000),
    (960, 80, 5000),
    (1200, 10, 2700),
];
fn curve(minute: f64) -> (u8, u16) {
    homewire::light_schedule::curve(POINTS, (10, 2700), minute)
}
fn at(value: &str) -> DateTime<Utc> {
    value
        .parse()
        .expect("valid fixture or expected schedule action")
}
fn config() -> Config {
    Config {
        light: "strip".into(),
        brightness_lights: vec![],
        follow_tv_backlight: false,
        brightness_entities: vec![],
        timezone: "Europe/London".into(),
        latitude: 51.5,
        longitude: -0.12,
        curve: POINTS.to_vec(),
        outside_curve: (10, 2700),
    }
}
#[path = "support/mod.rs"]
mod support;
struct Temp(support::Directory);
impl Temp {
    fn new() -> Self {
        Self(support::Directory::new().expect("fixture directory"))
    }
    fn state(&self) -> PathBuf {
        self.0.0.join("state.json")
    }
}
struct Fake {
    state: Status,
    sets: usize,
    fail_read: bool,
    fail_set: bool,
}
impl Fake {
    fn new(power: bool) -> Self {
        Self {state:Status::from_model_info("strip","syntheticStrip",Model::L900,json!({"model":"L900","device_id":"syntheticStrip","device_on":power,"brightness":5,"hue":0,"saturation":0,"color_temp":2500,"color_temp_range":[2500,9000],"overheated":false,"lighting_effect":{"enable":0},"music_rhythm_enable":false})).expect("valid fixture or expected schedule action"),sets:0,fail_read:false,fail_set:false}
    }
}
impl Device for Fake {
    fn read(&mut self) -> Result<Status> {
        if self.fail_read {
            Err(homewire::error("offline"))
        } else {
            Ok(self.state.clone())
        }
    }
    fn set(&mut self, c: &Change) -> Result<()> {
        self.sets += 1;
        if self.fail_set {
            return Err(homewire::error("uncertain"));
        }
        if let Some(v) = c.power {
            self.state.power = v;
        }
        if let Some(v) = c.brightness_percent {
            self.state.brightness_percent = v;
        }
        if let Some(v) = c.color_temperature_k {
            self.state.color_temperature_k = v;
        }
        Ok(())
    }
    fn settle(&mut self) -> Result<()> {
        Ok(())
    }
}
#[test]
fn sunset_matches_official_noaa_calculator_fixtures_and_utc_rollover() {
    // Fixtures calculated with NOAA's published main.js, not this implementation.
    for (date, lat, lon, expected) in [
        ("2026-06-21", 40.71, -74.01, "2026-06-22T00:30:46Z"),
        ("2026-12-21", -33.87, 151.21, "2026-12-21T09:05:26Z"),
        ("2026-03-29", 51.5, -0.12, "2026-03-29T18:28:39Z"),
    ] {
        assert_eq!(
            solar::sunset(
                date.parse()
                    .expect("valid fixture or expected schedule action"),
                lat,
                lon
            ),
            Some(at(expected))
        );
    }
    assert!(
        solar::sunset(
            "2026-06-21"
                .parse()
                .expect("valid fixture or expected schedule action"),
            80.,
            0.
        )
        .is_none()
    );
    assert!(
        solar::sunset(
            "2026-12-21"
                .parse()
                .expect("valid fixture or expected schedule action"),
            80.,
            0.
        )
        .is_none()
    );
    assert!(
        solar::sunset(
            "2026-06-21"
                .parse()
                .expect("valid fixture or expected schedule action"),
            f64::NAN,
            0.
        )
        .is_none()
    );
}
#[test]
fn synthetic_curve_points_interpolation_and_ties_even() {
    for &(minute, b, k) in POINTS {
        assert_eq!(curve(f64::from(minute)), (b, k));
    }
    assert_eq!(curve(420.), (45, 3850));
    assert_eq!(curve(1080.), (45, 3850));
    assert_eq!(curve(366.), (14, 2815));
    assert_eq!(curve(378.), (20, 3045));
    for minute in [0., 359., 1200., 1439.] {
        assert_eq!(curve(minute), (10, 2700));
    }
}
#[test]
fn curve_uses_local_time_across_dst() -> Result<()> {
    let c = config();
    assert_eq!(
        c.target(at("2026-01-01T08:00:00Z"))?.brightness_percent,
        Some(80)
    );
    assert_eq!(
        c.target(at("2026-07-01T07:00:00Z"))?.brightness_percent,
        Some(80)
    );
    assert_eq!(
        c.target(at("2026-03-29T06:00:00Z"))?.brightness_percent,
        Some(45)
    );
    let mut c = config();
    c.timezone = "Invalid/Zone".into();
    assert!(c.validate().is_err());
    c.timezone = "UTC".into();
    c.latitude = 90.;
    assert!(c.validate().is_err());
    Ok(())
}
#[test]
fn off_stays_off_startup_and_external_on_apply_curve_and_matching_state_skips() -> Result<()> {
    let t = Temp::new();
    let now = at("2026-10-07T10:00:00Z");
    let mut c = Controller::open(config(), &t.state(), "strip1", now)?;
    let mut d = Fake::new(false);
    assert!(c.tick(now, &mut d)?.is_none());
    assert_eq!(d.sets, 0);
    d.state.power = true;
    assert!(
        c.tick(now + Duration::seconds(10), &mut d)?
            .expect("valid fixture or expected schedule action")
            .applied
    );
    assert_eq!(d.state.brightness_percent, 80);
    assert_eq!(d.state.color_temperature_k, 5000);
    assert!(c.tick(now + Duration::minutes(1), &mut d)?.is_none());
    assert_eq!(d.sets, 1);
    let mut c = Controller::open(config(), &t.state(), "strip1", now + Duration::minutes(2))?;
    d.state.brightness_percent = 5;
    assert!(c.tick(now + Duration::minutes(2), &mut d)?.is_none());
    assert_eq!(
        d.sets, 1,
        "manual brightness survives restart and pauses this device"
    );
    Ok(())
}
#[test]
fn sunset_turns_on_once_and_does_not_replay_after_restart() -> Result<()> {
    let t = Temp::new();
    let sunset = config()
        .sunset(at("2026-10-07T12:00:00Z"))?
        .expect("valid fixture or expected schedule action");
    let mut c = Controller::open(
        config(),
        &t.state(),
        "strip1",
        sunset - Duration::seconds(20),
    )?;
    let mut d = Fake::new(false);
    assert!(c.tick(sunset - Duration::seconds(10), &mut d)?.is_none());
    assert!(
        c.tick(sunset + Duration::seconds(5), &mut d)?
            .expect("valid fixture or expected schedule action")
            .applied
    );
    assert!(d.state.power);
    assert_eq!(d.sets, 1);
    d.state.power = false;
    assert!(c.tick(sunset + Duration::seconds(20), &mut d)?.is_none());
    let mut c = Controller::open(
        config(),
        &t.state(),
        "strip1",
        sunset - Duration::seconds(20),
    )?;
    assert!(c.tick(sunset + Duration::seconds(30), &mut d)?.is_none());
    assert_eq!(d.sets, 1);
    Ok(())
}
#[test]
fn sunset_is_not_suppressed_by_prior_curve_change_in_same_minute() -> Result<()> {
    let t = Temp::new();
    let sunset = config()
        .sunset(at("2026-03-29T12:00:00Z"))?
        .expect("valid fixture or expected schedule action");
    assert_eq!(
        (sunset - Duration::seconds(4)).timestamp() / 60,
        (sunset + Duration::seconds(4)).timestamp() / 60,
        "fixture must exercise both writes in the same minute"
    );
    let mut c = Controller::open(
        config(),
        &t.state(),
        "strip1",
        sunset - Duration::seconds(5),
    )?;
    let mut d = Fake::new(true);
    d.state.brightness_percent = 40;
    assert!(c.tick(sunset - Duration::seconds(4), &mut d)?.is_some());
    d.state.power = false;
    assert!(c.tick(sunset + Duration::seconds(4), &mut d)?.is_some());
    assert!(d.state.power);
    assert_eq!(d.sets, 2);
    Ok(())
}
#[test]
fn missed_sunset_and_start_after_sunset_do_not_catch_up() -> Result<()> {
    let sunset = config()
        .sunset(at("2026-10-07T12:00:00Z"))?
        .expect("valid fixture or expected schedule action");
    for start in [
        sunset - Duration::seconds(10),
        sunset + Duration::seconds(1),
    ] {
        let t = Temp::new();
        let mut c = Controller::open(config(), &t.state(), "strip1", start)?;
        let mut d = Fake::new(false);
        assert!(c.tick(sunset + Duration::seconds(61), &mut d)?.is_none());
        assert_eq!(d.sets, 0);
    }
    Ok(())
}
#[test]
fn failed_sunset_read_or_write_is_retained_and_not_retried() -> Result<()> {
    let sunset = config()
        .sunset(at("2026-10-07T12:00:00Z"))?
        .expect("valid fixture or expected schedule action");
    for read_failure in [true, false] {
        let t = Temp::new();
        let mut c = Controller::open(
            config(),
            &t.state(),
            "strip1",
            sunset - Duration::seconds(20),
        )?;
        let mut d = Fake::new(false);
        d.fail_read = read_failure;
        d.fail_set = !read_failure;
        assert!(c.tick(sunset + Duration::seconds(1), &mut d).is_err());
        assert_eq!(d.sets, usize::from(!read_failure));
        d.fail_read = false;
        d.fail_set = false;
        assert!(c.tick(sunset + Duration::seconds(20), &mut d)?.is_none());
        assert!(!d.state.power);
        let mut c = Controller::open(
            config(),
            &t.state(),
            "strip1",
            sunset - Duration::seconds(20),
        )?;
        assert!(c.tick(sunset + Duration::seconds(30), &mut d)?.is_none());
        assert_eq!(d.sets, usize::from(!read_failure));
    }
    Ok(())
}
#[test]
fn persistence_failure_blocks_controls_and_same_minute_failure_survives_restart() -> Result<()> {
    let t = Temp::new();
    let now = at("2026-10-07T10:00:00Z");
    let mut c = Controller::open(config(), &t.0.0.join("missing/state.json"), "strip1", now)?;
    let mut d = Fake::new(true);
    assert!(c.tick(now, &mut d).is_err());
    assert_eq!(d.sets, 0);
    let mut c = Controller::open(config(), &t.state(), "strip1", now)?;
    d.fail_set = true;
    assert!(c.tick(now, &mut d).is_err());
    assert_eq!(d.sets, 1);
    d.fail_set = false;
    let mut c = Controller::open(config(), &t.state(), "strip1", now)?;
    assert!(c.tick(now + Duration::seconds(10), &mut d)?.is_none());
    assert_eq!(d.sets, 1);
    // A new minute is a fresh curve evaluation, never a blind retransmission.
    assert!(c.tick(now + Duration::minutes(1), &mut d)?.is_some());
    assert_eq!(d.sets, 2);
    Ok(())
}
#[test]
fn optional_schedule_config_is_validated_and_server_flag_requires_control() -> Result<()> {
    let base = json!({"lights":[{"name":"strip","model":"L900","host":"192.0.2.1","device_id":"synthetic","auth_hash":"00".repeat(32)}],"automation":{"light":"strip","timezone":"Europe/London","latitude":51.5,"longitude":-0.12,"curve":POINTS,"outside_curve":[10,2700]}});
    let config: homewire::bulb::Config = serde_json::from_value(base.clone())?;
    config.validate()?;
    for (key, value) in [
        ("light", json!("missing")),
        ("timezone", json!("invalid")),
        ("latitude", json!(91)),
    ] {
        let mut b = base.clone();
        b["automation"][key] = value;
        let config: homewire::bulb::Config = serde_json::from_value(b)?;
        assert!(config.validate().is_err());
    }
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_homewire"))
        .args(["serve", "--light-schedule"])
        .output()?;
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("--allow-control"));
    Ok(())
}

#[test]
fn custom_curve_and_outside_target_are_not_legacy_defaults() -> Result<()> {
    let mut c = config();
    c.curve = vec![(600, 10, 3000), (660, 90, 5000)];
    c.outside_curve = (20, 2700);
    assert_eq!(
        c.target(at("2026-01-01T10:30:00Z"))?.brightness_percent,
        Some(50)
    );
    assert_eq!(
        c.target(at("2026-01-01T09:00:00Z"))?.color_temperature_k,
        Some(2700)
    );
    for points in [
        vec![],
        vec![(600, 10, 3000), (600, 90, 5000)],
        vec![(1440, 10, 3000), (1441, 90, 5000)],
        vec![(600, 0, 3000), (660, 90, 5000)],
        vec![(600, 10, 2000), (660, 90, 5000)],
    ] {
        c.curve = points;
        assert!(c.validate().is_err());
    }
    Ok(())
}

#[test]
fn brightness_followers_preserve_colour_and_power_even_at_sunset() -> Result<()> {
    let t = Temp::new();
    let sunset = config()
        .sunset(at("2026-10-07T12:00:00Z"))?
        .expect("sunset");
    let mut c = Controller::open_brightness(
        config(),
        "lamp",
        &t.state(),
        "lamp1",
        sunset - Duration::seconds(10),
    )?;
    let mut d = Fake::new(false);
    d.state.model = Model::L430C;
    d.state.temperature_range_k = [2500, 6500];
    d.state.color_temperature_k = 2700;
    d.state.hue = 135;
    d.state.saturation_percent = 42;
    assert!(c.tick(sunset - Duration::seconds(5), &mut d)?.is_none());
    assert!(c.tick(sunset + Duration::seconds(5), &mut d)?.is_none());
    assert_eq!(d.sets, 0, "sunset must not turn on followers");
    d.state.power = true;
    let now = sunset + Duration::seconds(15);
    assert!(c.tick(now, &mut d)?.is_some());
    assert_eq!(
        d.state.brightness_percent,
        config()
            .target(now)?
            .brightness_percent
            .expect("brightness")
    );
    assert_eq!(d.state.color_temperature_k, 2700);
    assert_eq!(d.state.hue, 135);
    assert_eq!(d.state.saturation_percent, 42);
    d.state.power = false;
    assert!(c.tick(now + Duration::minutes(1), &mut d)?.is_none());
    assert!(!d.state.power);
    Ok(())
}

#[test]
fn followers_have_independent_durable_attempts_and_share_local_curve() -> Result<()> {
    let t = Temp::new();
    let now = at("2026-07-01T06:00:00Z");
    let mut config = config();
    config.brightness_lights = vec!["lamp_a".into(), "lamp_b".into()];
    let mut a = Controller::open_brightness(config.clone(), "lamp_a", &t.state(), "a", now)?;
    let b_path = t.0.0.join("b.state.json");
    let mut b = Controller::open_brightness(config.clone(), "lamp_b", &b_path, "b", now)?;
    let mut da = Fake::new(true);
    let mut db = Fake::new(true);
    da.fail_set = true;
    assert!(a.tick(now, &mut da).is_err());
    assert!(b.tick(now, &mut db)?.is_some());
    assert_eq!(db.state.brightness_percent, 45, "07:00 local summer time");
    let mut a = Controller::open_brightness(config, "lamp_a", &t.state(), "a", now)?;
    da.fail_set = false;
    assert!(a.tick(now + Duration::seconds(10), &mut da)?.is_none());
    assert_eq!(da.sets, 1, "uncertain write cannot replay after restart");
    assert!(a.tick(now + Duration::minutes(1), &mut da)?.is_some());
    Ok(())
}

#[test]
fn follower_configuration_requires_distinct_configured_bounded_targets() -> Result<()> {
    let base = json!({"lights":[
        {"name":"strip","model":"L900","host":"192.0.2.1","device_id":"syntheticStrip","auth_hash":"00".repeat(32)},
        {"name":"lamp","model":"L430C","host":"192.0.2.2","device_id":"syntheticLamp","auth_hash":"00".repeat(32)}
    ],"automation":{"light":"strip","brightness_lights":["lamp"],"timezone":"UTC","latitude":0,"longitude":0,"curve":POINTS,"outside_curve":[10,2700]}});
    let config: homewire::bulb::Config = serde_json::from_value(base.clone())?;
    config.validate()?;
    assert_eq!(
        config
            .automation
            .as_ref()
            .expect("automation")
            .preview(at("2026-01-01T12:00:00Z"))?["brightness_lights"],
        json!(["lamp"])
    );
    for targets in [
        json!(["missing"]),
        json!(["lamp", "lamp"]),
        json!(["strip"]),
        json!(["../lamp"]),
        json!(["a", "b", "c", "d", "e", "f", "g", "h"]),
    ] {
        let mut value = base.clone();
        value["automation"]["brightness_lights"] = targets;
        let config: homewire::bulb::Config = serde_json::from_value(value)?;
        assert!(config.validate().is_err());
    }
    Ok(())
}

#[test]
fn shared_policy_preserves_legacy_tv_attempts_and_never_replays_uncertain_writes() -> Result<()> {
    use homewire::brightness::{Cycle, Io, Observation, State};
    use std::time::{Duration as StdDuration, Instant};
    struct Tv(usize);
    impl Io for Tv {
        type Applied = ();
        fn read(&mut self) -> Result<Observation> {
            Ok(Observation {
                power: true,
                brightness: 1,
                temperature: None,
            })
        }
        fn apply(&mut self, _: &Change, _: &homewire::brightness::Permit) -> Result<()> {
            self.0 += 1;
            Err(homewire::error("Lost response; outcome unknown"))
        }
    }
    let t = Temp::new();
    let now = at("2026-10-23T10:00:00Z");
    let minute = now.timestamp().div_euclid(60);
    support::Directory::write_private(
        &t.state(),
        &serde_json::to_vec(&json!({
            "identity":"tv", "last_attempt_minute":minute
        }))?,
    )?;
    let mut target = Cycle::new(&config(), now, Instant::now() + StdDuration::from_secs(10))?;
    let mut tv = Tv(0);
    let mut state = State::open(&t.state(), "tv", false, now)?;
    assert!(state.tick(&target, &mut tv)?.is_none());
    assert_eq!(
        tv.0, 0,
        "old TV attempt must still suppress same-minute replay"
    );
    target.at += Duration::minutes(1);
    assert!(state.tick(&target, &mut tv).is_err());
    let memory: serde_json::Value = serde_json::from_slice(&std::fs::read(t.state())?)?;
    assert_eq!(
        memory,
        json!({"identity":"tv", "last_attempt_minute":minute + 1})
    );
    let mut restarted = State::open(&t.state(), "tv", false, target.at)?;
    assert!(restarted.tick(&target, &mut tv)?.is_none());
    assert_eq!(tv.0, 1, "unknown result must survive owner restart");
    Ok(())
}
