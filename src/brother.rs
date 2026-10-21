//! Read-only SNMP v1 Brother counters. No SNMP SET or printer jobs.
use crate::{
    Result,
    device::{Adapter, Tool},
    entity::{Capabilities, Domain, Entity, Support},
    error,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    net::{IpAddr, SocketAddr, UdpSocket},
    path::Path,
    time::Duration,
};
const MODEL: &[u32] = &[1, 3, 6, 1, 4, 1, 2435, 2, 3, 9, 1, 1, 7, 0];
const COUNTERS: &[u32] = &[1, 3, 6, 1, 4, 1, 2435, 2, 3, 9, 4, 2, 1, 5, 5, 10, 0];
const MAINTENANCE: &[u32] = &[1, 3, 6, 1, 4, 1, 2435, 2, 3, 9, 4, 2, 1, 5, 5, 8, 0];
const NEXTCARE: &[u32] = &[1, 3, 6, 1, 4, 1, 2435, 2, 3, 9, 4, 2, 1, 5, 5, 11, 0];
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub name: String,
    pub host: IpAddr,
    #[serde(default = "community")]
    pub community: String,
    #[serde(default = "timeout")]
    pub timeout_seconds: u64,
    #[serde(default)]
    pub connect: Option<SocketAddr>,
}
fn community() -> String {
    "public".into()
}
fn timeout() -> u64 {
    3
}
pub struct Manager {
    config: Config,
    _lock: crate::config::FileLock,
}
fn tlv(tag: u8, body: &[u8]) -> Vec<u8> {
    let mut v = vec![tag];
    if body.len() < 128 {
        v.push(body.len() as u8)
    } else {
        v.extend([0x82, (body.len() >> 8) as u8, body.len() as u8]);
    }
    v.extend(body);
    v
}
fn integer(n: u32) -> Vec<u8> {
    let bytes = n.to_be_bytes();
    let i = bytes.iter().position(|b| *b != 0).unwrap_or(3);
    let mut v = bytes[i..].to_vec();
    if v[0] & 128 != 0 {
        v.insert(0, 0);
    }
    tlv(2, &v)
}
fn oid(value: &[u32]) -> Vec<u8> {
    let mut b = vec![(value[0] * 40 + value[1]) as u8];
    for n in &value[2..] {
        let mut digits = [0; 5];
        let mut i = 4;
        let mut n = *n;
        digits[i] = (n & 127) as u8;
        n >>= 7;
        while n > 0 {
            i -= 1;
            digits[i] = ((n & 127) | 128) as u8;
            n >>= 7;
        }
        b.extend(&digits[i..]);
    }
    tlv(6, &b)
}
fn take<'a>(bytes: &mut &'a [u8]) -> Result<(u8, &'a [u8])> {
    if bytes.len() < 2 {
        return Err(error("Truncated SNMP value"));
    }
    let tag = bytes[0];
    let mut offset = 2;
    let len = if bytes[1] < 128 {
        bytes[1] as usize
    } else {
        let count = (bytes[1] & 127) as usize;
        if count == 0 || count > 2 || bytes.len() < 2 + count {
            return Err(error("Invalid SNMP length"));
        }
        offset += count;
        bytes[2..offset]
            .iter()
            .fold(0, |n, b| (n << 8) | *b as usize)
    };
    if offset + len > bytes.len() {
        return Err(error("Truncated SNMP body"));
    }
    let value = &bytes[offset..offset + len];
    *bytes = &bytes[offset + len..];
    Ok((tag, value))
}
fn tagged<'a>(bytes: &mut &'a [u8], tag: u8) -> Result<&'a [u8]> {
    let (t, v) = take(bytes)?;
    if t != tag {
        return Err(error("Unexpected SNMP type"));
    }
    Ok(v)
}
fn number(bytes: &mut &[u8]) -> Result<u32> {
    let b = tagged(bytes, 2)?;
    if b.is_empty() || b.len() > 5 || b[0] & 128 != 0 {
        return Err(error("Invalid SNMP integer"));
    }
    let mut n = 0u32;
    for x in b {
        n = n
            .checked_mul(256)
            .and_then(|n| n.checked_add(*x as u32))
            .ok_or_else(|| error("SNMP integer overflow"))?;
    }
    Ok(n)
}
fn decode(bytes: &[u8], id: u32, community: &str, oids: &[&[u32]]) -> Result<Vec<Vec<u8>>> {
    let mut outer = bytes;
    let mut msg = tagged(&mut outer, 0x30)?;
    if !outer.is_empty() || number(&mut msg)? != 0 || tagged(&mut msg, 4)? != community.as_bytes() {
        return Err(error("SNMP response identity mismatch"));
    }
    let mut pdu = tagged(&mut msg, 0xa2)?;
    if !msg.is_empty()
        || number(&mut pdu)? != id
        || number(&mut pdu)? != 0
        || number(&mut pdu)? != 0
    {
        return Err(error("SNMP GET failed"));
    }
    let mut bindings = tagged(&mut pdu, 0x30)?;
    if !pdu.is_empty() {
        return Err(error("Extra SNMP PDU data"));
    }
    let mut values = Vec::new();
    for expected in oids {
        let mut binding = tagged(&mut bindings, 0x30)?;
        let expected = oid(expected);
        let mut encoded = &expected[..];
        let expected = tagged(&mut encoded, 6)?;
        if tagged(&mut binding, 6)? != expected {
            return Err(error("SNMP response OID mismatch"));
        }
        let (tag, value) = take(&mut binding)?;
        if tag != 4 || !binding.is_empty() {
            return Err(error("Invalid Brother diagnostic type"));
        }
        values.push(value.to_vec());
    }
    if !bindings.is_empty() {
        return Err(error("Extra SNMP bindings"));
    }
    Ok(values)
}
fn fields(bytes: &[u8]) -> Result<Vec<(u8, u32)>> {
    let bytes = if bytes.len() % 7 == 1 && bytes.last() == Some(&255) {
        &bytes[..bytes.len() - 1]
    } else {
        bytes
    };
    if bytes.len() % 7 != 0 || bytes.len() > 1024 {
        return Err(error("Unsupported Brother diagnostic encoding"));
    }
    let mut result = Vec::new();
    for b in bytes.chunks_exact(7) {
        if b[1] != 1 || b[2] != 4 {
            return Err(error("Unsupported Brother counter width"));
        }
        result.push((b[0], u32::from_be_bytes([b[3], b[4], b[5], b[6]])));
    }
    Ok(result)
}
impl Manager {
    pub fn open(path: &Path) -> Result<Self> {
        Self::from_source(&crate::config::Source::file(path))
    }
    pub fn from_source(source: &crate::config::Source<'_>) -> Result<Self> {
        let path = source.path();
        let config: Config = source.decode()?;
        if !crate::config::valid_unique_labels(std::iter::once(config.name.as_str()), 32, b"-_")
            || config.community.is_empty()
            || config.community.len() > 64
            || !(1..=10).contains(&config.timeout_seconds)
        {
            return Err(error("Invalid SNMP configuration"));
        }
        Ok(Self {
            config,
            _lock: crate::config::lock_configuration(path)?,
        })
    }
    fn query(&self, oids: &[&[u32]]) -> Result<Vec<Vec<u8>>> {
        let mut seed = [0; 4];
        getrandom::getrandom(&mut seed).map_err(|_| error("SNMP request entropy unavailable"))?;
        let id = u32::from_be_bytes(seed) & 0x7fffffff;
        let mut bindings = Vec::new();
        for value in oids {
            let mut b = oid(value);
            b.extend(tlv(5, &[]));
            bindings.extend(tlv(0x30, &b));
        }
        let mut pdu = integer(id);
        pdu.extend(integer(0));
        pdu.extend(integer(0));
        pdu.extend(tlv(0x30, &bindings));
        let mut message = integer(0);
        message.extend(tlv(4, self.config.community.as_bytes()));
        message.extend(tlv(0xa0, &pdu));
        let dest = self
            .config
            .connect
            .unwrap_or(SocketAddr::new(self.config.host, 161));
        let socket = UdpSocket::bind(if dest.is_ipv4() {
            "0.0.0.0:0"
        } else {
            "[::]:0"
        })?;
        socket.set_read_timeout(Some(Duration::from_secs(self.config.timeout_seconds)))?;
        socket.connect(dest)?;
        socket.send(&tlv(0x30, &message))?;
        let mut response = [0; 8193];
        let len = socket.recv(&mut response)?;
        if len > 8192 {
            return Err(error("SNMP response exceeds limit"));
        }
        decode(&response[..len], id, &self.config.community, oids)
    }
    fn status(&self) -> Result<Value> {
        let model = self.query(&[MODEL])?;
        if !String::from_utf8_lossy(&model[0]).contains("MDL:HL-L2350DW") {
            return Err(error("Brother SNMP identity mismatch"));
        }
        let mut result = json!({"model":"HL-L2350DW"});
        for (oid, map) in [
            (
                COUNTERS,
                &[(0x00, "page_count"), (0x06, "duplex_count")][..],
            ),
            (
                MAINTENANCE,
                &[
                    (0x11, "drum_count"),
                    (0x41, "drum_life_percent"),
                    (0x6f, "toner_remaining_percent"),
                ][..],
            ),
            (NEXTCARE, &[(0x82, "drum_remaining_pages")][..]),
        ] {
            match self.query(&[oid]).and_then(|v| fields(&v[0])) {
                Ok(values) => {
                    for (code, key) in map {
                        result[*key] =
                            values
                                .iter()
                                .find(|(c, _)| c == code)
                                .map_or(Value::Null, |(_, n)| {
                                    if key.ends_with("_percent") {
                                        if *n <= 10000 {
                                            json!((*n as f64 / 100.0).round())
                                        } else {
                                            Value::Null
                                        }
                                    } else {
                                        json!(n)
                                    }
                                });
                    }
                }
                Err(_) => {
                    for (_, key) in map {
                        result[*key] = Value::Null;
                    }
                }
            }
        }
        Ok(result)
    }
}
impl Adapter for Manager {
    const TOOLS: &'static [Tool] = &[];
    const SUPPORT: Support = Support::READ;
    fn from_source(source: &crate::config::Source<'_>) -> Result<Self> {
        Self::from_source(source)
    }
    fn execute(&mut self, _: &str, _: &Value) -> Result<Value> {
        Err(error("Use diagnostic entities"))
    }
    fn entities<'a>(&'a self, v: &mut impl FnMut(Entity<'a>)) {
        let base = Entity::new(
            crate::device::Family::Brother,
            0,
            Domain::Printer,
            &self.config.name,
            "HL-L2350DW SNMP",
            Capabilities::Read,
        );
        v(base);
        for (s, f, u) in [
            ("_pages", "page_count", "pages"),
            ("_duplex", "duplex_count", "pages"),
            ("_drum_count", "drum_count", "pages"),
            ("_drum_remaining", "drum_remaining_pages", "pages"),
            ("_toner_remaining", "toner_remaining_percent", "%"),
            ("_drum_life", "drum_life_percent", "%"),
        ] {
            v(base.sensor(s, f, u));
        }
    }
    fn read_device(&mut self, i: usize) -> Result<Value> {
        if i != 0 {
            return Err(error("Unknown Brother diagnostics"));
        }
        self.status()
    }
}
