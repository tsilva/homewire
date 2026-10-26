//! Explicit bounded device groups; absolute light power is the first capability.
use crate::{
    Result,
    device::{Devices, Hints, Input, Selection, Tool},
    entity::{self, Capabilities, Device, Domain, Support},
    error,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Group {
    pub members: Vec<String>,
}

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(transparent)]
pub struct Groups(pub BTreeMap<String, Group>);

pub(crate) fn light_target(devices: &Devices, id: &str) -> Result<Device> {
    let entity = entity::resolve(devices, id)?;
    if entity.domain != Domain::Light
        || entity.field.is_some()
        || !matches!(entity.capabilities, Capabilities::Light { .. })
        || entity.owner.is_some()
    {
        return Err(error("Group power requires an unowned light entity"));
    }
    Ok(entity.device)
}

impl Groups {
    pub fn validate(&self, devices: &Devices) -> Result<()> {
        if !crate::config::valid_unique_labels(self.0.keys().map(String::as_str), 32, b"-_") {
            return Err(error(
                "Configure at most eight uniquely named device groups",
            ));
        }
        for name in self.0.keys() {
            self.targets(devices, &format!("group.{name}"))?;
        }
        Ok(())
    }
    pub(crate) fn targets<'a>(
        &'a self,
        devices: &Devices,
        id: &str,
    ) -> Result<Vec<(&'a str, Device)>> {
        let group = id
            .strip_prefix("group.")
            .and_then(|name| self.0.get(name))
            .ok_or_else(|| error("Unknown configured device group"))?;
        if group.members.is_empty() || group.members.len() > 8 {
            return Err(error("Group requires 1–8 explicit light members"));
        }
        let mut targets = Vec::new();
        for member in &group.members {
            if member.len() > 96 {
                return Err(error("Invalid group entity"));
            }
            let device = light_target(devices, member)?;
            if targets.iter().any(|(_, target)| *target == device) {
                return Err(error("Duplicate physical device in group"));
            }
            targets.push((member.as_str(), device));
        }
        Ok(targets)
    }
    pub(crate) fn list(&self) -> Value {
        json!({"groups":self.0.iter().map(|(name,group)|json!({"group":format!("group.{name}"),"members":group.members,"capabilities":{"power":true},"execution":"concurrent_best_effort"})).collect::<Vec<_>>()})
    }
}

fn parallel<T: Sync>(
    targets: &[T],
    control: &(impl Fn(&T) -> Result<Value> + Sync),
) -> Vec<Result<Value>> {
    std::thread::scope(|scope| {
        let jobs = targets
            .iter()
            .map(|target| {
                std::thread::Builder::new()
                    .name("homewire-group".into())
                    .spawn_scoped(scope, move || control(target))
            })
            .collect::<Vec<_>>();
        jobs.into_iter()
            .map(|job| match job {
                Ok(handle) => handle.join().unwrap_or_else(|_| {
                    Err(error("Group worker failed; outcome unknown; do not replay"))
                }),
                Err(_) => Err(error("Group worker unavailable; command not sent")),
            })
            .collect()
    })
}

pub(crate) fn apply(devices: &Devices, id: &str, power: bool) -> Result<Value> {
    let targets = devices.groups.targets(devices, id)?; // Resolve every member before any controls.
    let results = parallel(&targets, &|(_, device)| {
        devices.power_device(*device, power)
    });
    let completed = results.iter().all(Result::is_ok);
    let results=targets.iter().zip(results).map(|((entity,_),result)|match result {
        Ok(value)=>json!({"entity":entity,"verified":true,"power":power,"result":value}),
        Err(error)=>json!({"entity":entity,"verified":false,"error":error.to_string().chars().take(512).collect::<String>()}),
    }).collect::<Vec<_>>();
    Ok(
        json!({"group":id,"power":power,"completed":completed,"verified":completed,"execution":"concurrent_best_effort","results":results,"automatic_replay":false}),
    )
}

pub fn call(devices: &Devices, name: &str, args: &Value) -> Result<Value> {
    if name == "group_list" {
        return Ok(devices.groups.list());
    }
    let id = args["group"]
        .as_str()
        .ok_or_else(|| error("Expected group ID"))?;
    let power = args["power"]
        .as_bool()
        .ok_or_else(|| error("Expected absolute group power"))?;
    if name == "group_plan" {
        let members = devices
            .groups
            .targets(devices, id)?
            .iter()
            .map(|(name, _)| *name)
            .collect::<Vec<_>>();
        return Ok(
            json!({"group":id,"power":power,"members":members,"execution":"concurrent_best_effort","live_state_checked":false,"provisional":true,"atomic":false}),
        );
    }
    if name != "group_set" {
        return Err(error("Unknown group tool"));
    }
    apply(devices, id, power)
}

const LIST: Tool = Tool {
    name: "group_list",
    description: "Discover explicitly configured device groups and their light members without device I/O.",
    input: Input::Empty,
    hints: Hints::LIST,
};
const PLAN: Tool = Tool {
    name: "group_plan",
    description: "Preview absolute group light power and membership without device I/O. Commands run concurrently; synchronization is best effort, not atomic.",
    input: Input::Named {
        key: "group",
        extra: Some(entity::POWER),
        error: "Expected group ID and absolute power",
    },
    hints: Hints::READ,
};
const SET: Tool = Tool {
    name: "group_set",
    description: "Set absolute power for every explicit light-group member concurrently and verify each. Preserve brightness/colour. Partial completion is not success; never replay uncertain members automatically.",
    input: Input::Named {
        key: "group",
        extra: Some(entity::POWER),
        error: "Expected group ID and absolute power",
    },
    hints: Hints {
        idempotent: false,
        ..Hints::CONTROL
    },
};
pub fn tools(selection: Selection) -> impl Iterator<Item = &'static Tool> + Clone {
    [&LIST, &PLAN, &SET]
        .into_iter()
        .filter(move |_| selection.support().contains(Support::LIGHT))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn independent_controls_overlap_and_partial_failures_preserve_order() {
        let barrier = std::sync::Barrier::new(3);
        let result = parallel(&[0, 1, 2], &|index| {
            barrier.wait();
            if *index == 1 {
                Err(error("synthetic uncertain response"))
            } else {
                Ok(json!({"index":index}))
            }
        });
        assert_eq!(result[0].as_ref().expect("synthetic fixture")["index"], 0);
        assert!(result[1].is_err());
        assert_eq!(result[2].as_ref().expect("synthetic fixture")["index"], 2);
    }
}
