//! Named Tapo L430C/L900 lights, verified controls and retained overheating faults.
use crate::device::{Adapter, Field, Hints, Input, Tool};
use crate::{
    Result,
    config::{private_permissions, read_bounded, valid_unique_labels},
    error, fault, klap, light_schedule,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    net::{IpAddr, SocketAddr},
    path::Path,
    thread,
    time::Duration,
};

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum Model {
    #[default]
    L430C,
    L900,
}
impl Model {
    fn max_temperature(self) -> u16 {
        match self {
            Self::L430C => 6500,
            Self::L900 => 9000,
        }
    }
    fn as_str(self) -> &'static str {
        match self {
            Self::L430C => "L430C",
            Self::L900 => "L900",
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Endpoint {
    pub name: String,
    #[serde(default)]
    pub model: Model,
    pub host: IpAddr,
    pub device_id: String,
    pub auth_hash: String,
    #[serde(default = "port")]
    pub port: u16,
    #[serde(default)]
    pub connect: Option<SocketAddr>,
}
fn port() -> u16 {
    80
}
fn timeout() -> u64 {
    8
}
fn settle() -> u64 {
    2
}
fn poll() -> u64 {
    30
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(alias = "lights")]
    pub bulbs: Vec<Endpoint>,
    #[serde(default = "timeout")]
    pub timeout_seconds: u64,
    #[serde(default = "settle")]
    pub settle_seconds: u64,
    #[serde(default = "poll")]
    pub poll_seconds: u64,
    #[serde(default)]
    pub automation: Option<light_schedule::Config>,
}
impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        private_permissions(path)?;
        let config: Self = serde_json::from_slice(&read_bounded(path, "Bulb configuration")?)
            .map_err(|_| error("Invalid bulb configuration; use the documented schema"))?;
        config.validate()?;
        Ok(config)
    }
    pub fn validate(&self) -> Result<()> {
        if self.bulbs.is_empty()
            || self.bulbs.len() > 8
            || !(1..=30).contains(&self.timeout_seconds)
            || self.settle_seconds > 10
            || self.timeout_seconds <= self.settle_seconds
            || !(5..=300).contains(&self.poll_seconds)
        {
            return Err(error(
                "Configure 1–8 bulbs; timeout 1–30 seconds and greater than settle (0–10); poll 5–300 seconds",
            ));
        }
        if !valid_unique_labels(self.bulbs.iter().map(|d| d.name.as_str()), 32, b"-_")
            || !valid_unique_labels(self.bulbs.iter().map(|d| d.device_id.as_str()), 64, b"")
        {
            return Err(error(
                "Invalid or duplicate bulb name/device identity, or invalid port",
            ));
        }
        for bulb in &self.bulbs {
            if bulb.port == 0 || bulb.connect.is_some_and(|a| a.port() == 0) {
                return Err(error(
                    "Invalid or duplicate bulb name/device identity, or invalid port",
                ));
            }
            bulb.key()?;
        }
        if let Some(automation) = &self.automation {
            automation.validate()?;
            if self.poll_seconds > 30 {
                return Err(error("Light automation requires poll_seconds of 5–30"));
            }
            if !self
                .bulbs
                .iter()
                .any(|b| b.name == automation.light && b.model == Model::L900)
            {
                return Err(error("Light automation requires a configured L900 target"));
            }
        }
        Ok(())
    }
}
impl Endpoint {
    fn key(&self) -> Result<[u8; 32]> {
        klap::auth_hash(&self.auth_hash)
    }
    fn identity(&self) -> String {
        let identity = format!("{}:{}:{}", self.host, self.port, self.device_id);
        if self.model == Model::L900 {
            format!("{identity}:L900")
        } else {
            identity
        }
    }
    fn connect(&self, timeout: Duration) -> Result<NetworkDevice<'_>> {
        let address = SocketAddr::new(self.host, self.port);
        Ok(NetworkDevice {
            endpoint: self,
            delay: Duration::ZERO,
            client: klap::Client::connect(
                self.connect.unwrap_or(address),
                address.to_string(),
                &self.key()?,
                timeout,
            )?,
        })
    }
}

// Preserve the public aliases while keeping presentation out of device behavior.
pub use crate::naming::{canonical_tool, light_arguments, light_output};
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Status {
    pub bulb: String,
    pub model: Model,
    pub power: bool,
    pub brightness_percent: u8,
    pub hue: u16,
    pub saturation_percent: u8,
    pub color_temperature_k: u16,
    pub temperature_range_k: [u16; 2],
    pub overheated: Option<bool>,
    pub effect_active: Option<bool>,
}
#[derive(Deserialize)]
struct Effect {
    enable: u8,
}
#[derive(Deserialize)]
struct Info {
    model: String,
    device_id: String,
    device_on: bool,
    brightness: u8,
    hue: u16,
    saturation: u8,
    color_temp: u16,
    color_temp_range: [u16; 2],
    overheated: Option<bool>,
    lighting_effect: Option<Effect>,
    dynamic_light_effect_enable: Option<bool>,
    music_rhythm_enable: Option<bool>,
}
impl Status {
    pub fn from_info(name: &str, device_id: &str, value: Value) -> Result<Self> {
        Self::from_model_info(name, device_id, Model::L430C, value)
    }
    pub fn from_model_info(
        name: &str,
        device_id: &str,
        model: Model,
        value: Value,
    ) -> Result<Self> {
        let info: Info = serde_json::from_value(value)
            .map_err(|_| error("Incomplete or invalid bulb status"))?;
        let [min, max] = info.color_temp_range;
        if info.model != model.as_str() || info.device_id != device_id {
            return Err(error("Bulb model or identity does not match configuration"));
        }
        if !(1..=100).contains(&info.brightness)
            || info.hue > 360
            || info.saturation > 100
            || min < 2500
            || max > model.max_temperature()
            || min > max
            || info.color_temp != 0 && !(min..=max).contains(&info.color_temp)
        {
            return Err(error("Invalid bulb measurement range"));
        }
        let effect = match info.lighting_effect {
            Some(Effect { enable: 0 }) => Some(false),
            Some(Effect { enable: 1 }) => Some(true),
            Some(_) => return Err(error("Invalid light effect status")),
            None => info.dynamic_light_effect_enable,
        };
        let effect_active = match (effect, info.music_rhythm_enable) {
            (Some(true), _) | (_, Some(true)) => Some(true),
            (Some(false), Some(false)) => Some(false),
            _ => None,
        };
        Ok(Self {
            bulb: name.to_owned(),
            model,
            power: info.device_on,
            brightness_percent: info.brightness,
            hue: info.hue,
            saturation_percent: info.saturation,
            color_temperature_k: info.color_temp,
            temperature_range_k: info.color_temp_range,
            overheated: info.overheated,
            effect_active,
        })
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct Change {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub power: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub brightness_percent: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hue: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub saturation_percent: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color_temperature_k: Option<u16>,
}
impl Change {
    pub fn validate(&self) -> Result<()> {
        if self.power.is_none()
            && self.brightness_percent.is_none()
            && self.hue.is_none()
            && self.saturation_percent.is_none()
            && self.color_temperature_k.is_none()
            || self
                .brightness_percent
                .is_some_and(|v| !(1..=100).contains(&v))
            || self.hue.is_some_and(|v| v > 360)
            || self.saturation_percent.is_some_and(|v| v > 100)
            || self.hue.is_some() != self.saturation_percent.is_some()
            || self
                .color_temperature_k
                .is_some_and(|v| !(2500..=9000).contains(&v))
            || self.color_temperature_k.is_some() && self.hue.is_some()
        {
            return Err(error(
                "Specify absolute power/brightness (1–100), paired hue (0–360)/saturation (0–100), or white temperature (2500–9000 K within the light's range); do not mix colour and white",
            ));
        }
        Ok(())
    }
    pub fn matches(&self, status: &Status) -> bool {
        self.power.is_none_or(|v| v == status.power)
            && self
                .brightness_percent
                .is_none_or(|v| v == status.brightness_percent)
            && self
                .hue
                .is_none_or(|v| status.color_temperature_k == 0 && v == status.hue)
            && self
                .saturation_percent
                .is_none_or(|v| v == status.saturation_percent)
            && self
                .color_temperature_k
                .is_none_or(|v| v == status.color_temperature_k)
    }
    fn params(&self) -> Value {
        let mut value = json!({});
        if let Some(v) = self.power {
            value["device_on"] = json!(v);
        }
        if let Some(v) = self.brightness_percent {
            value["brightness"] = json!(v);
        }
        if let Some(v) = self.hue {
            value["hue"] = json!(v);
            value["color_temp"] = json!(0);
        }
        if let Some(v) = self.saturation_percent {
            value["saturation"] = json!(v);
        }
        if let Some(v) = self.color_temperature_k {
            value["color_temp"] = json!(v);
        }
        value
    }
    fn off_only(&self) -> bool {
        self.power == Some(false)
            && self.brightness_percent.is_none()
            && self.hue.is_none()
            && self.color_temperature_k.is_none()
    }
}
pub trait Device {
    fn read(&mut self) -> Result<Status>;
    fn set(&mut self, change: &Change) -> Result<()>;
    fn settle(&mut self) -> Result<()>;
}
struct NetworkDevice<'a> {
    endpoint: &'a Endpoint,
    client: klap::Client,
    delay: Duration,
}
impl Device for NetworkDevice<'_> {
    fn read(&mut self) -> Result<Status> {
        Status::from_model_info(
            &self.endpoint.name,
            &self.endpoint.device_id,
            self.endpoint.model,
            self.client.read_info()?,
        )
    }
    fn set(&mut self, change: &Change) -> Result<()> {
        self.client
            .query("set_device_info", Some(change.params()))?;
        Ok(())
    }
    fn settle(&mut self) -> Result<()> {
        thread::sleep(self.delay);
        Ok(())
    }
}
#[derive(Serialize)]
pub struct Applied {
    pub applied: bool,
    pub status: Status,
}
pub fn apply(device: &mut impl Device, change: &Change) -> Result<Applied> {
    change.validate()?;
    let before = device
        .read()
        .map_err(|e| error(format!("Light pre-control read failed: {e}")))?;
    if before.model == Model::L900
        && before.effect_active != Some(false)
        && (change.brightness_percent.is_some()
            || change.hue.is_some()
            || change.color_temperature_k.is_some())
    {
        return Err(error(
            "L900 effect status is active or unknown; disable effects/music mode in the Tapo app before fixed light settings",
        ));
    }
    if change.color_temperature_k.is_some_and(|v| {
        !(before.temperature_range_k[0]..=before.temperature_range_k[1]).contains(&v)
    }) {
        return Err(error(
            "Requested temperature is outside the bulb's reported range",
        ));
    }
    if change.matches(&before) {
        return Ok(Applied {
            applied: false,
            status: before,
        });
    }
    if before.overheated != Some(false) && !change.off_only() {
        return Err(error(
            "Bulb reports overheating or its fault status is unknown; only power-off is allowed",
        ));
    }
    device
        .set(change)
        .map_err(|e| error(format!("Light control failed (no automatic retry): {e}")))?;
    device.settle()?;
    let after = device.read().map_err(|e| {
        error(format!(
            "Light verification read failed (outcome uncertain): {e}"
        ))
    })?;
    if before.model == Model::L900
        && (change.brightness_percent.is_some()
            || change.hue.is_some()
            || change.color_temperature_k.is_some())
        && after.effect_active != Some(false)
    {
        return Err(error(
            "L900 effect state changed during verification; no automatic retry",
        ));
    }
    if !change.matches(&after) {
        return Err(error(
            "Bulb verification failed; outcome may be uncertain, no automatic retry",
        ));
    }
    Ok(Applied {
        applied: true,
        status: after,
    })
}

pub struct Manager {
    pub config: Config,
    pub monitor: fault::Monitor,
    store: fault::Store,
    pub scheduler: Option<light_schedule::Controller>,
}
pub type Check = fault::Check<Status, Unavailable>;
#[derive(Serialize)]
pub struct Unavailable {
    pub bulb: String,
    pub error: String,
}
impl Manager {
    pub fn open(path: &Path) -> Result<Self> {
        let config = Config::load(path)?;
        let (store, monitor) = fault::Store::open(path, &config.bulbs)?;
        let scheduler = if let Some(automation) = &config.automation {
            let endpoint = config
                .bulbs
                .iter()
                .find(|b| b.name == automation.light)
                .ok_or_else(|| error("Missing scheduled light"))?;
            Some(light_schedule::Controller::open(
                automation.clone(),
                &path.with_extension("schedule.state.json"),
                &endpoint.identity(),
                chrono::Utc::now(),
            )?)
        } else {
            None
        };
        Ok(Self {
            config,
            scheduler,
            monitor,
            store,
        })
    }
    fn endpoint(&self, name: &str) -> Result<&Endpoint> {
        self.config
            .bulbs
            .iter()
            .find(|b| b.name == name)
            .ok_or_else(|| error("Unknown configured bulb"))
    }
    pub fn list(&self) -> Value {
        json!({"bulbs":self.config.bulbs.iter().map(|b|json!({"name":b.name,"host":b.host,"model":b.model})).collect::<Vec<_>>()})
    }
    pub fn status(&self, name: &str) -> Result<Status> {
        self.endpoint(name)?
            .connect(Duration::from_secs(self.config.timeout_seconds))?
            .read()
    }
    pub fn apply(&self, name: &str, change: &Change) -> Result<Applied> {
        change.validate()?;
        let mut device = self
            .endpoint(name)?
            .connect(Duration::from_secs(self.config.timeout_seconds))?;
        device.delay = Duration::from_secs(self.config.settle_seconds);
        apply(&mut device, change)
    }
    pub fn schedule_tick(&mut self) -> Result<Option<Applied>> {
        let mut scheduler = self
            .scheduler
            .take()
            .ok_or_else(|| error("No light automation configured"))?;
        let result = (|| {
            let mut device = self
                .endpoint(&scheduler.config.light)?
                .connect(Duration::from_secs(self.config.timeout_seconds))?;
            device.delay = Duration::from_secs(self.config.settle_seconds);
            scheduler.tick(chrono::Utc::now(), &mut device)
        })();
        self.scheduler = Some(scheduler);
        result
    }
    pub fn check(&mut self) -> Result<Check> {
        self.store.check(
            &mut self.monitor,
            &self.config.bulbs,
            Duration::from_secs(self.config.timeout_seconds),
        )
    }
    pub fn watch(&mut self) -> Result<fault::Watch<Unavailable>> {
        self.store.watch(
            &mut self.monitor,
            &self.config.bulbs,
            Duration::from_secs(self.config.timeout_seconds),
        )
    }
}

impl fault::Target for Endpoint {
    type Status = Status;
    type Unavailable = Unavailable;
    type Event = fault::Event;
    fn name(&self) -> &str {
        &self.name
    }
    fn identity(&self) -> String {
        Endpoint::identity(self)
    }
    fn read(&self, timeout: Duration) -> Result<Status> {
        self.connect(timeout)?.read()
    }
    fn faults(status: &Status) -> fault::Faults<'_> {
        fault::Faults::flag(status.overheated, "overheated")
    }
    fn unavailable(&self, error: String) -> Unavailable {
        Unavailable {
            bulb: self.name.clone(),
            error,
        }
    }
}

impl Adapter for Manager {
    const SUPPORT: crate::entity::Support =
        crate::entity::Support::LIGHT.union(crate::entity::Support::CONFIGURE);
    fn entities<'a>(&'a self, visit: &mut impl FnMut(crate::entity::Entity<'a>)) {
        use crate::{
            device::Family,
            entity::{Capabilities, Domain, Entity},
        };
        for (index, endpoint) in self.config.bulbs.iter().enumerate() {
            visit(
                Entity::new(
                    Family::Bulbs,
                    index,
                    Domain::Light,
                    &endpoint.name,
                    endpoint.model.as_str(),
                    Capabilities::Light {
                        max_temperature_k: endpoint.model.max_temperature(),
                        fixed_settings_require_effects_off: endpoint.model == Model::L900,
                    },
                )
                .configurable(LIGHT_SETTINGS),
            );
        }
    }
    fn read_device(&mut self, index: usize) -> Result<Value> {
        let name = &self
            .config
            .bulbs
            .get(index)
            .ok_or_else(|| error("Unknown light"))?
            .name;
        Ok(serde_json::to_value(self.status(name)?)?)
    }
    fn options(&mut self, index: usize) -> Result<Value> {
        let endpoint = self
            .config
            .bulbs
            .get(index)
            .ok_or_else(|| error("Unknown light"))?;
        let mut device = endpoint.connect(Duration::from_secs(self.config.timeout_seconds))?;
        device.read()?;
        let presets = light_presets(&mut device.client)?;
        let fade = device.client.read("get_on_off_gradually_info")?;
        Ok(
            json!({"presets":presets,"transitions":fade,"effect_control":"disable or restore the device's currently stored effect"}),
        )
    }
    fn control_device(
        &mut self,
        index: usize,
        command: crate::entity::Command,
        args: &Value,
    ) -> Result<Value> {
        if matches!(command, crate::entity::Command::Configure) {
            let endpoint = self
                .config
                .bulbs
                .get(index)
                .ok_or_else(|| error("Unknown light"))?;
            let mut device = endpoint.connect(Duration::from_secs(self.config.timeout_seconds))?;
            device.read()?;
            return configure_light(&mut device, &args["settings"]);
        }
        if !matches!(command, crate::entity::Command::Light) {
            return Err(error("Unsupported light command"));
        }
        let name = &self
            .config
            .bulbs
            .get(index)
            .ok_or_else(|| error("Unknown light"))?
            .name;
        let change: Change = serde::Deserialize::deserialize(&args["settings"])?;
        Ok(serde_json::to_value(self.apply(name, &change)?)?)
    }

    const TOOLS: &'static [Tool] = &[
        Tool {
            name: "bulb_list",
            description: "List configured Tapo L430C/L900 light names and models without network queries or secrets.",
            input: Input::Empty,
            hints: Hints::LIST,
        },
        Tool {
            name: "bulb_status",
            description: "Read live bulb power, brightness, colour, white temperature and reported overheating.",
            input: Input::Named {
                key: "bulb",
                extra: None,
                error: "Expected a configured bulb name and settings for apply",
            },
            hints: Hints::READ,
        },
        Tool {
            name: "bulb_check",
            description: "Poll all bulbs and return debounced overheating/recovery events and retained faults. Missing readings never clear faults.",
            input: Input::Empty,
            hints: Hints::CHECK,
        },
        Tool {
            name: "bulb_apply",
            description: "Apply absolute bulb settings and verify them with a fresh read. No toggles or automatic retries. Brightness/colour do not implicitly turn power on.",
            input: Input::Named {
                key: "bulb",
                extra: Some(SETTINGS_FIELD),
                error: "Expected a configured bulb name and settings for apply",
            },
            hints: Hints::CONTROL,
        },
    ];
    fn open(path: &Path) -> Result<Self> {
        Self::open(path)
    }
    fn execute(&mut self, name: &str, args: &Value) -> Result<Value> {
        let bulb_name = args.get("bulb").and_then(Value::as_str).unwrap_or_default();
        match name {
            "bulb_list" => Ok(self.list()),
            "bulb_status" => Ok(serde_json::to_value(self.status(bulb_name)?)?),
            "bulb_apply" => {
                let change: Change =
                    serde::Deserialize::deserialize(args.get("settings").unwrap_or(&Value::Null))
                        .map_err(|_| error("Invalid bulb settings"))?;
                Ok(serde_json::to_value(self.apply(bulb_name, &change)?)?)
            }
            "bulb_check" => Ok(serde_json::to_value(self.check()?)?),
            _ => Err(error("Unknown bulb operation")),
        }
    }
}
use crate::tapo_settings as ts;
const LIGHT_RULES: &[ts::Rule] = &[ts::AUTO_UPDATE, ts::SMOOTH, ts::FADE_ON, ts::FADE_OFF];
const LIGHT_SETTINGS: &[crate::settings::Setting] = &[
    ts::AUTO_UPDATE.setting,
    ts::SMOOTH.setting,
    ts::FADE_ON.setting,
    ts::FADE_OFF.setting,
    crate::settings::Setting::integer("preset", 1, 16, 1),
    crate::settings::Setting::boolean("effect_enabled"),
];

impl crate::device::Monitored for Manager {
    fn active_faults(&self, visit: &mut impl FnMut(&str, &[String])) {
        for (name, saved) in &self.monitor.memory.devices {
            visit(name, &saved.faults);
        }
    }

    type Unavailable = Unavailable;
    type Event = fault::Event;
    fn poll_seconds(&self) -> u64 {
        self.config.poll_seconds
    }
    fn watch(&mut self) -> Result<fault::Watch<Self::Unavailable, Self::Event>> {
        Manager::watch(self)
    }
}

pub(crate) const SETTINGS_FIELD: Field = Field {
    name: "settings",
    schema: || json!({"type":"object","properties":{"power":{"type":"boolean"},"brightness_percent":{"type":"integer","minimum":1,"maximum":100},"hue":{"type":"integer","minimum":0,"maximum":360},"saturation_percent":{"type":"integer","minimum":0,"maximum":100},"color_temperature_k":{"type":"integer","minimum":2500,"maximum":9000}},"minProperties":1,"additionalProperties":false,"dependentRequired":{"hue":["saturation_percent"],"saturation_percent":["hue"]}}),
    valid: |v| <Change as serde::Deserialize>::deserialize(v).is_ok_and(|c| c.validate().is_ok()),
    error: "Invalid bulb settings",
};

fn light_presets(client: &mut klap::Client) -> Result<Value> {
    let mut states = Vec::new();
    for _ in 0..16 {
        let result = client.query(
            "get_preset_rules",
            Some(json!({"start_index":states.len()})),
        )?;
        let page = result["states"]
            .as_array()
            .ok_or_else(|| error("Missing light presets"))?;
        let total = result["sum"].as_u64().unwrap_or(page.len() as u64) as usize;
        if total > 16
            || page.is_empty()
            || states.len() + page.len() > 16
            || result["start_index"]
                .as_u64()
                .is_some_and(|n| n != states.len() as u64)
        {
            return Err(error("Invalid bounded light preset page"));
        }
        states.extend(page.iter().cloned());
        if states.len() >= total {
            return Ok(json!({"states":states,"sum":total}));
        }
    }
    Err(error("Incomplete light presets"))
}
fn configure_light(device: &mut NetworkDevice<'_>, settings: &Value) -> Result<Value> {
    crate::settings::validate(LIGHT_SETTINGS, settings)?;
    if settings.get("preset").is_some() && settings.as_object().is_some_and(|o| o.len() != 1) {
        return Err(error("Apply a light preset in a separate request"));
    }
    if let Some(index) = settings["preset"].as_u64() {
        let rules = light_presets(&mut device.client)?;
        let states = rules["states"]
            .as_array()
            .filter(|s| s.len() <= 16)
            .ok_or_else(|| error("Missing light preset states"))?;
        let p = states
            .get(index as usize - 1)
            .filter(|p| p.get("brightness").is_some())
            .ok_or_else(|| error("Preset is absent or is an unsupported effect preset"))?;
        let brightness = p["brightness"]
            .as_u64()
            .filter(|v| (1..=100).contains(v))
            .ok_or_else(|| error("Invalid preset brightness"))? as u8;
        let temp = p["color_temp"]
            .as_u64()
            .ok_or_else(|| error("Missing preset colour temperature"))?;
        let change = if temp > 0 {
            Change {
                brightness_percent: Some(brightness),
                color_temperature_k: Some(u16::try_from(temp)?),
                ..Change::default()
            }
        } else {
            Change {
                brightness_percent: Some(brightness),
                hue: Some(u16::try_from(
                    p["hue"]
                        .as_u64()
                        .ok_or_else(|| error("Missing preset hue"))?,
                )?),
                saturation_percent: Some(u8::try_from(
                    p["saturation"]
                        .as_u64()
                        .ok_or_else(|| error("Missing preset saturation"))?,
                )?),
                ..Change::default()
            }
        };
        return Ok(serde_json::to_value(apply(device, &change)?)?);
    }
    if let Some(enabled) = settings["effect_enabled"].as_bool() {
        if settings.as_object().is_some_and(|o| o.len() != 1) {
            return Err(error("Change light effects in a separate request"));
        }
        let status = device.read()?;
        if status.overheated != Some(false) && enabled {
            return Err(error(
                "Unknown or active overheating blocks effect activation",
            ));
        }
        if device.endpoint.model == Model::L900 {
            let mut effect = device.client.read("get_lighting_effect")?;
            if !effect.is_object()
                || effect.get("enable").is_none()
                || enabled && effect["id"].as_str().is_none_or(str::is_empty)
            {
                return Err(error("No stored effect is available to restore"));
            }
            effect["enable"] = json!(u8::from(enabled));
            device.client.query("set_lighting_effect", Some(effect))?;
        } else {
            let mut params = json!({"enable":enabled});
            if enabled {
                let rules = device.client.query(
                    "get_dynamic_light_effect_rules",
                    Some(json!({"start_index":0})),
                )?;
                let id = rules["current_rule_id"]
                    .as_str()
                    .filter(|s| !s.is_empty() && s.len() <= 64)
                    .ok_or_else(|| error("No stored bulb effect is available"))?;
                params["id"] = json!(id);
            }
            device
                .client
                .query("set_dynamic_light_effect_rule_enable", Some(params))?;
        }
        let info = device.client.read("get_device_info")?;
        let actual = if device.endpoint.model == Model::L900 {
            info["lighting_effect"]["enable"]
                .as_u64()
                .filter(|n| *n <= 1)
                .map(|n| n == 1)
        } else {
            info["dynamic_light_effect_enable"].as_bool()
        };
        if actual != Some(enabled) {
            return Err(error("Effect change could not be verified; no retry"));
        }
        return Ok(json!({"verified":true,"effect_enabled":enabled}));
    }
    crate::tapo_settings::configure(&mut device.client, LIGHT_RULES, settings)
}
