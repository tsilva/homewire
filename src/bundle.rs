//! One bounded startup document. Adapter schemas and ownership remain static.
use crate::{
    Result,
    config::{self, Source},
    device::Devices,
    error,
};
use serde::Deserialize;
use serde_json::{Map, Value};
use std::{
    fs::File,
    net::SocketAddr,
    path::{Path, PathBuf},
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    version: u32,
    #[serde(default)]
    adapters: Map<String, Value>,
    #[serde(default)]
    routines: Map<String, Value>,
    #[serde(default)]
    automations: Map<String, Value>,
    #[serde(default)]
    services: Map<String, Value>,
    #[serde(default)]
    deployment: Option<Value>,
}
pub struct Bundle {
    path: PathBuf,
    document: Document,
    lock: File,
}
impl Bundle {
    /// Detect the legacy Meaco schema without resolving secrets twice.
    pub fn load(path: &Path) -> Result<Option<Self>> {
        config::private_permissions(path)?;
        let value: Value = serde_json::from_slice(&config::read_bounded_limit(
            path,
            "Homewire configuration",
            65536,
        )?)
        .map_err(|_| error("Invalid private JSON configuration"))?;
        let unified = [
            "version",
            "adapters",
            "routines",
            "automations",
            "services",
            "deployment",
        ]
        .iter()
        .any(|key| value.get(key).is_some());
        if !unified {
            return Ok(None);
        }
        let mut document: Document = serde_json::from_value(value)
            .map_err(|_| error("Invalid unified configuration schema"))?;
        if document.version != 1 {
            return Err(error("Unsupported Homewire configuration version"));
        }
        if document.deployment.as_ref().is_some_and(|v| !v.is_object()) {
            return Err(error("Deployment must be an object"));
        }
        // Host-only installation settings never become resident runtime settings.
        document.deployment = None;
        for (section, entries) in [
            ("adapters", &document.adapters),
            ("services", &document.services),
        ] {
            for (family, value) in entries {
                if !value.is_object()
                    || (family != "api" || section != "services")
                        && Devices::config_section(catalog_name(family)) != Some(section)
                {
                    return Err(error("Unknown adapter/service family or invalid section"));
                }
            }
        }
        if document.adapters.contains_key("lights") && document.adapters.contains_key("bulbs") {
            return Err(error("Configure lights once"));
        }
        for (family, value) in &document.routines {
            if !value.is_object() || !matches!(family.as_str(), "meaco" | "tv") {
                return Err(error("Unknown routine or invalid section"));
            }
        }
        for (family, value) in &document.automations {
            if !value.is_object() || !matches!(family.as_str(), "meaco" | "lights") {
                return Err(error("Unknown automation or invalid section"));
            }
        }
        move_setting(
            &mut document.adapters,
            &mut document.routines,
            "meaco",
            "meaco",
            "presets",
        )?;
        move_setting(
            &mut document.adapters,
            &mut document.automations,
            "meaco",
            "meaco",
            "schedule",
        )?;
        let lights = if document.adapters.contains_key("bulbs") {
            "bulbs"
        } else {
            "lights"
        };
        move_setting(
            &mut document.adapters,
            &mut document.automations,
            lights,
            "lights",
            "automation",
        )?;
        for entries in [
            &mut document.adapters,
            &mut document.services,
            &mut document.routines,
        ] {
            for value in entries.values_mut() {
                config::resolve_secrets(value, &|name| std::env::var(name).ok(), 0)?;
            }
        }
        Ok(Some(Self {
            path: path.into(),
            document,
            lock: config::lock_configuration(path)?,
        }))
    }
    pub fn open_devices(&self, connect: Option<SocketAddr>) -> Result<Devices> {
        let mut devices = Devices::default();
        for (family, value) in self
            .document
            .adapters
            .iter()
            .chain(self.document.services.iter())
        {
            if family == "api" {
                continue;
            }
            let source = Source::section(&self.path, state_name(family), value);
            if family == "meaco" {
                devices.meaco = Some(crate::meaco::Manager::from_source_with_connect(
                    &source, connect,
                )?);
            } else {
                devices.open_source(catalog_name(family), &source)?;
            }
        }
        if connect.is_some() && devices.meaco.is_none() {
            return Err(error("--connect requires Meaco"));
        }
        if let Some(value) = self.document.routines.get("tv") {
            devices.tv = Some(crate::tv::Manager::from_children(
                &Source::section(&self.path, "tv", value),
                devices
                    .samsung
                    .take()
                    .ok_or_else(|| error("TV requires Samsung configuration"))?,
                devices
                    .digi
                    .take()
                    .ok_or_else(|| error("TV requires DIGI configuration"))?,
                devices
                    .plugs
                    .take()
                    .ok_or_else(|| error("TV requires plugs configuration"))?,
            )?);
        }
        devices.validate()?;
        Ok(devices)
    }
    pub fn api_settings(&self) -> Result<Option<crate::api::Settings>> {
        self.document
            .services
            .get("api")
            .map(|v| crate::api::Settings::from_source(&Source::section(&self.path, "api", v)))
            .transpose()
    }
    /// Drop all temporary JSON while keeping exclusive root ownership.
    pub fn into_lock(self) -> File {
        self.lock
    }
}
fn catalog_name(family: &str) -> &str {
    if family == "lights" { "bulbs" } else { family }
}
fn state_name(family: &str) -> &str {
    if family == "bulbs" { "lights" } else { family }
}
fn move_setting(
    adapters: &mut Map<String, Value>,
    settings: &mut Map<String, Value>,
    adapter: &str,
    family: &str,
    field: &str,
) -> Result<()> {
    if let Some(value) = settings.remove(family) {
        let config = adapters
            .get_mut(adapter)
            .and_then(Value::as_object_mut)
            .ok_or_else(|| error("Routine/automation requires its configured adapter"))?;
        if config.insert(field.into(), value).is_some() {
            return Err(error("Routine/automation configured more than once"));
        }
    }
    Ok(())
}
