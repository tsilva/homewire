use crate::{Result, error};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    net::{IpAddr, SocketAddr},
    path::{Path, PathBuf},
};

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
        private_permissions(path)?;
        let bytes = read_bounded(path, "Device configuration")?;
        // Do not propagate parser diagnostics containing secret values.
        let config: Self = serde_json::from_slice(&bytes).map_err(|_| {
            error("Invalid device configuration; expected the documented JSON schema")
        })?;
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

#[derive(Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Memory {
    pub device_id: String,
    pub base_mode: Option<String>,
    #[serde(default)]
    pub schedule_attempts: std::collections::BTreeMap<String, String>,
}

pub struct Store {
    path: PathBuf,
    device_id: String,
    _lock: File,
}
impl Store {
    pub fn open(config_path: &Path, device_id: &str) -> Result<Self> {
        let lock_path = config_path.with_extension("json.lock");
        let lock = private_options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(lock_path)?;
        lock.try_lock()
            .map_err(|_| error("Another Homewire instance is using this device configuration"))?;
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
        let tmp = self
            .path
            .with_extension(format!("json.tmp-{}", std::process::id()));
        let mut file = private_options().write(true).create_new(true).open(&tmp)?;
        let result = (|| -> Result<()> {
            file.write_all(&serde_json::to_vec(memory)?)?;
            file.sync_all()?;
            fs::rename(&tmp, &self.path)?;
            if let Some(parent) = self.path.parent() {
                File::open(parent)?.sync_all()?;
            }
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&tmp);
        }
        result
    }
}

fn private_options() -> OpenOptions {
    let mut options = OpenOptions::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
}
fn private_permissions(path: &Path) -> Result<()> {
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

fn read_bounded(path: &Path, label: &str) -> std::io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    File::open(path)?.take(8193).read_to_end(&mut bytes)?;
    if bytes.len() > 8192 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("{label} exceeds 8 KiB"),
        ));
    }
    Ok(bytes)
}
