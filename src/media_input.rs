//! Bounded transport-independent remote input schemas.
use crate::{Result, error};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};

pub(crate) const MAX_HOLD_MS: u64 = 2000;
pub(crate) const MAX_AUDIO: usize = 48_000;
pub(crate) const MAX_BASE64: usize = 64_000;

pub(crate) fn activity_schema() -> Value {
    json!({"type":"object","properties":{"name":{"type":"string","minLength":1,"maxLength":32},"query":{"type":"string","minLength":1,"maxLength":256},"content_id":{"type":"string","minLength":1,"maxLength":64},"input":{"type":"integer","minimum":1,"maximum":4}},"required":["name"],"additionalProperties":false,"maxProperties":2,"description":"Choose a named activity from entity_list. Optional query, content_id or input depends on that activity; each operation sends one key or app link."})
}
pub(crate) fn valid_activity(value: &Value) -> bool {
    value.as_object().is_some_and(|o| {
        (1..=2).contains(&o.len())
            && o.iter().all(|(key, v)| match key.as_str() {
                "name" => bounded_text(v, 32),
                "query" => bounded_text(v, 256),
                "content_id" => bounded_text(v, 64),
                "input" => v.as_u64().is_some_and(|n| (1..=4).contains(&n)),
                _ => false,
            })
            && o.contains_key("name")
    })
}
fn bounded_text(value: &Value, max: usize) -> bool {
    value
        .as_str()
        .is_some_and(|s| !s.trim().is_empty() && s.len() <= max && !s.chars().any(char::is_control))
}

pub(crate) fn hold_schema() -> Value {
    json!({"type":"object","properties":{"entity":{"type":"string","minLength":1,"maxLength":96},"key":{"type":"string","minLength":1,"maxLength":32},"hold_ms":{"type":"integer","minimum":1,"maximum":MAX_HOLD_MS}},"required":["entity","key","hold_ms"],"additionalProperties":false})
}
pub(crate) fn valid_hold(args: &Value) -> bool {
    args.as_object().is_some_and(|o| o.len() == 3)
        && args["entity"]
            .as_str()
            .is_some_and(|s| !s.is_empty() && s.len() <= 96)
        && args["key"]
            .as_str()
            .is_some_and(|s| !s.is_empty() && s.len() <= 32)
        && args["hold_ms"]
            .as_u64()
            .is_some_and(|n| (1..=MAX_HOLD_MS).contains(&n))
}
pub(crate) fn voice_schema() -> Value {
    json!({"type":"string","minLength":4,"maxLength":MAX_BASE64,"description":"Base64 raw signed 16-bit little-endian PCM, mono, 8000 Hz; at most 3 seconds. No WAV header."})
}
pub(crate) fn decode_voice(value: &Value) -> Result<Vec<u8>> {
    let encoded = value
        .as_str()
        .filter(|s| !s.is_empty() && s.len() <= MAX_BASE64)
        .ok_or_else(|| error("Expected base64 PCM audio of at most 3 seconds"))?;
    let audio = STANDARD
        .decode(encoded)
        .map_err(|_| error("Invalid base64 PCM audio"))?;
    if audio.is_empty() || audio.len() > MAX_AUDIO || audio.len() % 2 != 0 {
        return Err(error("Expected 1–24000 signed 16-bit PCM samples"));
    }
    if audio.starts_with(b"RIFF") {
        return Err(error("Send raw PCM samples without a WAV header"));
    }
    Ok(audio)
}
pub(crate) fn valid_voice(value: &Value) -> bool {
    decode_voice(value).is_ok()
}
