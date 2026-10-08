//! Standard capability interface over statically registered protocol adapters.
//! Descriptors borrow configuration. Snapshots exist only during one request.
use crate::{
    Result,
    device::{Devices, Family, Field, Hints, Input, Selection, Tool},
    error,
};
use serde::Serialize;
use serde_json::{Value, json};

pub const MAX_TARGETS: usize = 32;

/// Static category support; new adapters reuse category tools without MCP edits.
#[derive(Clone, Copy)]
pub struct Support(u8);
impl Support {
    pub const NONE: Self = Self(0);
    pub const READ: Self = Self(1);
    pub const LIGHT: Self = Self(1 | 2);
    pub const SWITCH: Self = Self(1 | 4);
    pub const MEDIA: Self = Self(1 | 8);
    pub const ROUTINES: Self = Self(16);
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Device {
    pub family: Family,
    pub index: usize,
}
#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Domain {
    Light,
    Switch,
    Sensor,
    Printer,
    Humidifier,
    MediaPlayer,
}
impl Domain {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Light => "light",
            Self::Switch => "switch",
            Self::Sensor => "sensor",
            Self::Printer => "printer",
            Self::Humidifier => "humidifier",
            Self::MediaPlayer => "media_player",
        }
    }
}
#[derive(Clone, Copy)]
pub enum Command {
    Light,
    Power,
    Key,
}
#[derive(Clone, Copy)]
pub enum Capabilities {
    Read,
    Light {
        max_temperature_k: u16,
        fixed_settings_require_effects_off: bool,
    },
    Power,
    Media {
        keys: &'static [&'static str],
        verified_off: bool,
    },
}
#[derive(Clone, Copy)]
pub struct Entity<'a> {
    pub device: Device,
    pub domain: Domain,
    pub name: &'a str,
    pub suffix: &'static str,
    pub model: &'a str,
    /// A scalar field from the physical snapshot, or the whole typed snapshot.
    pub field: Option<&'static str>,
    pub unit: Option<&'static str>,
    pub capabilities: Capabilities,
    pub owner: Option<&'static str>,
}
impl<'a> Entity<'a> {
    pub(crate) fn same_id(self, other: Self) -> bool {
        self.domain == other.domain
            && self
                .name
                .bytes()
                .chain(self.suffix.bytes())
                .eq(other.name.bytes().chain(other.suffix.bytes()))
    }

    pub fn new(
        family: Family,
        index: usize,
        domain: Domain,
        name: &'a str,
        model: &'a str,
        capabilities: Capabilities,
    ) -> Self {
        Self {
            device: Device { family, index },
            domain,
            name,
            suffix: "",
            model,
            field: None,
            unit: None,
            capabilities,
            owner: None,
        }
    }
    pub fn sensor(mut self, suffix: &'static str, field: &'static str, unit: &'static str) -> Self {
        self.domain = Domain::Sensor;
        self.suffix = suffix;
        self.field = Some(field);
        self.unit = Some(unit);
        self.capabilities = Capabilities::Read;
        self
    }
    fn matches(self, id: &str) -> bool {
        id.strip_prefix(self.domain.as_str())
            .and_then(|s| s.strip_prefix('.'))
            .is_some_and(|s| s.strip_prefix(self.name) == Some(self.suffix))
    }
    fn id(self) -> String {
        format!("{}.{}{}", self.domain.as_str(), self.name, self.suffix)
    }
    fn describe(self, maintenance: bool) -> Value {
        let mut features = match self.capabilities {
            Capabilities::Read => json!({"read":true}),
            Capabilities::Power => json!({"read":true,"power":true}),
            Capabilities::Light {
                max_temperature_k,
                fixed_settings_require_effects_off,
            } => {
                json!({"read":true,"power":true,"brightness_percent":{"min":1,"max":100},"color_modes":["hs","color_temperature"],"color_temperature_k":{"min":2500,"max":max_temperature_k,"range_source":"model_envelope","live_range_verified_on_control":true},"effects_control":false,"fixed_settings_require_effects_off":fixed_settings_require_effects_off})
            }
            Capabilities::Media { keys, verified_off } => {
                json!({"read":true,"power":true,"remote_keys":keys,"power_off_verifiable":verified_off})
            }
        };
        if self.owner.is_some() && !maintenance {
            features["power"] = json!(false);
        }
        json!({"entity":self.id(),"domain":self.domain,"device":format!("{}.{}",self.device.family.as_str(),self.name),"model":self.model,"unit":self.unit,"capabilities":features,"power_owner":self.owner})
    }
}
fn resolve<'a>(devices: &'a Devices, id: &str) -> Result<Entity<'a>> {
    let mut found = None;
    let mut duplicate = false;
    devices.entities(&mut |entity| {
        if entity.matches(id) {
            duplicate |= found.is_some();
            found = Some(entity);
        }
    });
    if duplicate {
        return Err(error("Ambiguous entity identity"));
    }
    found.ok_or_else(|| error("Unknown configured entity"))
}
/// Pure configuration discovery; never contacts a device or includes credentials.
pub fn list(devices: &Devices) -> Value {
    let mut entities = Vec::new();
    devices.entities(&mut |entity| entities.push(entity.describe(devices.maintenance)));
    json!({"entities":entities})
}
fn status(devices: &mut Devices, args: &Value) -> Result<Value> {
    (TARGETS.valid)(args.get("entities").unwrap_or(&Value::Null))
        .then_some(())
        .ok_or_else(|| error(TARGETS.error))?;
    let ids = args["entities"]
        .as_array()
        .ok_or_else(|| error(TARGETS.error))?;
    // Resolve the complete batch before I/O. Store only tiny Copy routes on stack.
    let mut routes = [None; MAX_TARGETS];
    for (i, id) in ids.iter().enumerate() {
        let entity = resolve(devices, id.as_str().ok_or_else(|| error(TARGETS.error))?)?;
        routes[i] = Some((entity.device, entity.field));
    }
    let mut states = vec![Value::Null; ids.len()];
    for i in 0..ids.len() {
        let (device, _) = routes[i].ok_or_else(|| error("Missing entity route"))?;
        if routes[..i]
            .iter()
            .flatten()
            .any(|(prior, _)| *prior == device)
        {
            continue;
        }
        let snapshot = devices.read_device(device);
        // Defer one whole-device result until projections are complete, then
        // move its JSON tree into the reply instead of cloning the snapshot.
        let aggregate = routes[..ids.len()].iter().rposition(|route| {
            route.is_some_and(|(target, field)| target == device && field.is_none())
        });
        let observed_at = chrono::Utc::now().to_rfc3339();
        for j in i..ids.len() {
            let (target, field) = routes[j].ok_or_else(|| error("Missing entity route"))?;
            if target != device {
                continue;
            }
            states[j] = match &snapshot {
                Ok(value) => {
                    json!({"entity":ids[j],"available":true,"state":if aggregate == Some(j) { &Value::Null } else { field.map_or(value,|key| if key.starts_with('/') { value.pointer(key) } else { value.get(key) }.unwrap_or(&Value::Null)) },"observed_at":observed_at,"measured_at":value.get("measured_at")})
                }
                Err(e) => {
                    json!({"entity":ids[j],"available":false,"state":null,"error":e.to_string(),"observed_at":observed_at})
                }
            };
        }
        if let (Ok(value), Some(index)) = (snapshot, aggregate) {
            states[index]["state"] = value;
        }
    }
    Ok(json!({"states":states}))
}
fn control(devices: &mut Devices, tool: &str, args: &Value) -> Result<Value> {
    let id = args["entity"]
        .as_str()
        .ok_or_else(|| error("Expected entity"))?;
    let entity = resolve(devices, id)?;
    let command = match (tool, entity.domain, entity.capabilities) {
        ("light_set", Domain::Light, Capabilities::Light { .. }) => Command::Light,
        ("switch_set", Domain::Switch, Capabilities::Power) => Command::Power,
        ("media_power", Domain::MediaPlayer, Capabilities::Media { .. }) => Command::Power,
        ("remote_key", Domain::MediaPlayer, Capabilities::Media { keys, .. })
            if args["key"].as_str().is_some_and(|key| keys.contains(&key)) =>
        {
            Command::Key
        }
        _ => return Err(error("Unsupported entity capability or remote key")),
    };
    let device = entity.device;
    // Never retain the configuration borrow during controls.
    Ok(json!({"entity":id,"result":devices.control_device(device,command,args)?}))
}

pub(crate) const POWER: Field = Field {
    name: "power",
    schema: || json!({"type":"boolean"}),
    valid: Value::is_boolean,
    error: "Expected absolute boolean power",
};
const KEY: Field = Field {
    name: "key",
    schema: || json!({"type":"string","minLength":1,"maxLength":32}),
    valid: |v| v.as_str().is_some_and(|s| !s.is_empty() && s.len() <= 32),
    error: "Expected a supported remote key",
};
const TARGETS: Field = Field {
    name: "entities",
    schema: || json!({"type":"array","minItems":1,"maxItems":MAX_TARGETS,"uniqueItems":true,"items":{"type":"string","minLength":1,"maxLength":96}}),
    valid: |v| {
        v.as_array().is_some_and(|ids| {
            !ids.is_empty()
                && ids.len() <= MAX_TARGETS
                && ids.iter().enumerate().all(|(i, id)| {
                    id.as_str().is_some_and(|s| !s.is_empty() && s.len() <= 96)
                        && !ids[..i].contains(id)
                })
        })
    },
    error: "Expected 1–32 unique entity IDs",
};
const LIST: Tool = Tool {
    name: "entity_list",
    description: "Discover configured entities, physical-device grouping, units, supported capabilities and routine-owned power. No device queries or credentials.",
    input: Input::Empty,
    hints: Hints::LIST,
};
const STATUS: Tool = Tool {
    name: "entity_status",
    description: "Read 1–32 entities, fetching each physical device once per batch. Returns availability and timestamps; unknown values stay null. No cached state or controls.",
    input: Input::Field {
        field: TARGETS,
        shape_error: "Expected only entities",
    },
    hints: Hints::READ,
};
const LIGHT: Tool = Tool {
    name: "light_set",
    description: "Set absolute light settings and verify. Brightness/colour do not imply power on. Inspect entity capabilities; active/unknown L900 effects block fixed settings.",
    input: Input::Named {
        key: "entity",
        extra: Some(crate::bulb::SETTINGS_FIELD),
        error: "Expected light entity and settings",
    },
    hints: Hints::CONTROL,
};
const SWITCH: Tool = Tool {
    name: "switch_set",
    description: "Set and verify absolute outlet power. Routine-owned outlets require maintenance access; overload guards remain enforced.",
    input: Input::Named {
        key: "entity",
        extra: Some(POWER),
        error: "Expected switch entity and power",
    },
    hints: Hints {
        destructive: Some(true),
        ..Hints::CONTROL
    },
};
const MEDIA: Tool = Tool {
    name: "media_power",
    description: "Set media power with adapter verification. Routine-owned power is blocked; use routine_run. Samsung shutdown can remain physically unverified.",
    input: Input::Named {
        key: "entity",
        extra: Some(POWER),
        error: "Expected media entity and power",
    },
    hints: Hints {
        idempotent: false,
        ..Hints::CONTROL
    },
};
const REMOTE: Tool = Tool {
    name: "remote_key",
    description: "Send one supported remote key, without retrying or power toggles. Discover supported keys through entity_list.",
    input: Input::Named {
        key: "entity",
        extra: Some(KEY),
        error: "Expected media entity and key",
    },
    hints: Hints {
        idempotent: false,
        ..Hints::CONTROL
    },
};
pub fn tools(selection: Selection) -> impl Iterator<Item = &'static Tool> + Clone {
    let support = selection.support();
    let any = support.contains(Support::READ);
    [
        any.then_some(&LIST),
        any.then_some(&STATUS),
        support.contains(Support::LIGHT).then_some(&LIGHT),
        support.contains(Support::SWITCH).then_some(&SWITCH),
        support.contains(Support::MEDIA).then_some(&MEDIA),
        support.contains(Support::MEDIA).then_some(&REMOTE),
    ]
    .into_iter()
    .flatten()
    .chain(crate::routine::tools(selection))
}
pub fn call(devices: &mut Devices, name: &str, args: &Value) -> Result<Value> {
    devices.validate_ownership()?;
    let tool = tools(devices.selection())
        .find(|tool| tool.name == name)
        .ok_or_else(|| error("Unknown or unconfigured capability tool"))?;
    tool.input.validate(args).map_err(error)?;

    match name {
        "entity_list" => Ok(list(devices)),
        "entity_status" => status(devices, args),
        "light_set" | "switch_set" | "media_power" | "remote_key" => control(devices, name, args),
        _ => crate::routine::call(devices, name, args),
    }
}
