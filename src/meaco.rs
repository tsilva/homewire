use crate::device::{Adapter, Field, Hints, Input, Tool};
pub use crate::routine::meaco::{Applied, Operation, apply, plan};
use crate::{
    Result,
    config::{Memory, Store},
    error,
    tuya::Client,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::{
    thread,
    time::{Duration, Instant},
};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct State {
    pub power: bool,
    pub mode: String,
    pub target_humidity: u8,
    pub current_humidity: Option<u8>,
    pub fault_code: u32,
    pub tank_full: bool,
    pub defrost: bool,
    pub problem: bool,
}
impl State {
    pub fn from_dps(dps: &Map<String, Value>) -> Result<Self> {
        let power = dps
            .get("1")
            .and_then(Value::as_bool)
            .ok_or_else(|| error("Missing or invalid power state"))?;
        let mode = dps
            .get("4")
            .and_then(Value::as_str)
            .ok_or_else(|| error("Missing or invalid mode"))?
            .to_owned();
        if !matches!(mode.as_str(), "manual" | "purify" | "sleep" | "laundry") {
            return Err(error("Unrecognized Meaco mode"));
        }
        let target_humidity =
            humidity(dps.get("2"))?.ok_or_else(|| error("Missing target humidity"))?;
        let current_humidity = humidity(dps.get("16"))?;
        let fault_code = dps
            .get("19")
            .and_then(Value::as_u64)
            .and_then(|n| u32::try_from(n).ok())
            .ok_or_else(|| error("Missing or invalid fault code"))?;
        Ok(Self {
            power,
            mode,
            target_humidity,
            current_humidity,
            fault_code,
            tank_full: fault_code == 1,
            defrost: fault_code == 2,
            problem: !matches!(fault_code, 0 | 1 | 2 | 128),
        })
    }
}
fn humidity(value: Option<&Value>) -> Result<Option<u8>> {
    value
        .map(|v| {
            v.as_u64()
                .filter(|n| *n <= 100)
                .map(|n| n as u8)
                .ok_or_else(|| error("Invalid humidity reading"))
        })
        .transpose()
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "command", content = "value", rename_all = "snake_case")]
pub enum Action {
    Power(bool),
    Humidity(u8),
    Mode(String),
}
impl Action {
    pub fn dps(&self) -> Map<String, Value> {
        let object = match self {
            Self::Power(v) => json!({"1":v}),
            Self::Humidity(v) => json!({"2":v}),
            Self::Mode(v) => json!({"4":v}),
        };
        object.as_object().cloned().unwrap_or_default()
    }
}
pub trait Device {
    fn read(&mut self) -> Result<State>;
    fn command(&mut self, action: &Action) -> Result<()>;
    fn settle(&mut self);
}
pub struct NetworkDevice {
    pub client: Client,
    pub settle_seconds: u64,
}
impl Device for NetworkDevice {
    fn read(&mut self) -> Result<State> {
        State::from_dps(&self.client.status()?)
    }
    fn command(&mut self, action: &Action) -> Result<()> {
        self.client.set(action.dps())
    }
    fn settle(&mut self) {
        thread::sleep(Duration::from_secs(self.settle_seconds));
    }
}

impl Adapter for Manager {
    const SUPPORT: crate::entity::Support = crate::entity::Support::READ
        .union(crate::entity::Support::ROUTINES)
        .union(crate::entity::Support::CONFIGURE);
    fn entities<'a>(&'a self, visit: &mut impl FnMut(crate::entity::Entity<'a>)) {
        use crate::{
            device::Family,
            entity::{Capabilities, Domain, Entity},
        };
        let base = Entity::new(
            Family::Meaco,
            0,
            Domain::Humidifier,
            "meaco",
            "Arete Two",
            Capabilities::Read,
        );
        visit(base.configurable(SETTINGS));
        for (s, f, u) in [
            ("_humidity", "humidity", "%"),
            ("_target", "target_humidity", "%"),
            ("_remaining_hours", "remaining_hours", "h"),
            ("_tank_full", "tank_full", ""),
            ("_moisture", "moisture", ""),
        ] {
            visit(base.sensor(s, f, u));
        }
    }
    fn read_device(&mut self, index: usize) -> Result<Value> {
        if index != 0 {
            return Err(error("Unknown Meaco device"));
        }
        self.execute("meaco_status", &json!({}))
    }
    fn control_device(
        &mut self,
        index: usize,
        command: crate::entity::Command,
        args: &Value,
    ) -> Result<Value> {
        if index != 0 || !matches!(command, crate::entity::Command::Configure) {
            return Err(error("Unsupported Meaco command"));
        }
        self.configure(&args["settings"])
    }

    const TOOLS: &'static [Tool] = &[
        Tool {
            name: "meaco_status",
            description: "Read live Meaco power, mode, humidity and fault status.",
            input: Input::Empty,
            hints: Hints {
                destructive: None,
                idempotent: true,
                ..Hints::READ
            },
        },
        Tool {
            name: "meaco_plan",
            description: "Preview the day/night/off sequence using live state. Sends no controls.",
            input: Input::Field {
                field: OPERATION_FIELD,
                shape_error: "Expected only operation",
            },
            hints: Hints {
                destructive: None,
                idempotent: false,
                ..Hints::READ
            },
        },
        Tool {
            name: "meaco_apply",
            description: "Apply a verified Meaco sequence: configured day/night presets or verified power off.",
            input: Input::Field {
                field: OPERATION_FIELD,
                shape_error: "Expected only operation",
            },
            hints: Hints {
                destructive: None,
                idempotent: false,
                ..Hints::CONTROL
            },
        },
    ];
    fn from_source(source: &crate::config::Source<'_>) -> Result<Self> {
        Self::from_source(source)
    }
    fn execute(&mut self, name: &str, args: &Value) -> Result<Value> {
        self.operation_call(
            name,
            args.get("operation")
                .and_then(Value::as_str)
                .and_then(|s| s.parse().ok()),
        )
    }
}

pub struct Manager {
    device: NetworkDevice,
    memory: Memory,
    store: Store,
    monitor: crate::fault::Monitor,
    started: Instant,
    presets: crate::routine::meaco::Presets,
    schedule: Option<crate::schedule::Config>,
}
impl Manager {
    fn operation_call(&mut self, name: &str, operation: Option<Operation>) -> Result<Value> {
        match name {
            "meaco_status" => self.snapshot(),
            "meaco_plan" => {
                let op = operation.ok_or_else(|| error("Missing operation"))?;
                let state = self.device.read()?;
                Ok(
                    json!({"operation":op,"state":state,"steps":plan(&state,op,&self.memory,&self.presets)?,"settle_seconds":self.device.settle_seconds,
                    "note":"Provisional plan; execution rechecks state after every command."}),
                )
            }
            "meaco_apply" => {
                let op = operation.ok_or_else(|| error("Missing operation"))?;
                Ok(serde_json::to_value(apply(
                    &mut self.device,
                    op,
                    &mut self.memory,
                    &self.presets,
                    |memory| self.store.save(memory),
                )?)?)
            }
            _ => Err(error("Unknown operation")),
        }
    }
    pub fn open(path: &std::path::Path) -> Result<Self> {
        Self::open_with_connect(path, None)
    }
    pub fn open_with_connect(
        path: &std::path::Path,
        connect: Option<std::net::SocketAddr>,
    ) -> Result<Self> {
        Self::from_source_with_connect(&crate::config::Source::file(path), connect)
    }
    pub fn from_source(source: &crate::config::Source<'_>) -> Result<Self> {
        Self::from_source_with_connect(source, None)
    }
    pub fn from_source_with_connect(
        source: &crate::config::Source<'_>,
        connect: Option<std::net::SocketAddr>,
    ) -> Result<Self> {
        let path = source.path();
        let config = crate::config::Config::from_source(source)?;
        let store = Store::open(path, &config.device_id)?;
        let memory = store.read()?;
        let monitor = crate::fault::Monitor::from_memory(memory.faults.clone());
        Ok(Self {
            device: NetworkDevice {
                client: Client::new(&config, connect)?,
                settle_seconds: config.settle_seconds,
            },
            memory,
            store,
            monitor,
            started: Instant::now(),
            presets: config.presets,
            schedule: config.schedule,
        })
    }
    pub fn has_schedule(&self) -> bool {
        self.schedule.is_some()
    }
    pub fn has_preset(&self, operation: Operation) -> bool {
        self.presets.configured(operation)
    }
    pub fn preset(&self, operation: Operation) -> Option<&crate::routine::meaco::Preset> {
        match operation {
            Operation::Day => self.presets.day.as_ref(),
            Operation::Night => self.presets.night.as_ref(),
            Operation::Off => None,
        }
    }
    /// Persist a due attempt before returning it; callers must never replay it.
    pub fn scheduled_operation(
        &mut self,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<Option<Operation>> {
        let schedule = self
            .schedule
            .as_ref()
            .ok_or_else(|| error("Configure the Meaco schedule first"))?;
        let Some((operation, name, date)) =
            crate::schedule::due(schedule, now, &self.memory.schedule_attempts)
        else {
            return Ok(None);
        };
        self.memory.schedule_attempts.insert(name, date);
        self.store.save(&self.memory)?;
        Ok(Some(operation))
    }
    fn snapshot(&mut self) -> Result<Value> {
        let dps = self.device.client.status()?;
        let state = State::from_dps(&dps)?;
        let mut result = serde_json::to_value(&state)?;
        for (name, dp) in [
            ("child_lock", "14"),
            ("off_timer", "17"),
            ("remaining_hours", "18"),
            ("on_timer", "101"),
        ] {
            result[name] = dps.get(dp).cloned().unwrap_or(Value::Null);
        }
        result["moisture"] = json!(state.fault_code == 128);
        Ok(result)
    }
    fn configure(&mut self, settings: &Value) -> Result<Value> {
        crate::settings::validate(SETTINGS, settings)?;
        if settings["power"] == false && settings.get("mode").is_some() {
            return Err(error("Do not combine power-off with a mode change"));
        }
        let before = self.snapshot()?;
        for (name, _) in settings
            .as_object()
            .ok_or_else(|| error("Expected settings"))?
        {
            if before[name].is_null() {
                return Err(error(
                    "Meaco does not report the requested setting; no changes sent",
                ));
            }
        }
        if settings["power"] == true && before["problem"] != false {
            return Err(error("Meaco problem state blocks power-on"));
        }
        self.memory.base_mode = None;
        self.store.save(&self.memory)?;
        // Sleep is a toggle. Clear it once, verify, then establish an absolute
        // base mode. Never replay a command when its outcome is uncertain.
        if let Some(mode) = settings["mode"].as_str()
            && before["mode"] == "sleep"
            && mode != "sleep"
        {
            self.device.command(&Action::Mode("sleep".into()))?;
            self.device.settle();
            if self.device.read()?.mode == "sleep" {
                return Err(error("Sleep did not clear; no retry"));
            }
        }
        for (name, dp) in [
            ("power", "1"),
            ("target_humidity", "2"),
            ("mode", "4"),
            ("child_lock", "14"),
            ("off_timer", "17"),
            ("on_timer", "101"),
        ] {
            let Some(value) = settings.get(name) else {
                continue;
            };
            let current = self.snapshot()?;
            let wire = if name == "on_timer" && value == "cancel" {
                json!("Cancel")
            } else {
                value.clone()
            };
            let current_value = &current[name];
            if current_value == &wire {
                continue;
            }
            if name == "power" && value == true && current["problem"] == true {
                return Err(error("Meaco reports a problem; power-on is blocked"));
            }
            self.device.client.set(
                json!({dp:wire})
                    .as_object()
                    .cloned()
                    .ok_or_else(|| error("Invalid Meaco settings"))?,
            )?;
            self.device.settle();
            if self.snapshot()?[name] != wire {
                return Err(error("Meaco setting did not persist; no retry"));
            }
        }
        let after = self.snapshot()?;
        if after["mode"] != "sleep" {
            self.memory.base_mode = after["mode"]
                .as_str()
                .filter(|s| matches!(*s, "manual" | "purify"))
                .map(str::to_owned);
        }
        self.store.save(&self.memory)?;
        Ok(json!({"verified":true,"state":after}))
    }
}

impl crate::device::Monitored for Manager {
    fn active_faults(&self, visit: &mut impl FnMut(&str, &[String])) {
        for (name, saved) in &self.monitor.memory.devices {
            visit(name, &saved.faults);
        }
    }
    type Unavailable = Value;
    type Event = crate::fault::Event;
    fn poll_seconds(&self) -> u64 {
        30
    }
    fn watch(&mut self) -> Result<crate::fault::Watch<Value>> {
        let result = self.device.read();
        let faults = result.as_ref().ok().map(|s| {
            if s.tank_full {
                vec!["tank_full".into()]
            } else if s.problem {
                vec![format!("problem:{}", s.fault_code)]
            } else {
                vec![]
            }
        });
        let mut next = self.monitor.clone();
        let event = next.observe_faults(
            "meaco",
            &self.memory.device_id,
            faults.as_deref(),
            self.started.elapsed().as_secs(),
        );
        if event.is_some() {
            let mut memory = self.memory.clone();
            memory.faults = next.memory.clone();
            self.store.save(&memory)?;
            self.memory = memory;
        }
        self.monitor = next;
        Ok(crate::fault::Watch {
            events: event.into_iter().collect(),
            unavailable: result
                .err()
                .map(|e| json!({"device":"meaco","error":e.to_string()}))
                .into_iter()
                .collect(),
        })
    }
}
use crate::settings::Setting;
const TIMERS: &[&str] = &[
    "cancel", "1h", "2h", "3h", "4h", "5h", "6h", "7h", "8h", "9h", "10h", "11h", "12h", "13h",
    "14h", "15h", "16h", "17h", "18h", "19h", "20h", "21h", "22h", "23h", "24h",
];
pub(crate) const HUMIDITY_SETTING: Setting = Setting::integer("target_humidity", 35, 70, 5);
const SETTINGS: &[Setting] = &[
    Setting::boolean("power"),
    HUMIDITY_SETTING,
    Setting::choice("mode", &["manual", "laundry", "purify", "sleep"]),
    Setting::boolean("child_lock"),
    Setting::choice("off_timer", TIMERS),
    Setting::choice("on_timer", TIMERS),
];

const OPERATION_FIELD: Field = Field {
    name: "operation",
    schema: || json!({"type":"string","enum":["day","night","off"]}),
    valid: |v| {
        v.as_str()
            .and_then(|s| s.parse::<Operation>().ok())
            .is_some()
    },
    error: "Operation must be day, night or off",
};
