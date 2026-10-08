use crate::device::{Adapter, Field, Hints, Input, Tool};
pub use crate::routine::meaco::{Applied, Operation, already_correct, apply, plan};
use crate::{
    Result,
    config::{Memory, Store},
    error,
    tuya::Client,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::{path::Path, thread, time::Duration};

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
    const SUPPORT: crate::entity::Support =
        crate::entity::Support::READ.union(crate::entity::Support::ROUTINES);
    fn entities<'a>(&'a self, visit: &mut impl FnMut(crate::entity::Entity<'a>)) {
        use crate::{
            device::Family,
            entity::{Capabilities, Domain, Entity},
        };
        visit(Entity::new(
            Family::Meaco,
            0,
            Domain::Humidifier,
            "meaco",
            "Arete Two",
            Capabilities::Read,
        ));
    }
    fn read_device(&mut self, index: usize) -> Result<Value> {
        if index != 0 {
            return Err(error("Unknown Meaco device"));
        }
        self.execute("meaco_status", &json!({}))
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
            description: "Apply a verified Meaco sequence: day=normal/50%; night=purify then sleep; off=power off.",
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
    fn open(path: &Path) -> Result<Self> {
        Self::open(path)
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
}
impl Manager {
    fn operation_call(&mut self, name: &str, operation: Option<Operation>) -> Result<Value> {
        match name {
            "meaco_status" => Ok(serde_json::to_value(self.device.read()?)?),
            "meaco_plan" => {
                let op = operation.ok_or_else(|| error("Missing operation"))?;
                let state = self.device.read()?;
                Ok(
                    json!({"operation":op,"state":state,"steps":plan(&state,op,&self.memory),"settle_seconds":self.device.settle_seconds,
                    "note":"Provisional plan; execution rechecks state after every command."}),
                )
            }
            "meaco_apply" => {
                let op = operation.ok_or_else(|| error("Missing operation"))?;
                Ok(serde_json::to_value(apply(
                    &mut self.device,
                    op,
                    &mut self.memory,
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
        let config = crate::config::Config::load(path)?;
        let store = Store::open(path, &config.device_id)?;
        let memory = store.read()?;
        Ok(Self {
            device: NetworkDevice {
                client: Client::new(&config, connect)?,
                settle_seconds: config.settle_seconds,
            },
            memory,
            store,
        })
    }
    /// Persist a due attempt before returning it; callers must never replay it.
    pub fn scheduled_operation(
        &mut self,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<Option<Operation>> {
        let Some((operation, name, date)) =
            crate::schedule::due(now, &self.memory.schedule_attempts)
        else {
            return Ok(None);
        };
        self.memory.schedule_attempts.insert(name, date);
        self.store.save(&self.memory)?;
        Ok(Some(operation))
    }
}

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
