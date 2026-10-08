use serde_json::{Value, json};
pub(crate) fn valid_play(v: &Value) -> bool {
    v.as_object().is_some_and(|o| {
        o.len() == 2
            && v["url"]
                .as_str()
                .is_some_and(|s| crate::web::Url::parse(s).is_ok())
            && v["content_type"].as_str().is_some_and(|s| {
                s.len() <= 128
                    && s.contains('/')
                    && s.bytes().all(|b| {
                        b.is_ascii_alphanumeric() || matches!(b, b'/' | b'-' | b'+' | b'.')
                    })
            })
    })
}
pub(crate) fn play_schema() -> Value {
    json!({"type":"object","properties":{"url":{"type":"string","maxLength":2048},"content_type":{"type":"string","maxLength":128}},"required":["url","content_type"],"additionalProperties":false})
}
pub(crate) fn valid_command(v: &Value) -> bool {
    v.as_object().is_some_and(|o| match v["action"].as_str() {
        Some("play" | "pause" | "stop") => o.len() == 1,
        Some("seek") => {
            o.len() == 2
                && v["seconds"]
                    .as_f64()
                    .is_some_and(|n| n.is_finite() && (0.0..=86400.0).contains(&n))
        }
        Some("volume") => o.len() == 2 && v["percent"].as_u64().is_some_and(|n| n <= 100),
        Some("mute") => o.len() == 2 && v["muted"].is_boolean(),
        _ => false,
    })
}
pub(crate) fn command_schema() -> Value {
    json!({"type":"object","properties":{"action":{"type":"string","enum":["play","pause","stop","seek","volume","mute"]},"seconds":{"type":"number","minimum":0,"maximum":86400},"percent":{"type":"integer","minimum":0,"maximum":100},"muted":{"type":"boolean"}},"required":["action"],"additionalProperties":false})
}
