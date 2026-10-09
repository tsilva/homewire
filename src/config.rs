use crate::{Result, error};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    net::{IpAddr, SocketAddr},
    path::{Path, PathBuf},
};

fn secret_environment_name(text: &str) -> Option<&str> {
    let name = text.strip_prefix("env:")?;
    (name.starts_with("HOMEWIRE_SECRET_")
        && name.len() <= 128
        && name
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_'))
    .then_some(name)
}

pub(crate) fn environment_secret_path(path: &Path) -> bool {
    path.to_str().and_then(secret_environment_name).is_some()
}

pub(crate) fn resolve_secrets(
    value: &mut Value,
    lookup: &impl Fn(&str) -> Option<String>,
    depth: usize,
) -> Result<()> {
    if depth > 32 {
        return Err(error("Configuration nesting exceeds its limit"));
    }
    match value {
        Value::String(text) if text.starts_with("${HOMEWIRE_SECRET_") => {
            let marker = text.strip_prefix("${").and_then(|s| s.strip_suffix('}'));
            let name = marker
                .filter(|s| {
                    s.len() <= 128
                        && s.bytes()
                            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
                })
                .ok_or_else(|| error("Invalid secret environment reference"))?;
            *text = lookup(name)
                .filter(|s| !s.is_empty() && s.len() <= 8192 && !s.contains('\0'))
                .ok_or_else(|| {
                    error("A required secret environment variable is missing or invalid")
                })?;
        }
        Value::Array(values) => {
            for child in values {
                resolve_secrets(child, lookup, depth + 1)?;
            }
        }
        Value::Object(values) => {
            for child in values.values_mut() {
                resolve_secrets(child, lookup, depth + 1)?;
            }
        }
        _ => {}
    }
    Ok(())
}

pub(crate) fn load_secret_json<T: DeserializeOwned>(path: &Path, label: &str) -> Result<T> {
    private_permissions(path)?;
    let mut value: Value = serde_json::from_slice(&read_bounded(path, label)?)
        .map_err(|_| error("Invalid private JSON configuration"))?;
    resolve_secrets(&mut value, &|name| std::env::var(name).ok(), 0)?;
    serde_json::from_value(value).map_err(|_| error("Invalid private configuration schema"))
}

/// A startup-only configuration input. Adapters decode their own typed schema;
/// the borrowed JSON is never retained by a running device manager.
pub struct Source<'a> {
    path: std::borrow::Cow<'a, Path>,
    value: Option<&'a Value>,
}
impl<'a> Source<'a> {
    pub fn file(path: &'a Path) -> Self {
        Self {
            path: path.into(),
            value: None,
        }
    }
    pub(crate) fn section(root: &Path, family: &str, value: &'a Value) -> Self {
        Self {
            path: root
                .parent()
                .unwrap_or(Path::new("."))
                .join(format!("{family}.json"))
                .into(),
            value: Some(value),
        }
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn decode<T: DeserializeOwned>(&self) -> Result<T> {
        match self.value {
            Some(value) => {
                T::deserialize(value).map_err(|_| error("Invalid private configuration schema"))
            }
            None => load_secret_json(self.path(), "Adapter configuration"),
        }
    }
}

/// Configured groups contain at most eight devices: a stack array avoids
/// allocating sets just to check names and identities during startup.
pub(crate) fn valid_unique_labels<'a>(
    labels: impl Iterator<Item = &'a str>,
    max_length: usize,
    separators: &[u8],
) -> bool {
    let mut seen = [""; 8];
    for (index, label) in labels.enumerate() {
        if index >= seen.len()
            || label.is_empty()
            || label.len() > max_length
            || !label
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || separators.contains(&b))
            || seen[..index].contains(&label)
        {
            return false;
        }
        seen[index] = label;
    }
    true
}

pub(crate) fn save_private(path: &Path, value: &impl Serialize) -> Result<()> {
    save_private_bounded(path, value, 8192)
}
pub(crate) fn save_private_bounded(
    path: &Path,
    value: &impl Serialize,
    limit: usize,
) -> Result<()> {
    let bytes = serde_json::to_vec(value)?;
    if bytes.len() > limit {
        return Err(error("Saved state exceeds its size limit"));
    }
    let tmp = path.with_extension(format!("json.tmp-{}", std::process::id()));
    let mut file = private_options().write(true).create_new(true).open(&tmp)?;
    let result = (|| -> Result<()> {
        file.write_all(&bytes)?;
        file.sync_all()?;
        fs::rename(&tmp, path)?;
        if let Some(parent) = path.parent() {
            File::open(parent)?.sync_all()?;
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

pub(crate) fn lock_configuration(path: &Path) -> Result<File> {
    let lock = private_options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path.with_extension("json.lock"))?;
    lock_file(
        &lock,
        "Another Homewire instance owns this device configuration",
    )?;
    Ok(lock)
}

/// Rust 1.95's standard File lock is unsupported on Android. Use the same
/// kernel flock operation directly there; retaining the File retains ownership.
pub(crate) fn lock_file(file: &File, conflict: &str) -> Result<()> {
    #[cfg(target_os = "android")]
    let result = {
        use std::os::fd::AsRawFd;
        unsafe extern "C" {
            fn flock(fd: std::ffi::c_int, operation: std::ffi::c_int) -> std::ffi::c_int;
        }
        // SAFETY: fd is a live File descriptor; Linux/Bionic LOCK_EX|LOCK_NB
        // constants are 2|4. flock retains no Rust pointers or borrowed storage.
        if unsafe { flock(file.as_raw_fd(), 2 | 4) } == 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error())
        }
    };
    #[cfg(not(target_os = "android"))]
    let result = file.try_lock().map_err(|e| match e {
        std::fs::TryLockError::WouldBlock => std::io::Error::from(std::io::ErrorKind::WouldBlock),
        std::fs::TryLockError::Error(e) => e,
    });
    match result {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => Err(error(conflict)),
        Err(e) => Err(e.into()),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub device_id: String,
    pub host: IpAddr,
    pub local_key: String,
    pub protocol_version: String,
    #[serde(default = "port")]
    pub port: u16,
    #[serde(default = "timeout")]
    pub timeout_seconds: u64,
    #[serde(default = "settle")]
    pub settle_seconds: u64,
    #[serde(default)]
    pub presets: crate::routine::meaco::Presets,
    pub schedule: Option<crate::schedule::Config>,
}
fn port() -> u16 {
    6668
}
fn timeout() -> u64 {
    8
}
fn settle() -> u64 {
    20
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        Self::from_source(&crate::config::Source::file(path))
    }
    pub fn from_source(source: &crate::config::Source<'_>) -> Result<Self> {
        let config: Self = source.decode()?;
        config.presets.validate()?;
        if let Some(schedule) = &config.schedule {
            schedule.validate()?;
            if config.presets.day.is_none() || config.presets.night.is_none() {
                return Err(error("Meaco schedule requires both presets"));
            }
        }
        if config.protocol_version != "3.3" {
            return Err(error("Only Tuya protocol 3.3 is supported"));
        }
        if config.local_key.len() != 16 || !config.local_key.is_ascii() {
            return Err(error("local_key must contain exactly 16 ASCII bytes"));
        }
        if config.device_id.is_empty()
            || config.device_id.len() > 64
            || !config.device_id.is_ascii()
        {
            return Err(error("device_id must contain 1–64 ASCII bytes"));
        }
        if config.port == 0
            || !(1..=30).contains(&config.timeout_seconds)
            || !(1..=60).contains(&config.settle_seconds)
        {
            return Err(error(
                "Invalid port or timeout; settle_seconds must be 1–60",
            ));
        }
        Ok(config)
    }

    pub fn address(&self) -> SocketAddr {
        SocketAddr::new(self.host, self.port)
    }
    pub fn key(&self) -> Result<[u8; 16]> {
        self.local_key
            .as_bytes()
            .try_into()
            .map_err(|_| error("Invalid local key length"))
    }
}

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Memory {
    pub device_id: String,
    pub base_mode: Option<String>,
    #[serde(default)]
    pub schedule_attempts: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    pub faults: crate::fault::Memory,
}

pub struct Store {
    path: PathBuf,
    device_id: String,
    _lock: File,
}
impl Store {
    pub fn open(config_path: &Path, device_id: &str) -> Result<Self> {
        let lock = lock_configuration(config_path)?;
        Ok(Self {
            path: config_path.with_extension("state.json"),
            device_id: device_id.into(),
            _lock: lock,
        })
    }
    pub fn read(&self) -> Result<Memory> {
        match read_bounded(&self.path, "Device state") {
            Ok(bytes) => {
                let memory: Memory = serde_json::from_slice(&bytes)
                    .map_err(|_| error("Invalid saved device state"))?;
                if memory.device_id != self.device_id {
                    return Err(error("Saved state belongs to a different device"));
                }
                if !matches!(
                    memory.base_mode.as_deref(),
                    None | Some("manual" | "purify")
                ) {
                    return Err(error("Invalid saved base mode"));
                }
                Ok(memory)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Memory {
                device_id: self.device_id.clone(),
                ..Memory::default()
            }),
            Err(e) => Err(e.into()),
        }
    }
    pub fn save(&self, memory: &Memory) -> Result<()> {
        save_private(&self.path, memory)
    }
}

pub(crate) fn private_options() -> OpenOptions {
    let mut options = OpenOptions::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
}
pub(crate) fn private_permissions(path: &Path) -> Result<()> {
    if environment_secret_path(path) {
        return Ok(());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if fs::metadata(path)?.permissions().mode() & 0o077 != 0 {
            return Err(error(
                "Device configuration must be private: chmod 600 CONFIG_PATH",
            ));
        }
    }
    Ok(())
}

pub(crate) fn read_bounded(path: &Path, label: &str) -> std::io::Result<Vec<u8>> {
    read_bounded_limit(path, label, 8192)
}
pub(crate) fn read_bounded_limit(
    path: &Path,
    label: &str,
    limit: usize,
) -> std::io::Result<Vec<u8>> {
    if let Some(name) = path.to_str().and_then(secret_environment_name) {
        let value = std::env::var(name)
            .ok()
            .filter(|v| !v.is_empty() && v.len() <= limit && !v.contains('\0'))
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "A required secret environment variable is missing or invalid",
                )
            })?;
        return Ok(value.into_bytes());
    }
    let mut bytes = Vec::new();
    File::open(path)?
        .take((limit + 1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("{label} exceeds its size limit"),
        ));
    }
    Ok(bytes)
}

#[cfg(test)]
mod secret_reference_tests {
    use super::*;
    #[test]
    fn resolves_nested_secrets_without_json_interpolation() {
        let mut value = serde_json::json!({"devices":[{"auth_hash":"${HOMEWIRE_SECRET_TEST}"}]});
        resolve_secrets(&mut value, &|_| Some("quote\" newline\n".into()), 0)
            .expect("resolve valid reference");
        assert_eq!(value["devices"][0]["auth_hash"], "quote\" newline\n");
    }
    #[test]
    fn missing_and_malformed_references_fail_without_disclosure() {
        for marker in ["${HOMEWIRE_SECRET_MISSING}", "${HOMEWIRE_SECRET_BAD;VALUE}"] {
            let mut value = Value::String(marker.into());
            let failure = resolve_secrets(&mut value, &|_| None, 0)
                .expect_err("reject missing reference")
                .to_string();
            assert!(!failure.contains(marker));
        }
    }
    #[test]
    fn unrelated_strings_and_paths_are_literal() {
        let mut value = Value::String("${ordinary_config}".into());
        resolve_secrets(&mut value, &|_| panic!("unexpected lookup"), 0)
            .expect("resolve valid reference");
        assert_eq!(value, "${ordinary_config}");
        assert!(environment_secret_path(Path::new(
            "env:HOMEWIRE_SECRET_PRIVATE_KEY"
        )));
        assert!(!environment_secret_path(Path::new("env:OTHER_VARIABLE")));
    }
}
