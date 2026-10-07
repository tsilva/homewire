use crate::meaco::Operation;
use chrono::{DateTime, Timelike, Utc};
use chrono_tz::Europe::London;
use std::collections::BTreeMap;

/// Fixed daily clock events, matching HA: no late catch-up or startup changes.
pub fn due(
    now: DateTime<Utc>,
    attempts: &BTreeMap<String, String>,
) -> Option<(Operation, String, String)> {
    let local = now.with_timezone(&London);
    let (operation, name) = match (local.hour(), local.minute()) {
        (9, 0) => (Operation::Day, "day"),
        (22, 0) => (Operation::Night, "night"),
        _ => return None,
    };
    let date = local.date_naive().to_string();
    if attempts.get(name) == Some(&date) {
        return None;
    }
    Some((operation, name.into(), date))
}
