use chrono::{TimeZone, Utc};
use homewire::{meaco::Operation, schedule::due};
use std::collections::BTreeMap;

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
