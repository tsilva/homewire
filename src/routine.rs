//! Household behavior composed above physical-device capabilities.
//! Named routines preserve the existing verified sequences and durable guards.
use crate::{
    Result,
    device::{Devices, Hints, Input, Selection, Tool},
    error,
};
use serde_json::{Value, json};
pub mod brightness;
pub mod computer;
pub mod meaco;
pub mod tcl;
pub mod tv;

fn configured(devices: &Devices, name: &str) -> bool {
    match name {
        "meaco_day" | "meaco_night" | "meaco_off" => devices.meaco.as_ref().is_some_and(|m| {
            name.strip_prefix("meaco_")
                .and_then(|s| s.parse().ok())
                .is_some_and(|op| m.has_preset(op))
        }),
        "tv_on" | "tv_off" => devices.tv.is_some() || devices.tcl_routine.is_some(),
        "computer_on" | "computer_off" => devices.computer_routine.is_some(),
        _ => false,
    }
}
pub fn call(devices: &mut Devices, tool: &str, args: &Value) -> Result<Value> {
    if tool == "routine_list" {
        let mut routines = Vec::new();
        for (name, description) in [
            ("meaco_day", "Configured daytime preset"),
            ("meaco_night", "Configured nighttime preset"),
            ("meaco_off", "Verified dehumidifier power off"),
            (
                "computer_on",
                "Outlet first, one Wake-on-LAN packet, authenticated readiness",
            ),
            (
                "computer_off",
                "Signed shutdown once, configured shutdown verification, then verified outlet cutoff; leave_on disables cutoff",
            ),
            (
                "tv_on",
                "Configured outlet-first startup and verified media wake",
            ),
            (
                "tv_off",
                "Configured media shutdown before verified outlet cutoff",
            ),
        ] {
            if configured(devices, name) {
                let mut row = json!({"routine":name,"description":description});
                if let Some(op) = name.strip_prefix("meaco_").and_then(|s| s.parse().ok()) {
                    row["preset"] =
                        serde_json::to_value(devices.meaco.as_ref().and_then(|m| m.preset(op)))?;
                }
                routines.push(row);
            }
        }
        return Ok(json!({"routines":routines}));
    }
    if !matches!(tool, "routine_plan" | "routine_run") {
        return Err(error("Unknown capability tool"));
    }
    let name = args["routine"]
        .as_str()
        .ok_or_else(|| error("Expected named routine"))?;
    if !configured(devices, name) {
        return Err(error("Unknown configured routine"));
    }
    let result = if let Some(operation) = name.strip_prefix("computer_") {
        let mut routine = devices
            .computer_routine
            .take()
            .ok_or_else(|| error("Computer routine is not configured"))?;
        let result = if tool == "routine_plan" {
            Ok(routine.preview(operation == "on"))
        } else {
            routine.run(devices, operation == "on")
        };
        devices.computer_routine = Some(routine);
        result?
    } else if name.starts_with("tv_") && devices.tcl_routine.is_some() {
        let mut routine = devices
            .tcl_routine
            .take()
            .ok_or_else(|| error("TV is not configured"))?;
        let result = if tool == "routine_plan" {
            Ok(routine.preview(name == "tv_on"))
        } else {
            routine.run(devices, name == "tv_on")
        };
        devices.tcl_routine = Some(routine);
        result?
    } else if let Some(operation) = name.strip_prefix("meaco_") {
        devices.call(
            if tool == "routine_plan" {
                "meaco_plan"
            } else {
                "meaco_apply"
            },
            &json!({"operation":operation}),
        )?
    } else if tool == "routine_run" {
        devices.call("tv_apply", &json!({"power":name == "tv_on"}))?
    } else {
        devices
            .tv
            .as_ref()
            .ok_or_else(|| error("TV is not configured"))?
            .preview(name == "tv_on")
    };
    Ok(json!({"routine":name,"result":result}))
}
const INPUT: Input = Input::Named {
    key: "routine",
    extra: None,
    error: "Expected only a configured routine name",
};
const LIST: Tool = Tool {
    name: "routine_list",
    description: "Discover configured deterministic household routines. No controls or device queries.",
    input: Input::Empty,
    hints: Hints::LIST,
};
const PLAN: Tool = Tool {
    name: "routine_plan",
    description: "Preview a named routine without controls. Meaco previews read live state; TV previews describe the configured conditional sequence without querying devices. Plans are provisional.",
    input: INPUT,
    hints: Hints::READ,
};
const RUN: Tool = Tool {
    name: "routine_run",
    description: "Execute a named household routine with verified ordering and durable attempt guards. Never automatically replay an uncertain result.",
    input: INPUT,
    hints: Hints {
        idempotent: false,
        destructive: Some(true),
        ..Hints::CONTROL
    },
};
pub fn tools(selection: Selection) -> impl Iterator<Item = &'static Tool> + Clone {
    let enabled = selection
        .support()
        .contains(crate::entity::Support::ROUTINES);
    [
        enabled.then_some(&LIST),
        enabled.then_some(&PLAN),
        enabled.then_some(&RUN),
    ]
    .into_iter()
    .flatten()
}
