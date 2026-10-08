//! Household behavior composed above physical-device capabilities.
//! Named routines preserve the existing verified sequences and durable guards.
use crate::{
    Result,
    device::{Devices, Hints, Input, Selection, Tool},
    error,
};
use serde_json::{Value, json};
pub mod meaco;
pub mod tv;

fn configured(selection: Selection, name: &str) -> bool {
    match name {
        "meaco_day" | "meaco_night" | "meaco_off" => selection.meaco,
        "tv_on" | "tv_off" => selection.tv,
        _ => false,
    }
}
pub fn call(devices: &mut Devices, tool: &str, args: &Value) -> Result<Value> {
    if tool == "routine_list" {
        let selection = devices.selection();
        let mut routines = Vec::new();
        for (name, description) in [
            ("meaco_day", "Normal mode at 55% humidity"),
            ("meaco_night", "Purify base mode followed by sleep"),
            ("meaco_off", "Verified dehumidifier power off"),
            (
                "tv_on",
                "Outlet first, Samsung wake, DIGI ready and verified wake",
            ),
            (
                "tv_off",
                "Verified DIGI sleep, Samsung shutdown once, explicit fixed-delay outlet cutoff",
            ),
        ] {
            if configured(selection, name) {
                routines.push(json!({"routine":name,"description":description}));
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
    if !configured(devices.selection(), name) {
        return Err(error("Unknown configured routine"));
    }
    let result = if let Some(operation) = name.strip_prefix("meaco_") {
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
        json!({"power":name == "tv_on","live_state_checked":false,"provisional":true,"shutdown_policy":"fixed_delay","physical_samsung_off_verified":false,"note":"Execution checks live state; failures abort the sequence and uncertain controls are not replayed.","steps":if name == "tv_on" {vec!["outlet_on_if_needed","wait_5s_if_outlet_changed","samsung_wake_verify","wait_digi_ready_up_to_120s","digi_wake_verify"]} else {vec!["if_outlet_on_digi_sleep_verify","samsung_shutdown_once","wait_5s","outlet_off_verify"]}})
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
