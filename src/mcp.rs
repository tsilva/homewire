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
    mut request: Value,
    tools: ToolSet,
    mut call: impl FnMut(&str, &Value) -> Result<Value>,
) -> Option<Value> {
    let id = request.get_mut("id").map(Value::take);
    let method = request.get("method").and_then(Value::as_str);
    let valid_id = id
        .as_ref()
        .is_none_or(|v| v.is_string() || v.is_number() || v.is_null());
    if request.get("jsonrpc").and_then(Value::as_str) != Some("2.0")
        || method.is_none()
        || !valid_id
    {
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
        "tools/list" => {
            let mut result = json!({"tools":null});
            result["tools"] = Value::Array(tool_list(tools));
            result
        }
        "tools/call" => {
            let args = request
                .pointer_mut("/params/arguments")
                .map(Value::take)
                .unwrap_or_else(|| json!({}));
            let raw_name = request
                .pointer("/params/name")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let name = bulb::canonical_tool(raw_name);
            let Some(tool) = TOOLS
                .iter()
                .find(|tool| tool.0 == name && enabled(tool.0, tools))
            else {
                return Some(rpc_error(id, -32602, "Unknown tool"));
            };
            let args = match bulb::light_arguments(raw_name, args) {
                Ok(args) => args,
                Err(_) => return Some(rpc_error(id, -32602, "Use light for light tools")),
            };
            if let Err(message) = tool.2.validate(&args) {
                return Some(rpc_error(id, -32602, message));
            }
            if name.ends_with("_apply") && !tools.allow_control {
                tool_error("Device control is disabled; restart with --allow-control")
            } else {
                match call(name, &args) {
                    Ok(value) => {
                        let value = if raw_name.starts_with("light_") {
                            bulb::light_output(value)
                        } else {
                            value
                        };
                        let mut result =
                            json!({"content":[{"type":"text","text":null}],"isError":false});
                        result["content"][0]["text"] = Value::String(value.to_string());
                        result["structuredContent"] = value;
                        result
                    }
                    Err(e) => tool_error(&e.to_string()),
                }
            }
        }
        _ => return Some(rpc_error(id, -32601, "Method not found")),
    };
    let mut response = json!({"jsonrpc":"2.0","id":null,"result":null});
    response["id"] = id;
    response["result"] = result;
    Some(response)
}
// Advertising, argument validation and dispatch share this bounded registry.
const TOOLS: &[(&str, &str, Arguments)] = &[
    (
        "meaco_status",
        "Read live Meaco power, mode, humidity and fault status.",
        Arguments::Empty,
    ),
    (
        "meaco_plan",
        "Preview the day/night/off sequence using live state. Sends no controls.",
        Arguments::Operation,
    ),
    (
        "meaco_apply",
        "Apply a verified Meaco sequence: day=normal/55%; night=purify then sleep; off=power off.",
        Arguments::Operation,
    ),
    (
        "bulb_list",
        "List configured Tapo L430C/L900 light names and models without network queries or secrets.",
        Arguments::Empty,
    ),
    (
        "bulb_status",
        "Read live bulb power, brightness, colour, white temperature and reported overheating.",
        Arguments::Named("bulb"),
    ),
    (
        "bulb_check",
        "Poll all bulbs and return debounced overheating/recovery events and retained faults. Missing readings never clear faults.",
        Arguments::Empty,
    ),
    (
        "bulb_apply",
        "Apply absolute bulb settings and verify them with a fresh read. No toggles or automatic retries. Brightness/colour do not implicitly turn power on.",
        Arguments::Settings,
    ),
    (
        "plug_list",
        "List configured Tapo P100/P110 plugs without network queries or credentials.",
        Arguments::Empty,
    ),
    (
        "plug_status",
        "Read live relay, overload, auto-off timer state and P110 energy (W, V, A, kWh). Missing readings remain unknown.",
        Arguments::Named("plug"),
    ),
    (
        "plug_check",
        "Poll plug relays and debounced reported overload/recovery events. Missing readings never clear retained faults. No energy queries.",
        Arguments::Empty,
    ),
    (
        "plug_apply",
        "Apply absolute plug power and verify read-back. No toggles or control retries. Turning off may interrupt connected equipment. P110 overload/unknown faults block power-on.",
        Arguments::Power,
    ),
    (
        "awair_status",
        "Read fresh Awair Element measurements: temperature (Celsius), humidity (%), CO2 (ppm), TVOC (ppb), PM2.5 (ug/m3), score, and timestamps. Invalid/stale readings fail. No device controls.",
        Arguments::Empty,
    ),
    (
        "printer_list",
        "List configured printers without network queries.",
        Arguments::Empty,
    ),
    (
        "printer_check",
        "Poll all printers and return debounced fault/recovery events plus retained faults. Unavailable readings never clear faults. Use repeatedly in this server for monitoring.",
        Arguments::Empty,
    ),
    (
        "printer_status",
        "Read live printer state, reported fault reasons, job count, and ink/toner levels using IPP. Sends no print jobs or controls.",
        Arguments::Named("printer"),
    ),
];

#[derive(Clone, Copy)]
enum Arguments {
    Empty,
    Operation,
    Named(&'static str),
    Settings,
    Power,
}
impl Arguments {
    fn key(self) -> Option<&'static str> {
        match self {
            Self::Named(key) => Some(key),
            Self::Settings => Some("bulb"),
            Self::Power => Some("plug"),
            _ => None,
        }
    }
    fn validate(self, args: &Value) -> std::result::Result<(), &'static str> {
        let object = args.as_object().ok_or("Arguments must be an object")?;
        match self {
            Self::Empty => {
                if !object.is_empty() {
                    return Err("This tool takes no arguments");
                }
            }
            Self::Operation => {
                if object.len() != 1 {
                    return Err("Expected only operation");
                }
                if object
                    .get("operation")
                    .and_then(Value::as_str)
                    .and_then(|s| s.parse::<Operation>().ok())
                    .is_none()
                {
                    return Err("Operation must be day, night or off");
                }
            }
            _ => {
                let key = self.key().unwrap_or_default();
                let apply = matches!(self, Self::Settings | Self::Power);
                if object.len() != if apply { 2 } else { 1 }
                    || !object
                        .get(key)
                        .and_then(Value::as_str)
                        .is_some_and(|s| !s.is_empty() && s.len() <= 32)
                    || matches!(self, Self::Power)
                        && !object.get("power").is_some_and(Value::is_boolean)
                {
                    return Err(match key {
                        "bulb" => "Expected a configured bulb name and settings for apply",
                        "plug" => "Expected configured plug name and boolean power for apply",
                        _ => "Expected only a configured printer name",
                    });
                }
                if matches!(self, Self::Settings)
                    && !object.get("settings").is_some_and(|v| {
                        <bulb::Change as serde::Deserialize>::deserialize(v)
                            .is_ok_and(|c| c.validate().is_ok())
                    })
                {
                    return Err("Invalid bulb settings");
                }
            }
        }
        Ok(())
    }
    fn schema(self, light_names: bool) -> Value {
        let mut schema = json!({"type":"object","properties":{},"additionalProperties":false});
        if matches!(self, Self::Operation) {
            schema["properties"]["operation"] =
                json!({"type":"string","enum":["day","night","off"]});
            schema["required"] = json!(["operation"]);
        } else if let Some(key) = self.key() {
            let key = if key == "bulb" && light_names {
                "light"
            } else {
                key
            };
            schema["properties"][key] = json!({"type":"string","minLength":1,"maxLength":32});
            let mut required =
                Vec::with_capacity(if matches!(self, Self::Settings | Self::Power) {
                    2
                } else {
                    1
                });
            required.push(Value::String(key.into()));
            match self {
                Self::Settings => {
                    schema["properties"]["settings"] = json!({"type":"object","properties":{"power":{"type":"boolean"},"brightness_percent":{"type":"integer","minimum":1,"maximum":100},"hue":{"type":"integer","minimum":0,"maximum":360},"saturation_percent":{"type":"integer","minimum":0,"maximum":100},"color_temperature_k":{"type":"integer","minimum":2500,"maximum":9000}},"minProperties":1,"additionalProperties":false,"dependentRequired":{"hue":["saturation_percent"],"saturation_percent":["hue"]}});
                    required.push(Value::String("settings".into()));
                }
                Self::Power => {
                    schema["properties"]["power"] = json!({"type":"boolean"});
                    required.push(Value::String("power".into()));
                }
                _ => {}
            }
            schema["required"] = Value::Array(required);
        }
        schema
    }
}
fn enabled(name: &str, config: ToolSet) -> bool {
    match name.split_once('_').map(|(family, _)| family) {
        Some("meaco") => config.meaco,
        Some("bulb") => config.bulbs,
        Some("plug") => config.plugs,
        Some("awair") => config.awair,
        Some("printer") => config.printers,
        _ => false,
    }
}
fn tool_list(config: ToolSet) -> Vec<Value> {
    let visible =
        |name: &str| enabled(name, config) && (!name.ends_with("_apply") || config.allow_control);
    let mut tools = Vec::with_capacity(TOOLS.iter().filter(|t| visible(t.0)).count());
    for &(name, description, arguments) in TOOLS.iter().filter(|t| visible(t.0)) {
        let apply = name.ends_with("_apply");
        let meaco = name.starts_with("meaco_");
        let mut annotations = json!({"readOnlyHint":!apply,
            "idempotentHint":if meaco { name == "meaco_status" } else { !name.ends_with("_check") },
            "openWorldHint":!name.ends_with("_list")});
        if !meaco {
            annotations["destructiveHint"] = Value::Bool(name == "plug_apply");
        }
        let advertised_name = if config.light_names {
            match name {
                "bulb_list" => "light_list",
                "bulb_status" => "light_status",
                "bulb_check" => "light_check",
                "bulb_apply" => "light_apply",
                _ => name,
            }
        } else {
            name
        };
        let mut tool = json!({"name":advertised_name,"description":description});
        // Move schemas and annotations: json! on an existing Value clones its tree.
        tool["inputSchema"] = arguments.schema(config.light_names);
        tool["annotations"] = annotations;
        tools.push(tool);
    }
    tools
}
fn tool_error(message: &str) -> Value {
    json!({"content":[{"type":"text","text":message}],"isError":true})
}
pub fn rpc_error(id: Value, code: i32, message: &str) -> Value {
    let mut response = json!({"jsonrpc":"2.0","id":null,"error":{"code":code,"message":message}});
    response["id"] = id;
    response
}
pub fn write_message(writer: &mut impl Write, message: &impl serde::Serialize) -> Result<()> {
    serde_json::to_writer(&mut *writer, message)?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    Ok(())
}
