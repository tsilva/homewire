use chrono::{TimeZone, Utc};
use homewire::{meaco::Operation, schedule::due};
use std::collections::BTreeMap;

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
fn lisbon_schedule_follows_summer_and_winter_offsets() {
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
