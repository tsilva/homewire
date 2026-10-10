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
    const PUBLIC_TOOLS: &'static [Tool] = &[];
    const SUPPORT: crate::entity::Support;
    const CONFIG_SECTION: &'static str = "adapters";
    fn open(path: &Path) -> Result<Self> {
        Self::from_source(&crate::config::Source::file(path))
    }
    fn from_source(source: &crate::config::Source<'_>) -> Result<Self>;
    /// Implementation hook for validated canonical commands. Callers should
    /// use `Devices::call`; transport permission policy remains outside adapters.
    fn execute(&mut self, name: &str, arguments: &Value) -> Result<Value>;
    /// Borrowed entity descriptors; no retained registry or device I/O.
    fn entities<'a>(&'a self, visit: &mut impl FnMut(crate::entity::Entity<'a>));
    /// Borrowed, configured wake endpoints; no network discovery or retained registry.
    fn wake_targets<'a>(&'a self, _visit: &mut impl FnMut(crate::wol::Target<'a>)) {}
    fn wake_device(&mut self, _index: usize) -> Result<Value> {
        Err(error("This device does not support Wake-on-LAN"))
    }
    /// One fresh physical-device snapshot, shared across requested entities.
    fn read_device(&mut self, _index: usize) -> Result<Value> {
        Err(error("This adapter has no readable physical device"))
    }
    fn options(&mut self, _index: usize) -> Result<Value> {
        Err(error("This adapter has no additional live options"))
    }
    /// Optional autonomous read job. Deadlines stay with the adapter; no
    /// scheduler registry, cached JSON or polling thread is required.
    fn next_background(&self) -> Option<std::time::Instant> {
        None
    }
    fn background_tick(&mut self) -> Result<()> {
        Ok(())
    }
    /// Publish retained autonomous monitor state through the shared alert ledger.
    /// Reconciliation also runs after failed reads; unknown state cannot clear it.
    fn reconcile_alerts(&self, _alerts: &mut crate::alerts::Manager, _now: i64) -> Result<()> {
        Ok(())
    }
    fn authorize_control(
        &self,
        _command: crate::entity::Command,
        _arguments: &Value,
        _maintenance: bool,
    ) -> Result<()> {
        Ok(())
    }
    fn control_device(
        &mut self,
        _index: usize,
        _command: crate::entity::Command,
        _arguments: &Value,
    ) -> Result<Value> {
        Err(error("This device has no requested control capability"))
    }
}

/// Optional monitoring capability, implemented only by families with faults.
/// Typed results avoid a universal status enum or an allocated event wrapper.
pub trait Monitored: Adapter {
    type Unavailable: Serialize;
    type Event: Serialize;
    fn poll_seconds(&self) -> u64;
    fn watch(&mut self) -> Result<fault::Watch<Self::Unavailable, Self::Event>>;
    fn active_faults(&self, visit: &mut impl FnMut(&str, &[String]));
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
    Object {
        schema: fn() -> Value,
        valid: fn(&Value) -> bool,
        error: &'static str,
    },
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
            Self::Object { valid, error, .. } => return valid(args).then_some(()).ok_or(error),
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
                    || !object.get(key).and_then(Value::as_str).is_some_and(|s| {
                        !s.is_empty() && s.len() <= if key == "entity" { 96 } else { 32 }
                    })
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
            Self::Object { schema, .. } => return schema(),
            Self::Empty => return schema,
            Self::Field { field, .. } => (None, Some(field)),
            Self::Named { key, extra, .. } => (Some(named_key.unwrap_or(key)), extra),
        };
        let mut required =
            Vec::with_capacity(usize::from(key.is_some()) + usize::from(field.is_some()));
        if let Some(key) = key {
            schema["properties"][key] = json!({"type":"string","minLength":1,"maxLength":if key == "entity" { 96 } else { 32 }});
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
    ($($field:ident / $variant:ident: $adapter:ty $(=> $owner:ident.$child:ident)?),+ $(,)?) => {
        #[derive(Default)]
        pub struct Devices {
            $(pub $field: Option<$adapter>,)+
            /// Explicit override for direct controls owned by a routine.
            pub maintenance: bool,
        }
        #[derive(Clone, Copy, PartialEq, Eq, serde::Serialize)]
        #[serde(rename_all = "snake_case")]
        pub enum Family { $($variant,)+ }
        impl Family { pub const fn as_str(self) -> &'static str { match self { $(Self::$variant => stringify!($field),)+ } } }
        #[derive(Clone, Copy, Default)]
        pub struct Selection { $(pub $field: bool,)+ }
        impl Selection {
            pub fn any(self)->bool { false $(||self.$field)+ }
            pub fn support(self) -> crate::entity::Support {
                [$(if self.$field { <$adapter as Adapter>::SUPPORT } else { crate::entity::Support::NONE },)+]
                    .into_iter().fold(crate::entity::Support::NONE, crate::entity::Support::union)
            }
            pub const ALL: Self = Self { $($field:true,)+ };
            pub fn from_filter(mut enabled: impl FnMut(&str) -> bool) -> Self {
                Self { $($field:enabled(stringify!($field)),)+ }
            }
            pub fn tools(self) -> impl Iterator<Item = &'static Tool> + Clone {
                [$(if self.$field { <$adapter as Adapter>::TOOLS } else { &[] },)+]
                    .into_iter().flatten()
            }
            pub fn public_tools(self) -> impl Iterator<Item = &'static Tool> + Clone {
                [$(if self.$field { <$adapter as Adapter>::PUBLIC_TOOLS } else { &[] },)+].into_iter().flatten()
            }
        }
        impl Devices {
            pub fn config_section(family: &str) -> Option<&'static str> {
                match family {
                    $(stringify!($field) => Some(<$adapter as Adapter>::CONFIG_SECTION),)+
                    _ => None,
                }
            }
            pub fn open_source(&mut self, family: &str, source: &crate::config::Source<'_>) -> Result<()> {
                match family {
                    $(stringify!($field) => {
                        if self.$field.is_some() { return Err(error("Adapter configured more than once")); }
                        self.$field = Some(<$adapter as Adapter>::from_source(source)?);
                    },)+
                    _ => return Err(error("Unknown static adapter family")),
                }
                Ok(())
            }
            pub fn next_background(&self) -> Option<std::time::Instant> {
                [$(self.$field.as_ref().and_then(Adapter::next_background),)+].into_iter().flatten().min()
            }
            pub fn background_tick(&mut self) {
                let now=std::time::Instant::now();
                $(if let Some(adapter)=self.$field.as_mut() && adapter.next_background().is_some_and(|due| due<=now)
                    && let Err(e)=adapter.background_tick() { eprintln!("{} background read failed: {e}", stringify!($field)); })+
                if let Some(mut alerts) = self.alerts.take() {
                    let now = chrono::Utc::now().timestamp();
                    $(if let Some(adapter) = self.$field.as_ref()
                        && let Err(e) = adapter.reconcile_alerts(&mut alerts, now) {
                        eprintln!("{} alert reconciliation failed: {e}", stringify!($field));
                    })+
                    self.alerts = Some(alerts);
                }
            }
            pub fn open_adapter(&mut self, family: &str, path: &Path) -> Result<()> {
                match family {
                    $(stringify!($field) => {
                        if self.$field.is_some() { return Err(error("Adapter configured more than once")); }
                        self.$field = Some(<$adapter as Adapter>::open(path)?);
                    },)+
                    _ => return Err(error("Unknown static adapter family")),
                }
                Ok(())
            }
            pub(crate) fn validate_ownership(&self) -> Result<()> {
                if self.tv.is_some() && (self.samsung.is_some() || self.digi.is_some() || self.plugs.is_some()) {
                    return Err(error("TV owns its Samsung, DIGI and plug adapters; configure those through --tv only"));
                }
                Ok(())
            }
            pub fn validate(&self) -> Result<()> {
                self.validate_ownership()?;
                crate::wol::validate_targets(self)?;
                let mut ambiguous = false;
                self.entities(&mut |entity| {
                    let mut matches = 0;
                    self.entities(&mut |other| { if entity.same_id(other) { matches += 1; } });
                    ambiguous |= matches != 1;
                });
                if ambiguous { return Err(error("Duplicate entity identity; rename configured endpoints")); }
                Ok(())
            }
            $(pub fn $field(&mut self) -> Option<&mut $adapter> {
                self.$field.as_mut() $(.or_else(||self.$owner.as_mut().map(|owner|owner.$child())))?
            })+
            pub fn selection(&self) -> Selection {
                Selection { $($field: self.$field.is_some() $(|| self.$owner.is_some())?,)+ }
            }
            pub fn entities<'a>(&'a self, visit: &mut impl FnMut(crate::entity::Entity<'a>)) {
                $(if let Some(adapter) = self.$field.as_ref() {
                    adapter.entities(visit);
                })+
                if let Some(tv) = &self.tv {
                    tv.child_entities(visit);
                }
            }
            pub(crate) fn wake_targets<'a>(&'a self, visit: &mut impl FnMut(crate::wol::Target<'a>)) {
                $(if let Some(adapter) = self.$field.as_ref() {
                    adapter.wake_targets(visit);
                })+
                if let Some(tv) = &self.tv { tv.child_wake_targets(visit); }
            }
            pub(crate) fn wake_device(&mut self, device: crate::entity::Device) -> Result<Value> {
                if !self.maintenance && self.tv.as_ref().is_some_and(|tv|tv.owns_power(device)) {
                    return Err(error("Power is owned by the TV routine; use routine_run or explicit --maintenance"));
                }
                match device.family {
                    $(Family::$variant => self.$field().ok_or_else(||error("Unconfigured device"))?.wake_device(device.index),)+
                }
            }
            pub(crate) fn read_device(&mut self, device: crate::entity::Device) -> Result<Value> {
                match device.family {
                    $(Family::$variant => self.$field().ok_or_else(||error("Unconfigured device"))?.read_device(device.index),)+
                }
            }
            pub(crate) fn options(&mut self,device:crate::entity::Device)->Result<Value> {
                match device.family {$(Family::$variant=>self.$field().ok_or_else(||error("Unconfigured device"))?.options(device.index),)+}
            }
            pub(crate) fn control_device(&mut self, device: crate::entity::Device, command: crate::entity::Command, args: &Value) -> Result<Value> {
                if matches!(command, crate::entity::Command::Power)
                    && !self.maintenance && self.tv.as_ref().is_some_and(|tv|tv.owns_power(device)) {
                    return Err(error("Power is owned by the TV routine; use routine_run or explicit --maintenance"));
                }
                if matches!(command,crate::entity::Command::Configure) && !self.maintenance
                    && self.tv.as_ref().is_some_and(|tv|tv.owns_power(device))
                    && args["settings"].as_object().is_some_and(|o|o.contains_key("auto_off")||o.contains_key("auto_off_minutes")) {
                    return Err(error("TV routine owns outlet shutdown; auto-off changes require --maintenance"));
                }
                match device.family {
                    $(Family::$variant => {
                        let maintenance=self.maintenance;
                        let adapter=self.$field().ok_or_else(||error("Unconfigured device"))?;
                        adapter.authorize_control(command,args,maintenance)?;
                        adapter.control_device(device.index,command,args)
                    },)+
                }
            }
            /// Canonical command dispatch validates before any device I/O.
            /// Permission policy belongs to the caller (e.g. MCP control gate).
            pub fn call(&mut self, name: &str, args: &Value) -> Result<Value> {
                self.validate_ownership()?;
                if !self.maintenance && self.tv.as_ref().is_some_and(|tv|tv.owns_command(name,args)) {
                    return Err(error("Power is owned by the TV routine; use routine_run or explicit --maintenance"));
                }
                $(if let Some(adapter) = self.$field()
                    && let Some(tool) = <$adapter as Adapter>::TOOLS.iter().chain(<$adapter as Adapter>::PUBLIC_TOOLS).find(|t| t.name == name) {
                    tool.input.validate(args).map_err(error)?;
                    return adapter.execute(name, args);
                })+
                Err(error("Unknown or unconfigured device tool"))
            }
        }
    };
}
static_adapters! {
    wol/Wol: crate::wol::Manager,
    radio/Radio: crate::convenience::Radio,
    speech/Speech: crate::convenience::Speech,
    brother/Brother: crate::brother::Manager,
    cast/Cast: crate::cast::Manager,
    dlna/Dlna: crate::dlna::Manager,
    mobile/Mobile: crate::mobile::Manager,
    shopping/Shopping: crate::shopping::Manager,
    weather/Weather: crate::weather::Config,
    history/History: crate::history::Manager,
    alerts/Alerts: crate::alerts::Manager,
    meaco/Meaco: crate::meaco::Manager,
    bulbs/Bulbs: crate::bulb::Manager,
    plugs/Plugs: crate::plug::Manager => tv.plugs,
    awair/Awair: crate::awair::Config,
    printers/Printers: crate::printer::Manager,
    samsung/Samsung: crate::samsung::Manager => tv.samsung,
    digi/Digi: crate::androidtv::Manager => tv.digi,
    tv/Tv: crate::tv::Manager,
}
