use chrono::{TimeZone, Utc};
use homewire::{meaco::Operation, schedule::due as due_configured};
use std::collections::BTreeMap;

fn config() -> homewire::schedule::Config {
    serde_json::from_str(r#"{"timezone":"Europe/London","day":"09:00","night":"22:00"}"#)
        .expect("fixture")
}
fn due(
    now: chrono::DateTime<Utc>,
    attempts: &BTreeMap<String, String>,
) -> Option<(Operation, String, String)> {
    due_configured(&config(), now, attempts)
}
#[test]
fn minute_deadlines_sleep_to_the_next_boundary_without_per_second_polling() {
    let start = Utc
        .with_ymd_and_hms(2026, 10, 8, 7, 59, 0)
        .single()
        .expect("date");
    for (nanos, expected) in [
        (0, 60_000_000_000u128),
        (30_500_000_000, 29_500_000_000),
        (59_999_999_999, 1),
    ] {
        let now = start + chrono::Duration::nanoseconds(nanos);
        assert_eq!(
            homewire::schedule::until_next_minute(now).as_nanos(),
            expected
        );
    }
    let due_time = start
        + chrono::Duration::from_std(homewire::schedule::until_next_minute(start))
            .expect("duration");
    assert_eq!(
        due(due_time, &BTreeMap::new()).expect("09:00 London").0,
        Operation::Day
    );
}

#[test]
fn configured_schedule_follows_summer_and_winter_offsets() {
    let attempts = BTreeMap::new();
    for (month, day, hour, minute, expected) in [
        (7, 1, 8, 0, Operation::Day),
        (1, 1, 9, 0, Operation::Day),
        (7, 1, 21, 0, Operation::Night),
        (1, 1, 22, 0, Operation::Night),
    ] {
        let time = Utc
            .with_ymd_and_hms(2026, month, day, hour, minute, 12)
            .single()
            .expect("date");
        assert_eq!(due(time, &attempts).expect("due").0, expected);
    }
}
#[test]
fn late_start_does_not_catch_up() {
    assert!(
        due(
            Utc.with_ymd_and_hms(2026, 1, 1, 9, 1, 0)
                .single()
                .expect("date"),
            &BTreeMap::new()
        )
        .is_none()
    );
}
#[test]
fn persisted_attempt_prevents_same_day_replay() {
    let time = Utc
        .with_ymd_and_hms(2026, 1, 1, 9, 0, 40)
        .single()
        .expect("date");
    let attempts = BTreeMap::from([("day".into(), "2026-01-01".into())]);
    assert!(due(time, &attempts).is_none());
    assert!(due(time + chrono::Duration::days(1), &attempts).is_some());
}

#[test]
fn custom_times_and_timezone_and_dst_replay_are_respected() {
    let cfg: homewire::schedule::Config =
        serde_json::from_str(r#"{"timezone":"America/New_York","day":"01:30","night":"23:00"}"#)
            .expect("fixture");
    cfg.validate().expect("valid");
    let first = Utc
        .with_ymd_and_hms(2026, 11, 1, 5, 30, 0)
        .single()
        .expect("date");
    let repeated = first + chrono::Duration::hours(1);
    let (_, key, date) = due_configured(&cfg, first, &BTreeMap::new()).expect("due");
    assert!(due_configured(&cfg, repeated, &BTreeMap::from([(key, date)])).is_none());
    assert!(due_configured(&cfg, first + chrono::Duration::minutes(1), &BTreeMap::new()).is_none());
}
#[test]
fn ambiguous_or_invalid_schedule_configuration_is_rejected() {
    for value in [
        r#"{"timezone":"bad","day":"09:00","night":"22:00"}"#,
        r#"{"timezone":"UTC","day":"9:00","night":"22:00"}"#,
        r#"{"timezone":"UTC","day":"24:00","night":"22:00"}"#,
    ] {
        assert!(serde_json::from_str::<homewire::schedule::Config>(value).is_err());
    }
    let c: homewire::schedule::Config =
        serde_json::from_str(r#"{"timezone":"UTC","day":"09:00","night":"09:00"}"#)
            .expect("parsed");
    assert!(c.validate().is_err());
}
