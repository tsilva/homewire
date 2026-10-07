//! Minimal MCP stdio server. One JSON-RPC message per line, max 64 KiB.
//! Device operations are serialized by the caller; no web server is needed.
use crate::{Result, bulb, error, meaco::Operation};
use serde_json::{Value, json};
use std::io::{BufRead, Read, Write};

pub const MAX_MESSAGE: usize = 64 * 1024;
pub fn read_message(reader: &mut impl BufRead) -> Result<Option<String>> {
    let mut bytes = Vec::new();
    let count = (&mut *reader)
        .take((MAX_MESSAGE + 1) as u64)
        .read_until(b'\n', &mut bytes)?;
    if count == 0 {
        return Ok(None);
    }
    if count > MAX_MESSAGE {
        return Err(error("MCP message exceeds 64 KiB"));
    }
    Ok(Some(
        String::from_utf8(bytes).map_err(|_| error("MCP requires UTF-8"))?,
    ))
}

#[derive(Clone, Copy)]
pub struct ToolSet {
    pub meaco: bool,
    pub printers: bool,
    pub awair: bool,
    pub bulbs: bool,
    pub plugs: bool,
    pub light_names: bool,
    pub allow_control: bool,
}

// Preserve the original Meaco-only interface for existing callers.
pub fn handle(
    request: Value,
    allow_control: bool,
    mut call: impl FnMut(&str, Option<Operation>) -> Result<Value>,
) -> Option<Value> {
    handle_configured(
        request,
        ToolSet {
            meaco: true,
            printers: false,
            awair: false,
            bulbs: false,
            plugs: false,
            light_names: false,
            allow_control,
        },
        |name, args| {
            call(
                name,
                args.get("operation")
                    .and_then(Value::as_str)
                    .and_then(|s| s.parse().ok()),
            )
        },
    )
}
pub fn handle_configured(
    request: Value,
    tools: ToolSet,
    mut call: impl FnMut(&str, &Value) -> Result<Value>,
) -> Option<Value> {
    let id = request.get("id").cloned();
    let method = request.get("method").and_then(Value::as_str);
    let valid_id = id
        .as_ref()
        .is_none_or(|v| v.is_string() || v.is_number() || v.is_null());
    if request.get("jsonrpc") != Some(&json!("2.0")) || method.is_none() || !valid_id {
        return Some(rpc_error(
            if valid_id {
                id.unwrap_or(Value::Null)
            } else {
                Value::Null
            },
            -32600,
            "Invalid JSON-RPC request",
        ));
    }
    // Well-formed notifications, including initialized/cancelled, have no response.
    let id = id?;
    let result = match method.unwrap_or_default() {
        "initialize" => {
            let wanted = request
                .pointer("/params/protocolVersion")
                .and_then(Value::as_str)
                .unwrap_or("2025-11-25");
            let version = if matches!(
                wanted,
                "2024-11-05" | "2025-03-26" | "2025-06-18" | "2025-11-25"
            ) {
                wanted
            } else {
                "2025-11-25"
            };
            json!({"protocolVersion":version,"capabilities":{"tools":{"listChanged":false}},
                "serverInfo":{"name":"homewire","version":env!("CARGO_PKG_VERSION")},
                "instructions":"Configured home device tools. Plug apply uses absolute relay power and verifies outcomes; P110 overload checks retain faults and energy reads preserve units. Bulb apply uses absolute settings and verifies outcomes; bulb checks retain overheating faults across missing readings. Printer tools read IPP status only. Awair returns fresh local sensor measurements with explicit units; invalid or stale readings fail. Meaco apply verifies each step and never retries an uncertain sleep toggle. Printer checks retain faults across missing readings and debounce transitions."})
        }
        "ping" => json!({}),
        "tools/list" => json!({"tools":tool_list(tools)}),
        "tools/call" => {
            let raw_name = request
                .pointer("/params/name")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let name = bulb::canonical_tool(raw_name);
            if !(tools.meaco && matches!(name, "meaco_status" | "meaco_plan" | "meaco_apply")
                || tools.printers
                    && matches!(name, "printer_list" | "printer_status" | "printer_check")
                || tools.plugs
                    && matches!(
                        name,
                        "plug_list" | "plug_status" | "plug_check" | "plug_apply"
                    )
                || tools.awair && name == "awair_status"
                || tools.bulbs
                    && matches!(
                        name,
                        "bulb_list" | "bulb_status" | "bulb_check" | "bulb_apply"
                    ))
            {
                return Some(rpc_error(id, -32602, "Unknown tool"));
            }
            let args = request
                .pointer("/params/arguments")
                .cloned()
                .unwrap_or(json!({}));
            let args = match bulb::light_arguments(raw_name, args) {
                Ok(args) => args,
                Err(_) => return Some(rpc_error(id, -32602, "Use light for light tools")),
            };
            let Some(object) = args.as_object() else {
                return Some(rpc_error(id, -32602, "Arguments must be an object"));
            };
            if matches!(
                name,
                "meaco_status"
                    | "printer_list"
                    | "printer_check"
                    | "awair_status"
                    | "bulb_list"
                    | "bulb_check"
                    | "plug_list"
                    | "plug_check"
            ) {
                if !object.is_empty() {
                    return Some(rpc_error(id, -32602, "This tool takes no arguments"));
                }
            } else if matches!(name, "plug_status" | "plug_apply") {
                let expected = if name == "plug_apply" { 2 } else { 1 };
                if object.len() != expected
                    || !object
                        .get("plug")
                        .is_some_and(|v| v.as_str().is_some_and(|s| !s.is_empty() && s.len() <= 32))
                    || name == "plug_apply" && !object.get("power").is_some_and(Value::is_boolean)
                {
                    return Some(rpc_error(
                        id,
                        -32602,
                        "Expected configured plug name and boolean power for apply",
                    ));
                }
            } else if matches!(name, "bulb_status" | "bulb_apply") {
                let expected = if name == "bulb_apply" { 2 } else { 1 };
                if object.len() != expected
                    || !object
                        .get("bulb")
                        .is_some_and(|v| v.as_str().is_some_and(|s| !s.is_empty() && s.len() <= 32))
                {
                    return Some(rpc_error(
                        id,
                        -32602,
                        "Expected a configured bulb name and settings for apply",
                    ));
                }
                if name == "bulb_apply"
                    && !object.get("settings").is_some_and(|v| {
                        serde_json::from_value::<crate::bulb::Change>(v.clone())
                            .is_ok_and(|c| c.validate().is_ok())
                    })
                {
                    return Some(rpc_error(id, -32602, "Invalid bulb settings"));
                }
            } else if name == "printer_status" {
                if object.len() != 1
                    || !object
                        .get("printer")
                        .is_some_and(|v| v.as_str().is_some_and(|s| !s.is_empty() && s.len() <= 32))
                {
                    return Some(rpc_error(
                        id,
                        -32602,
                        "Expected only a configured printer name",
                    ));
                }
            } else {
                if object.len() != 1 {
                    return Some(rpc_error(id, -32602, "Expected only operation"));
                }
                let operation: Option<Operation> = object
                    .get("operation")
                    .and_then(Value::as_str)
                    .and_then(|s| s.parse().ok());
                if operation.is_none() {
                    return Some(rpc_error(id, -32602, "Operation must be day, night or off"));
                }
            }
            if matches!(name, "meaco_apply" | "bulb_apply" | "plug_apply") && !tools.allow_control {
                tool_error("Device control is disabled; restart with --allow-control")
            } else {
                match call(name, &args) {
                    Ok(value) => {
                        let value = if raw_name.starts_with("light_") {
                            bulb::light_output(value)
                        } else {
                            value
                        };
                        json!({"content":[{"type":"text","text":value.to_string()}],"structuredContent":value,"isError":false})
                    }
                    Err(e) => tool_error(&e.to_string()),
                }
            }
        }
        _ => return Some(rpc_error(id, -32601, "Method not found")),
    };
    Some(json!({"jsonrpc":"2.0","id":id,"result":result}))
}
fn tool_list(config: ToolSet) -> Vec<Value> {
    let mut tools = if config.meaco {
        vec![
            json!({"name":"meaco_status","description":"Read live Meaco power, mode, humidity and fault status.",
        "inputSchema":{"type":"object","properties":{},"additionalProperties":false},
        "annotations":{"readOnlyHint":true,"idempotentHint":true,"openWorldHint":true}}),
        ]
    } else {
        vec![]
    };
    for (name, description, read_only) in [
        (
            "meaco_plan",
            "Preview the day/night/off sequence using live state. Sends no controls.",
            true,
        ),
        (
            "meaco_apply",
            "Apply a verified Meaco sequence: day=normal/55%; night=purify then sleep; off=power off.",
            false,
        ),
    ] {
        if !config.meaco || name == "meaco_apply" && !config.allow_control {
            continue;
        }
        tools.push(json!({"name":name,"description":description,
            "inputSchema":{"type":"object","properties":{"operation":{"type":"string","enum":["day","night","off"]}},"required":["operation"],"additionalProperties":false},
            "annotations":{"readOnlyHint":read_only,"idempotentHint":false,"openWorldHint":true}}));
    }
    if config.bulbs {
        for (name, description) in [
            (
                "bulb_list",
                "List configured Tapo L430C/L900 light names and models without network queries or secrets.",
            ),
            (
                "bulb_status",
                "Read live bulb power, brightness, colour, white temperature and reported overheating.",
            ),
            (
                "bulb_check",
                "Poll all bulbs and return debounced overheating/recovery events and retained faults. Missing readings never clear faults.",
            ),
            (
                "bulb_apply",
                "Apply absolute bulb settings and verify them with a fresh read. No toggles or automatic retries. Brightness/colour do not implicitly turn power on.",
            ),
        ] {
            if name == "bulb_apply" && !config.allow_control {
                continue;
            }
            let schema = match name {
                "bulb_status" => {
                    json!({"type":"object","properties":{"bulb":{"type":"string","minLength":1,"maxLength":32}},"required":["bulb"],"additionalProperties":false})
                }
                "bulb_apply" => {
                    json!({"type":"object","properties":{"bulb":{"type":"string","minLength":1,"maxLength":32},"settings":{"type":"object","properties":{"power":{"type":"boolean"},"brightness_percent":{"type":"integer","minimum":1,"maximum":100},"hue":{"type":"integer","minimum":0,"maximum":360},"saturation_percent":{"type":"integer","minimum":0,"maximum":100},"color_temperature_k":{"type":"integer","minimum":2500,"maximum":9000}},"minProperties":1,"additionalProperties":false,"dependentRequired":{"hue":["saturation_percent"],"saturation_percent":["hue"]}}},"required":["bulb","settings"],"additionalProperties":false})
                }
                _ => json!({"type":"object","properties":{},"additionalProperties":false}),
            };
            let advertised_name = if config.light_names {
                name.replacen("bulb_", "light_", 1)
            } else {
                name.to_owned()
            };
            let mut schema = schema;
            if config.light_names
                && let Some(properties) =
                    schema.get_mut("properties").and_then(Value::as_object_mut)
            {
                if let Some(property) = properties.remove("bulb") {
                    properties.insert("light".into(), property);
                }
                if let Some(required) = schema.get_mut("required").and_then(Value::as_array_mut) {
                    for key in required {
                        if key == "bulb" {
                            *key = json!("light");
                        }
                    }
                }
            }
            tools.push(json!({"name":advertised_name,"description":description,"inputSchema":schema,
                "annotations":{"readOnlyHint":name!="bulb_apply","destructiveHint":false,"idempotentHint":name!="bulb_check","openWorldHint":name!="bulb_list"}}));
        }
    }
    if config.plugs {
        for (name, description) in [
            (
                "plug_list",
                "List configured Tapo P100/P110 plugs without network queries or credentials.",
            ),
            (
                "plug_status",
                "Read live relay, overload, auto-off timer state and P110 energy (W, V, A, kWh). Missing readings remain unknown.",
            ),
            (
                "plug_check",
                "Poll plug relays and debounced reported overload/recovery events. Missing readings never clear retained faults. No energy queries.",
            ),
            (
                "plug_apply",
                "Apply absolute plug power and verify read-back. No toggles or control retries. Turning off may interrupt connected equipment. P110 overload/unknown faults block power-on.",
            ),
        ] {
            if name == "plug_apply" && !config.allow_control {
                continue;
            }
            let schema = match name {
                "plug_status" => {
                    json!({"type":"object","properties":{"plug":{"type":"string","minLength":1,"maxLength":32}},"required":["plug"],"additionalProperties":false})
                }
                "plug_apply" => {
                    json!({"type":"object","properties":{"plug":{"type":"string","minLength":1,"maxLength":32},"power":{"type":"boolean"}},"required":["plug","power"],"additionalProperties":false})
                }
                _ => json!({"type":"object","properties":{},"additionalProperties":false}),
            };
            tools.push(json!({"name":name,"description":description,"inputSchema":schema,"annotations":{"readOnlyHint":name!="plug_apply","destructiveHint":name=="plug_apply","idempotentHint":name!="plug_check","openWorldHint":name!="plug_list"}}));
        }
    }
    if config.awair {
        tools.push(json!({"name":"awair_status",
            "description":"Read fresh Awair Element measurements: temperature (Celsius), humidity (%), CO2 (ppm), TVOC (ppb), PM2.5 (ug/m3), score, and timestamps. Invalid/stale readings fail. No device controls.",
            "inputSchema":{"type":"object","properties":{},"additionalProperties":false},
            "annotations":{"readOnlyHint":true,"destructiveHint":false,"idempotentHint":true,"openWorldHint":true}}));
    }
    if config.printers {
        for (name, description) in [
            (
                "printer_list",
                "List configured printers without network queries.",
            ),
            (
                "printer_check",
                "Poll all printers and return debounced fault/recovery events plus retained faults. Unavailable readings never clear faults. Use repeatedly in this server for monitoring.",
            ),
            (
                "printer_status",
                "Read live printer state, reported fault reasons, job count, and ink/toner levels using IPP. Sends no print jobs or controls.",
            ),
        ] {
            let schema = if name == "printer_status" {
                json!({"type":"object","properties":{"printer":{"type":"string","minLength":1,"maxLength":32}},"required":["printer"],"additionalProperties":false})
            } else {
                json!({"type":"object","properties":{},"additionalProperties":false})
            };
            tools.push(json!({"name":name,"description":description,"inputSchema":schema,
                "annotations":{"readOnlyHint":true,"destructiveHint":false,"idempotentHint":name!="printer_check","openWorldHint":name!="printer_list"}}));
        }
    }
    tools
}
fn tool_error(message: &str) -> Value {
    json!({"content":[{"type":"text","text":message}],"isError":true})
}
pub fn rpc_error(id: Value, code: i32, message: &str) -> Value {
    json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}})
}
pub fn write_message(writer: &mut impl Write, message: &Value) -> Result<()> {
    serde_json::to_writer(&mut *writer, message)?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    Ok(())
}
