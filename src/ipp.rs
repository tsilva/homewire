//! Read-only IPP/1.1 Get-Printer-Attributes over bounded HTTP.
use crate::{Result, error};
use serde_json::Value;
use std::{collections::BTreeMap, io::BufRead, net::SocketAddr, time::Duration};

pub use crate::http::MAX_BODY;
pub type Attributes = BTreeMap<String, Vec<Value>>;
const REQUESTED: &[&str] = &[
    "printer-make-and-model",
    "printer-state",
    "printer-state-reasons",
    "printer-state-message",
    "printer-is-accepting-jobs",
    "queued-job-count",
    "marker-names",
    "marker-levels",
    "marker-colors",
    "marker-types",
];

pub fn request(uri: &str) -> Result<Vec<u8>> {
    if uri.len() > 512 {
        return Err(error("Printer URI is too long"));
    }
    let mut bytes = vec![1, 1, 0, 0x0b, 0, 0, 0, 1, 1];
    attribute(&mut bytes, 0x47, "attributes-charset", "utf-8");
    attribute(&mut bytes, 0x48, "attributes-natural-language", "en");
    attribute(&mut bytes, 0x45, "printer-uri", uri);
    for (i, value) in REQUESTED.iter().enumerate() {
        attribute(
            &mut bytes,
            0x44,
            if i == 0 { "requested-attributes" } else { "" },
            value,
        );
    }
    bytes.push(3);
    Ok(bytes)
}
fn attribute(bytes: &mut Vec<u8>, tag: u8, name: &str, value: &str) {
    bytes.push(tag);
    bytes.extend_from_slice(&(name.len() as u16).to_be_bytes());
    bytes.extend_from_slice(name.as_bytes());
    bytes.extend_from_slice(&(value.len() as u16).to_be_bytes());
    bytes.extend_from_slice(value.as_bytes());
}

pub fn query(
    address: SocketAddr,
    authority: &str,
    path: &str,
    timeout: Duration,
) -> Result<Attributes> {
    let body = request(&format!("ipp://{authority}{path}"))?;
    decode(&crate::http::exchange(
        address,
        authority,
        path,
        timeout,
        "application/ipp",
        Some(&body),
    )?)
}

pub fn http_body(reader: &mut impl BufRead) -> Result<Vec<u8>> {
    crate::http::read_body(reader, "application/ipp")
}

pub fn decode(bytes: &[u8]) -> Result<Attributes> {
    if bytes.len() > MAX_BODY
        || bytes.len() < 9
        || !matches!((bytes[0], bytes[1]), (1, 0 | 1) | (2, 0..=2))
    {
        return Err(error("Invalid IPP response size or version"));
    }
    if u16::from_be_bytes([bytes[2], bytes[3]]) > 0xff {
        return Err(error("Printer rejected the IPP request"));
    }
    if bytes[4..8] != [0, 0, 0, 1] {
        return Err(error("IPP response request ID mismatch"));
    }
    let mut cursor = Cursor { bytes, pos: 8 };
    let mut attributes = Attributes::new();
    let mut group = 0;
    let mut name = String::new();
    let mut count = 0;
    while cursor.pos < bytes.len() {
        let tag = cursor.take(1)?[0];
        if tag == 3 {
            if cursor.pos != bytes.len() {
                return Err(error("Unexpected data after IPP attributes"));
            }
            return Ok(attributes);
        }
        if tag < 0x10 {
            group = tag;
            name.clear();
            continue;
        }
        count += 1;
        if count > 1024 {
            return Err(error("Too many IPP attributes"));
        }
        let n = cursor.u16()?;
        if n > 256 {
            return Err(error("IPP attribute name is too long"));
        }
        if n != 0 {
            name = std::str::from_utf8(cursor.take(n)?)?.into();
        }
        if name.is_empty() {
            return Err(error("IPP continuation has no attribute name"));
        }
        let n = cursor.u16()?;
        let value = cursor.take(n)?;
        if group != 4 {
            continue;
        }
        let value = match tag {
            0x21 | 0x23 => Value::from(i32::from_be_bytes(
                value.try_into().map_err(|_| error("Invalid IPP integer"))?,
            )),
            0x22 if value.len() == 1 && value[0] <= 1 => Value::from(value[0] != 0),
            0x22 => return Err(error("Invalid IPP boolean")),
            0x41 | 0x42 | 0x44..=0x49 => {
                if value.len() > 2048 {
                    return Err(error("IPP text value exceeds 2 KiB"));
                }
                Value::from(std::str::from_utf8(value)?.to_owned())
            }
            _ => continue,
        };
        if attributes.len() >= 256 && !attributes.contains_key(&name) {
            return Err(error("Too many IPP attributes"));
        }
        let values = attributes.entry(name.clone()).or_default();
        if values.len() >= 64 {
            return Err(error("Too many IPP attribute values"));
        }
        values.push(value);
    }
    Err(error("Truncated IPP response"))
}
struct Cursor<'a> {
    bytes: &'a [u8],
    pos: usize,
}
impl<'a> Cursor<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self
            .pos
            .checked_add(n)
            .ok_or_else(|| error("Invalid IPP length"))?;
        let value = self
            .bytes
            .get(self.pos..end)
            .ok_or_else(|| error("Truncated IPP attribute"))?;
        self.pos = end;
        Ok(value)
    }
    fn u16(&mut self) -> Result<usize> {
        let bytes = self.take(2)?;
        Ok(usize::from(u16::from_be_bytes([bytes[0], bytes[1]])))
    }
}
