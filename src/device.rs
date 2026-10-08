//! Compile-time device catalog and transport-independent command interface.
//! Adapters stay concrete and inline: no boxes, plugin loader or retained JSON.
use crate::{Result, error, fault};
use serde::Serialize;
use serde_json::{Value, json};
use std::path::Path;

/// A device family owns configuration, commands, validation and safety rules.
/// Both CLI and MCP use this interface; protocol details stay inside the family.
pub trait Adapter: Sized {
    const TOOLS: &'static [Tool];
    fn open(path: &Path) -> Result<Self>;
    /// Implementation hook for validated canonical commands. Callers should
    /// use `Devices::call`; transport permission policy remains outside adapters.
    fn execute(&mut self, name: &str, arguments: &Value) -> Result<Value>;
}

/// Optional monitoring capability, implemented only by families with faults.
/// Typed results avoid a universal status enum or an allocated event wrapper.
pub trait Monitored: Adapter {
    type Unavailable: Serialize;
    type Event: Serialize;
    fn poll_seconds(&self) -> u64;
    fn watch(&mut self) -> Result<fault::Watch<Self::Unavailable, Self::Event>>;
}

#[derive(Clone, Copy, Serialize)]
pub struct Hints {
    #[serde(rename = "readOnlyHint")]
    pub read_only: bool,
    #[serde(rename = "destructiveHint", skip_serializing_if = "Option::is_none")]
    pub destructive: Option<bool>,
    #[serde(rename = "idempotentHint")]
    pub idempotent: bool,
    #[serde(rename = "openWorldHint")]
    pub open_world: bool,
}
impl Hints {
    pub const READ: Self = Self {
        read_only: true,
        destructive: Some(false),
        idempotent: true,
        open_world: true,
    };
    pub const LIST: Self = Self {
        open_world: false,
        ..Self::READ
    };
    pub const CHECK: Self = Self {
        idempotent: false,
        ..Self::READ
    };
    pub const CONTROL: Self = Self {
        read_only: false,
        ..Self::READ
    };
}

/// Small static field definitions permit family-specific schemas and validation
/// without teaching the catalog or a transport about device-specific settings.
#[derive(Clone, Copy)]
pub struct Field {
    pub name: &'static str,
    pub schema: fn() -> Value,
    pub valid: fn(&Value) -> bool,
    pub error: &'static str,
}
#[derive(Clone, Copy)]
pub enum Input {
    Empty,
    Field {
        field: Field,
        shape_error: &'static str,
    },
    Named {
        key: &'static str,
        extra: Option<Field>,
        error: &'static str,
    },
}
impl Input {
    pub fn validate(self, args: &Value) -> std::result::Result<(), &'static str> {
        let object = args.as_object().ok_or("Arguments must be an object")?;
        let field = match self {
            Self::Empty => {
                return if object.is_empty() {
                    Ok(())
                } else {
                    Err("This tool takes no arguments")
                };
            }
            Self::Field { field, shape_error } => {
                if object.len() != 1 {
                    return Err(shape_error);
                }
                Some(field)
            }
            Self::Named { key, extra, error } => {
                if object.len() != 1 + usize::from(extra.is_some())
                    || !object
                        .get(key)
                        .and_then(Value::as_str)
                        .is_some_and(|s| !s.is_empty() && s.len() <= 32)
                {
                    return Err(error);
                }
                extra
            }
        };
        if let Some(field) = field
            && !object.get(field.name).is_some_and(field.valid)
        {
            return Err(field.error);
        }
        Ok(())
    }
    /// Optional key substitution supports presentation aliases without copying
    /// or editing a completed schema. It changes no device validation rule.
    pub fn schema(self, named_key: Option<&str>) -> Value {
        let mut schema = json!({"type":"object","properties":{},"additionalProperties":false});
        let (key, field) = match self {
            Self::Empty => return schema,
            Self::Field { field, .. } => (None, Some(field)),
            Self::Named { key, extra, .. } => (Some(named_key.unwrap_or(key)), extra),
        };
        let mut required =
            Vec::with_capacity(usize::from(key.is_some()) + usize::from(field.is_some()));
        if let Some(key) = key {
            schema["properties"][key] = json!({"type":"string","minLength":1,"maxLength":32});
            required.push(Value::String(key.into()));
        }
        if let Some(field) = field {
            schema["properties"][field.name] = (field.schema)();
            required.push(Value::String(field.name.into()));
        }
        schema["required"] = Value::Array(required);
        schema
    }
}

pub struct Tool {
    pub name: &'static str,
    pub description: &'static str,
    pub input: Input,
    pub hints: Hints,
}

// This single declaration generates storage, discovery and dispatch. Adding a
// family requires registering its concrete type here, never editing MCP routing.
macro_rules! static_adapters {
    ($($field:ident: $adapter:ty),+ $(,)?) => {
        #[derive(Default)]
        pub struct Devices { $(pub $field: Option<$adapter>,)+ }
        #[derive(Clone, Copy, Default)]
        pub struct Selection { $(pub $field: bool,)+ }
        impl Selection {
            pub const ALL: Self = Self { $($field:true,)+ };
            pub fn from_filter(mut enabled: impl FnMut(&str) -> bool) -> Self {
                Self { $($field:enabled(stringify!($field)),)+ }
            }
            pub fn tools(self) -> impl Iterator<Item = &'static Tool> + Clone {
                [$(if self.$field { <$adapter as Adapter>::TOOLS } else { &[] },)+]
                    .into_iter().flatten()
            }
        }
        impl Devices {
            pub fn selection(&self) -> Selection {
                Selection { $($field: self.$field.is_some(),)+ }
            }
            /// Canonical command dispatch validates before any device I/O.
            /// Permission policy belongs to the caller (e.g. MCP control gate).
            pub fn call(&mut self, name: &str, args: &Value) -> Result<Value> {
                $(if let Some(adapter) = &mut self.$field
                    && let Some(tool) = <$adapter as Adapter>::TOOLS.iter().find(|t| t.name == name) {
                    tool.input.validate(args).map_err(error)?;
                    return adapter.execute(name, args);
                })+
                Err(error("Unknown or unconfigured device tool"))
            }
        }
    };
}
static_adapters! {
    meaco: crate::meaco::Manager,
    bulbs: crate::bulb::Manager,
    plugs: crate::plug::Manager,
    awair: crate::awair::Config,
    printers: crate::printer::Manager,
}
