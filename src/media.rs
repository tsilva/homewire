//! Common small schemas and private configuration loading for media adapters.
use crate::{
    Result,
    config::load_secret_json,
    device::{Field, Input},
    error,
};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use std::path::Path;
pub(crate) fn load<T: DeserializeOwned>(path: &Path) -> Result<T> {
    load_secret_json(path, "Media configuration")
}
pub(crate) fn hex<const N: usize>(text: &str) -> Result<[u8; N]> {
    if text.len() != N * 2 || !text.is_ascii() {
        return Err(error("Invalid hexadecimal credential or identity"));
    }
    let mut bytes = [0; N];
    for (i, b) in bytes.iter_mut().enumerate() {
        *b = u8::from_str_radix(&text[i * 2..i * 2 + 2], 16)
            .map_err(|_| error("Invalid hexadecimal credential or identity"))?;
    }
    Ok(bytes)
}
pub(crate) const POWER: Input = Input::Field {
    field: Field {
        name: "power",
        schema: || json!({"type":"boolean"}),
        valid: Value::is_boolean,
        error: "Expected boolean power",
    },
    shape_error: "Expected only power",
};
pub(crate) fn valid_endpoint(
    port: u16,
    connect: Option<std::net::SocketAddr>,
    timeout: u64,
) -> Result<()> {
    if port == 0 || connect.is_some_and(|a| a.port() == 0) || !(1..=30).contains(&timeout) {
        return Err(error(
            "Media requires nonzero ports and timeout 1–30 seconds",
        ));
    }
    Ok(())
}
