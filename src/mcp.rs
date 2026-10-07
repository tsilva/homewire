//! Minimal MCP stdio server. One JSON-RPC message per line, max 64 KiB.
//! Device operations are serialized by the caller; no web server is needed.
use crate::{Result, error, meaco::Operation};
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

pub fn handle(
    request: Value,
    allow_control: bool,
    mut call: impl FnMut(&str, Option<Operation>) -> Result<Value>,
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
                "instructions":"Meaco LAN controls. Plan reads device status without changes. Apply verifies each step and never retries an uncertain sleep toggle."})
        }
        "ping" => json!({}),
        "tools/list" => json!({"tools":tools(allow_control)}),
        "tools/call" => {
            let name = request
                .pointer("/params/name")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if !matches!(name, "meaco_status" | "meaco_plan" | "meaco_apply") {
                return Some(rpc_error(id, -32602, "Unknown tool"));
            }
            let args = request
                .pointer("/params/arguments")
                .cloned()
                .unwrap_or(json!({}));
            let Some(object) = args.as_object() else {
                return Some(rpc_error(id, -32602, "Arguments must be an object"));
            };
            let mut operation = None;
            if name == "meaco_status" {
                if !object.is_empty() {
                    return Some(rpc_error(id, -32602, "Status takes no arguments"));
                }
            } else {
                if object.len() != 1 {
                    return Some(rpc_error(id, -32602, "Expected only operation"));
                }
                operation = object
                    .get("operation")
                    .and_then(Value::as_str)
                    .and_then(|s| s.parse().ok());
                if operation.is_none() {
                    return Some(rpc_error(id, -32602, "Operation must be day, night or off"));
                }
            }
            if name == "meaco_apply" && !allow_control {
                tool_error("Device control is disabled; restart with --allow-control")
            } else {
                match call(name, operation) {
                    Ok(value) => {
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
fn tools(allow_control: bool) -> Vec<Value> {
    let mut tools = vec![
        json!({"name":"meaco_status","description":"Read live Meaco power, mode, humidity and fault status.",
        "inputSchema":{"type":"object","properties":{},"additionalProperties":false},
        "annotations":{"readOnlyHint":true,"idempotentHint":true,"openWorldHint":true}}),
    ];
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
        if name == "meaco_apply" && !allow_control {
            continue;
        }
        tools.push(json!({"name":name,"description":description,
            "inputSchema":{"type":"object","properties":{"operation":{"type":"string","enum":["day","night","off"]}},"required":["operation"],"additionalProperties":false},
            "annotations":{"readOnlyHint":read_only,"idempotentHint":false,"openWorldHint":true}}));
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
