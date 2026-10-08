//! Minimal MCP stdio server. One JSON-RPC message per line, max 64 KiB.
//! Device operations are serialized by the caller; no web server is needed.
use crate::{Result, device::Selection, error, meaco::Operation, naming};
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
/// Compatibility entry point for callers using the original fixed ToolSet.
pub fn handle_configured(
    request: Value,
    tools: ToolSet,
    call: impl FnMut(&str, &Value) -> Result<Value>,
) -> Option<Value> {
    handle_selected(
        request,
        Selection::from_filter(|family| match family {
            "meaco" => tools.meaco,
            "printers" => tools.printers,
            "awair" => tools.awair,
            "bulbs" => tools.bulbs,
            "plugs" => tools.plugs,
            _ => false,
        }),
        Options {
            light_names: tools.light_names,
            allow_control: tools.allow_control,
        },
        call,
    )
}

#[derive(Clone, Copy, Default)]
pub struct Options {
    pub light_names: bool,
    pub allow_control: bool,
}

/// MCP knows framing, permission policy and presentation, not adapter families.
pub fn handle_selected(
    request: Value,
    selection: Selection,
    options: Options,
    call: impl FnMut(&str, &Value) -> Result<Value>,
) -> Option<Value> {
    handle_tools(request, selection.tools(), options, true, call)
}

/// Standard capability/routine interface. Legacy adapter tools are opt-in.
pub fn handle_entities(
    request: Value,
    devices: &mut crate::device::Devices,
    allow_control: bool,
) -> Option<Value> {
    handle_capabilities(request, devices.selection(), allow_control, |name, args| {
        crate::entity::call(devices, name, args)
    })
}
pub fn handle_capabilities(
    request: Value,
    selection: Selection,
    allow_control: bool,
    call: impl FnMut(&str, &Value) -> Result<Value>,
) -> Option<Value> {
    handle_tools(
        request,
        crate::entity::tools(selection),
        Options {
            allow_control,
            light_names: false,
        },
        false,
        call,
    )
}
fn handle_tools(
    mut request: Value,
    tools: impl Iterator<Item = &'static crate::device::Tool> + Clone,
    options: Options,
    legacy: bool,
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
                "instructions":"Configured home capabilities and deterministic routines. Discover entities/routines before acting. Unknown or unavailable state never proves power-off or recovery. Controls require explicit permission, identity checks and verification; uncertain controls are never automatically replayed. TV routine-owned power requires maintenance access for direct controls. Monitoring events go to stderr."})
        }
        "ping" => json!({}),
        "tools/list" => {
            let mut result = json!({"tools":null});
            result["tools"] = Value::Array(tool_list(tools.clone(), options));
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
            let name = if legacy {
                naming::canonical_tool(raw_name)
            } else {
                raw_name
            };
            let Some(tool) = tools.clone().find(|tool| tool.name == name) else {
                return Some(rpc_error(id, -32602, "Unknown tool"));
            };
            let args = match if legacy {
                naming::light_arguments(raw_name, args)
            } else {
                Ok(args)
            } {
                Ok(args) => args,
                Err(_) => return Some(rpc_error(id, -32602, "Use light for light tools")),
            };
            if let Err(message) = tool.input.validate(&args) {
                return Some(rpc_error(id, -32602, message));
            }
            if !tool.hints.read_only && !options.allow_control {
                tool_error("Device control is disabled; restart with --allow-control")
            } else {
                match call(name, &args) {
                    Ok(value) => {
                        let value = if legacy && raw_name.starts_with("light_") {
                            naming::light_output(value)
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
fn tool_list(
    catalog: impl Iterator<Item = &'static crate::device::Tool> + Clone,
    options: Options,
) -> Vec<Value> {
    let visible = |tool: &&crate::device::Tool| tool.hints.read_only || options.allow_control;
    let mut tools = Vec::with_capacity(catalog.clone().filter(visible).count());
    for tool in catalog.filter(visible) {
        let advertised_name = if options.light_names {
            match tool.name {
                "bulb_list" => "light_list",
                "bulb_status" => "light_status",
                "bulb_check" => "light_check",
                "bulb_apply" => "light_apply",
                other => other,
            }
        } else {
            tool.name
        };
        let key = if options.light_names && tool.name.starts_with("bulb_") {
            Some("light")
        } else {
            None
        };
        let mut value = json!({"name":advertised_name,"description":tool.description});
        value["inputSchema"] = tool.input.schema(key);
        value["annotations"] =
            serde_json::to_value(tool.hints).expect("static tool hints serialize");
        tools.push(value);
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
