//! Bounded media-screen payloads shared by adapters and MCP presentation.
use crate::{Result, error};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};

// Base64 + metadata fits the existing 1 MiB IPC reply budget, without duplication.
pub(crate) const MAX_JPEG: usize = 512 * 1024;
const MAX_BASE64: usize = MAX_JPEG.div_ceil(3) * 4;
pub(crate) const ACTIONS: &[&str] = &["up", "down", "left", "right", "select", "back", "home"];

pub(crate) fn valid_action(value: &Value) -> bool {
    value
        .as_str()
        .is_some_and(|action| ACTIONS.contains(&action))
}

fn dimensions(bytes: &[u8]) -> Result<(u16, u16)> {
    if bytes.len() > MAX_JPEG
        || !bytes.starts_with(&[0xff, 0xd8])
        || !bytes.ends_with(&[0xff, 0xd9])
    {
        return Err(error("Invalid or oversized screen JPEG"));
    }
    let mut offset = 2;
    while offset + 4 <= bytes.len() {
        if bytes[offset] != 0xff {
            break;
        }
        while offset < bytes.len() && bytes[offset] == 0xff {
            offset += 1;
        }
        let marker = *bytes
            .get(offset)
            .ok_or_else(|| error("Truncated screen JPEG"))?;
        offset += 1;
        let length_bytes = bytes
            .get(offset..offset + 2)
            .ok_or_else(|| error("Truncated screen JPEG"))?;
        let length = usize::from(u16::from_be_bytes([length_bytes[0], length_bytes[1]]));
        if length < 2 || offset + length > bytes.len() {
            break;
        }
        if matches!(marker, 0xc0..=0xc2) {
            if length < 8 || bytes[offset + 2] != 8 {
                break;
            }
            let height = u16::from_be_bytes([bytes[offset + 3], bytes[offset + 4]]);
            let width = u16::from_be_bytes([bytes[offset + 5], bytes[offset + 6]]);
            if width == 0 || height == 0 || width > 1280 || height > 720 {
                break;
            }
            return Ok((width, height));
        }
        if matches!(marker, 0x00 | 0xd8..=0xda | 0xd0..=0xd7) {
            break;
        }
        offset += length;
    }
    Err(error("Invalid or unsupported screen JPEG dimensions"))
}

pub(crate) fn image(bytes: &[u8]) -> Result<Value> {
    let (width, height) = dimensions(bytes)?;
    Ok(
        json!({"mime_type":"image/jpeg","data":STANDARD.encode(bytes),"width":width,"height":height,
        "observed_at":chrono::Utc::now().to_rfc3339()}),
    )
}

/// Remove the image bytes from JSON metadata before emitting one native MCP image.
pub(crate) fn mcp_result(mut value: Value) -> Result<Value> {
    let image = value
        .get_mut("result")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| error("Missing screen image result"))?;
    let data = image
        .remove("data")
        .and_then(|data| match data {
            Value::String(data) => Some(data),
            _ => None,
        })
        .filter(|data| data.len() <= MAX_BASE64)
        .ok_or_else(|| error("Missing or oversized screen image data"))?;
    let bytes = STANDARD
        .decode(&data)
        .map_err(|_| error("Invalid screen image encoding"))?;
    let (width, height) = dimensions(&bytes)?;
    if image.get("mime_type").and_then(Value::as_str) != Some("image/jpeg")
        || image.get("width").and_then(Value::as_u64) != Some(u64::from(width))
        || image.get("height").and_then(Value::as_u64) != Some(u64::from(height))
    {
        return Err(error("Screen image metadata mismatch"));
    }
    Ok(json!({"content":[{"type":"text","text":value.to_string()},
        {"type":"image","mimeType":"image/jpeg","data":data}],"structuredContent":value,"isError":false}))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    pub(crate) fn jpeg(width: u16, height: u16) -> Vec<u8> {
        let mut bytes = vec![0xff, 0xd8, 0xff, 0xc0, 0, 11, 8];
        bytes.extend_from_slice(&height.to_be_bytes());
        bytes.extend_from_slice(&width.to_be_bytes());
        bytes.extend_from_slice(&[1, 1, 0x11, 0, 0xff, 0xd9]);
        bytes
    }
    #[test]
    fn validates_image_size_dimensions_and_metadata() -> Result<()> {
        assert_eq!(dimensions(&jpeg(1280, 720))?, (1280, 720));
        for bytes in [
            jpeg(1281, 720),
            jpeg(1, 721),
            jpeg(0, 1),
            vec![0; MAX_JPEG + 1],
            b"not an image".to_vec(),
        ] {
            assert!(image(&bytes).is_err());
        }
        let mut payload = image(&jpeg(1, 1))?;
        payload["width"] = json!(2);
        assert!(mcp_result(json!({"entity":"media_player.fixture","result":payload})).is_err());
        Ok(())
    }
    #[test]
    fn native_image_is_emitted_once_with_small_text_metadata() -> Result<()> {
        let result =
            mcp_result(json!({"entity":"media_player.fixture","result":image(&jpeg(2,3))?}))?;
        assert_eq!(result["content"][1]["type"], "image");
        assert_eq!(result["content"][1]["mimeType"], "image/jpeg");
        assert!(result["structuredContent"]["result"].get("data").is_none());
        assert!(
            !result["content"][0]["text"]
                .as_str()
                .expect("metadata")
                .contains("data")
        );
        Ok(())
    }
    #[test]
    fn mcp_advertises_reads_and_gates_navigation_before_io() {
        let selection = crate::device::Selection::ALL;
        let list = crate::mcp::handle_capabilities(
            json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}),
            selection,
            false,
            |_, _| panic!("no I/O"),
        )
        .expect("list");
        let tools = list["result"]["tools"].as_array().expect("tools");
        for name in ["media_screenshot", "media_screen_status"] {
            assert!(tools.iter().any(|tool| tool["name"] == name && tool["annotations"]["readOnlyHint"] == true));
        }
        assert!(!tools.iter().any(|tool| tool["name"] == "media_navigate"));
        let request = json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"media_navigate","arguments":{"entity":"media_player.fixture","action":"right"}}});
        let response =
            crate::mcp::handle_capabilities(request.clone(), selection, false, |_, _| {
                panic!("control gate")
            })
            .expect("reply");
        assert_eq!(response["result"]["isError"], true);
        for action in ["power", "RIGHT", "", "right\n"] {
            let mut invalid = request.clone();
            invalid["params"]["arguments"]["action"] = json!(action);
            let response = crate::mcp::handle_capabilities(invalid, selection, true, |_, _| {
                panic!("validation")
            })
            .expect("reply");
            assert_eq!(response["error"]["code"], -32602);
        }
    }
    #[cfg(unix)]
    #[test]
    fn maximum_image_crosses_real_ipc_without_increasing_reply_limit() -> Result<()> {
        use std::{
            io::{BufRead, BufReader, Write},
            os::unix::net::UnixStream,
            time::Duration,
        };
        let (mut client, mut server) = UnixStream::pair()?;
        client.set_read_timeout(Some(Duration::from_secs(3)))?;
        server.set_write_timeout(Some(Duration::from_secs(3)))?;
        let worker = std::thread::spawn(move || -> Result<()> {
            crate::ipc::exchange(&mut server, |request| {
                crate::mcp::handle_capabilities(
                    request,
                    crate::device::Selection::ALL,
                    false,
                    |name, _| {
                        assert_eq!(name, "media_screenshot");
                        let mut bytes = jpeg(1280, 720);
                        bytes.truncate(bytes.len() - 2);
                        bytes.resize(MAX_JPEG - 2, 0);
                        bytes.extend_from_slice(&[0xff, 0xd9]);
                        Ok(json!({"entity":"media_player.fixture","result":image(&bytes)?}))
                    },
                )
            })
        });
        writeln!(
            client,
            "{}",
            json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"media_screenshot","arguments":{"entity":"media_player.fixture"}}})
        )?;
        let mut line = String::new();
        BufReader::new(client).read_line(&mut line)?;
        assert!(line.len() < 1024 * 1024);
        let response: Value = serde_json::from_str(&line)?;
        assert_eq!(response["result"]["isError"], false);
        assert_eq!(
            response["result"]["content"][1]["data"]
                .as_str()
                .expect("image")
                .len(),
            MAX_BASE64
        );
        worker.join().expect("IPC worker")?;
        Ok(())
    }
}
