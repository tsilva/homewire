//! Presentation-only compatibility aliases for the original bulb names.
use crate::{Result, error};
use serde_json::Value;

// The original bulb interface remains usable; the shared light interface names
// both bulbs and strips consistently without registering duplicate MCP tools.
pub fn canonical_tool(name: &str) -> &str {
    match name {
        "light_list" => "bulb_list",
        "light_status" => "bulb_status",
        "light_apply" => "bulb_apply",
        "light_check" => "bulb_check",
        _ => name,
    }
}
pub fn light_arguments(name: &str, mut args: Value) -> Result<Value> {
    if name.starts_with("light_")
        && let Some(object) = args.as_object_mut()
    {
        if object.contains_key("bulb") {
            return Err(error("Light tools require light, not bulb"));
        }
        if let Some(value) = object.remove("light") {
            object.insert("bulb".into(), value);
        }
    }
    Ok(args)
}
pub fn light_output(mut value: Value) -> Value {
    match &mut value {
        Value::Object(object) => {
            if let Some(name) = object.remove("bulb") {
                object.insert("light".into(), name);
            }
            if let Some(lights) = object.remove("bulbs") {
                object.insert("lights".into(), lights);
            }
            for item in object.values_mut() {
                *item = light_output(item.take());
            }
        }
        Value::Array(items) => {
            for item in items {
                *item = light_output(item.take());
            }
        }
        _ => {}
    }
    value
}
