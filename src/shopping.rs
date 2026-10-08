//! Small persistent shopping list, independent of any cloud or HA process.
use crate::{
    Result, config,
    device::{Adapter, Hints, Input, Tool},
    entity::{Entity, Support},
    error,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs::File,
    path::{Path, PathBuf},
};
#[derive(Default, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Memory {
    #[serde(default)]
    next_id: u64,
    #[serde(default)]
    items: Vec<Item>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Item {
    id: u64,
    name: String,
    complete: bool,
}
pub struct Manager {
    path: PathBuf,
    memory: Memory,
    _lock: File,
}
impl Adapter for Manager {
    const TOOLS: &'static [Tool] = &[];
    const SUPPORT: Support = Support::NONE;
    const PUBLIC_TOOLS: &'static [Tool] = &[
        Tool {
            name: "shopping_list",
            description: "Read the persistent home shopping list.",
            input: Input::Empty,
            hints: Hints::LIST,
        },
        Tool {
            name: "shopping_update",
            description: "Add an item, set its completion, remove an item, or clear completed items. Stable item IDs come from shopping_list.",
            input: Input::Object {
                schema: || json!({"type":"object","properties":{"action":{"enum":["add","check","remove","clear_completed"]},"name":{"type":"string","minLength":1,"maxLength":128},"id":{"type":"integer","minimum":1},"complete":{"type":"boolean"}},"required":["action"],"additionalProperties":false}),
                valid: valid_update,
                error: "Expected add/name, check/id/complete, remove/id, or clear_completed",
            },
            hints: Hints::CONTROL,
        },
    ];
    fn open(path: &Path) -> Result<Self> {
        let value: Value = crate::media::load(path)?;
        if value != json!({}) {
            return Err(error("Shopping configuration must be an empty object"));
        }
        let state = path.with_extension("state.json");
        let memory: Memory = match config::read_bounded_limit(&state, "Shopping list", 32768) {
            Ok(b) => serde_json::from_slice(&b).map_err(|_| error("Invalid shopping list"))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Memory::default(),
            Err(e) => return Err(e.into()),
        };
        if memory.items.len() > 64
            || memory.items.iter().any(|i| {
                i.name.is_empty()
                    || i.name.len() > 128
                    || i.id == 0
                    || i.id > memory.next_id
                    || i.name.chars().any(char::is_control)
            })
            || memory
                .items
                .iter()
                .enumerate()
                .any(|(n, i)| memory.items[..n].iter().any(|p| p.id == i.id))
        {
            return Err(error("Invalid bounded shopping list"));
        }
        Ok(Self {
            path: state,
            memory,
            _lock: config::lock_configuration(path)?,
        })
    }
    fn entities<'a>(&'a self, _: &mut impl FnMut(Entity<'a>)) {}
    fn execute(&mut self, name: &str, args: &Value) -> Result<Value> {
        if name == "shopping_list" {
            return Ok(json!({"items":self.memory.items}));
        }
        if name != "shopping_update" || !valid_update(args) {
            return Err(error("Invalid shopping action"));
        }
        let mut next = self.memory.clone();
        match args["action"].as_str().unwrap_or_default() {
            "add" => {
                let name = args["name"].as_str().unwrap_or_default().trim();
                if !next
                    .items
                    .iter()
                    .any(|i| !i.complete && i.name.eq_ignore_ascii_case(name))
                {
                    if next.items.len() >= 64 {
                        return Err(error("Shopping list full; remove completed items"));
                    }
                    next.next_id = next
                        .next_id
                        .checked_add(1)
                        .ok_or_else(|| error("Shopping ID overflow"))?;
                    next.items.push(Item {
                        id: next.next_id,
                        name: name.into(),
                        complete: false,
                    });
                }
            }
            "check" => {
                next.items
                    .iter_mut()
                    .find(|i| Some(i.id) == args["id"].as_u64())
                    .ok_or_else(|| error("Unknown shopping item"))?
                    .complete = args["complete"]
                    .as_bool()
                    .ok_or_else(|| error("Expected completion"))?
            }
            "remove" => {
                let index = next
                    .items
                    .iter()
                    .position(|i| Some(i.id) == args["id"].as_u64())
                    .ok_or_else(|| error("Unknown shopping item"))?;
                next.items.remove(index);
            }
            "clear_completed" => next.items.retain(|i| !i.complete),
            _ => return Err(error("Invalid shopping action")),
        }
        config::save_private_bounded(&self.path, &next, 32768)?;
        self.memory = next;
        Ok(json!({"items":self.memory.items}))
    }
}
fn valid_update(v: &Value) -> bool {
    let Some(o) = v.as_object() else { return false };
    let id = || o.get("id").and_then(Value::as_u64).is_some_and(|id| id > 0);
    match v["action"].as_str() {
        Some("add") => {
            o.len() == 2
                && v["name"].as_str().is_some_and(|s| {
                    !s.trim().is_empty() && s.len() <= 128 && !s.chars().any(char::is_control)
                })
        }
        Some("check") => o.len() == 3 && id() && v["complete"].is_boolean(),
        Some("remove") => o.len() == 2 && id(),
        Some("clear_completed") => o.len() == 1,
        _ => false,
    }
}
