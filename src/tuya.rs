//! The Meaco's Tuya 3.3 LAN protocol: bounded frames, AES-128-ECB and CRC32.
//! A CRC is a corruption check, not authentication; use a trusted LAN.
use crate::{Result, config::Config, error};
use aes::Aes128;
use cipher::{BlockDecrypt, BlockEncrypt, KeyInit, generic_array::GenericArray};
use serde_json::{Map, Value, json};
use std::{
    io::{Read, Write},
    net::{SocketAddr, TcpStream},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const PREFIX: u32 = 0x55aa;
const SUFFIX: u32 = 0xaa55;
const MAX_FRAME: usize = 16 * 1024;
const VERSION: &[u8; 15] = b"3.3\0\0\0\0\0\0\0\0\0\0\0\0";

pub struct Client {
    address: SocketAddr,
    device_id: String,
    key: [u8; 16],
    timeout: Duration,
    sequence: u32,
}
impl Client {
    pub fn new(config: &Config, address: Option<SocketAddr>) -> Result<Self> {
        Ok(Self {
            address: address.unwrap_or(config.address()),
            device_id: config.device_id.clone(),
            key: config.key()?,
            timeout: Duration::from_secs(config.timeout_seconds),
            sequence: 1,
        })
    }

    pub fn status(&mut self) -> Result<Map<String, Value>> {
        let t = timestamp()?.to_string();
        let payload =
            json!({"gwId":self.device_id,"devId":self.device_id,"uid":self.device_id,"t":t});
        let result = self.exchange(10, &payload, true)?;
        let dps = result
            .get("dps")
            .or_else(|| result.get("data").and_then(|x| x.get("dps")));
        dps.and_then(Value::as_object)
            .cloned()
            .ok_or_else(|| error("Device response contains no data points"))
    }

    pub fn set(&mut self, dps: Map<String, Value>) -> Result<()> {
        let payload = json!({"devId":self.device_id,"uid":self.device_id,"t":timestamp()?.to_string(),"dps":dps});
        self.exchange(7, &payload, false)?;
        Ok(())
    }

    fn exchange(&mut self, command: u32, payload: &Value, need_data: bool) -> Result<Value> {
        // A fresh, short-lived connection avoids a persistent per-device thread.
        let mut stream = TcpStream::connect_timeout(&self.address, self.timeout)
            .map_err(|e| error(format!("Cannot connect to device: {e}")))?;
        stream.set_write_timeout(Some(self.timeout))?;
        stream.set_nodelay(true)?;
        let deadline = Instant::now() + self.timeout;
        let sequence = self.sequence;
        self.sequence = self.sequence.wrapping_add(1);
        let frame = encode_request(sequence, command, &serde_json::to_vec(payload)?, &self.key)?;
        stream.write_all(&frame)?;
        let mut merged = Map::new();
        for _ in 0..16 {
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .ok_or_else(|| {
                    error("Device response timed out; command outcome may be unknown")
                })?;
            stream.set_read_timeout(Some(remaining))?;
            let mut header = [0u8; 16];
            stream.read_exact(&mut header)?;
            if number(&header[..4])? != PREFIX {
                return Err(error("Invalid Tuya frame prefix"));
            }
            let length = number(&header[12..16])? as usize;
            if !(12..=MAX_FRAME).contains(&length) {
                return Err(error("Invalid Tuya frame length"));
            }
            let mut bytes = header.to_vec();
            bytes.resize(16 + length, 0);
            stream.read_exact(&mut bytes[16..])?;
            let response = decode_response(&bytes, &self.key)?;
            // STATUS updates may precede the query response. Merge only within
            // this bounded query, never across old connections or failed reads.
            if need_data {
                if let Some(dps) = response
                    .get("dps")
                    .or_else(|| response.get("data").and_then(|x| x.get("dps")))
                    .and_then(Value::as_object)
                {
                    merged.extend(dps.clone());
                    if ["1", "2", "4", "19"]
                        .iter()
                        .all(|k| merged.contains_key(*k))
                    {
                        return Ok(json!({"dps":merged}));
                    }
                }
            } else if number(&header[4..8])? == sequence || number(&header[8..12])? == command {
                return Ok(response);
            }
        }
        Err(error("No complete response within 16 device frames"))
    }
}

fn timestamp() -> Result<u64> {
    Ok(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs())
}
fn number(bytes: &[u8]) -> Result<u32> {
    Ok(u32::from_be_bytes(
        bytes
            .try_into()
            .map_err(|_| error("Invalid frame integer"))?,
    ))
}
pub fn encrypt(bytes: &[u8], key: &[u8; 16]) -> Vec<u8> {
    let padding = 16 - bytes.len() % 16;
    let mut out = bytes.to_vec();
    out.resize(bytes.len() + padding, padding as u8);
    let cipher = Aes128::new(GenericArray::from_slice(key));
    for block in out.chunks_exact_mut(16) {
        cipher.encrypt_block(GenericArray::from_mut_slice(block));
    }
    out
}
pub fn decrypt(bytes: &[u8], key: &[u8; 16]) -> Result<Vec<u8>> {
    if bytes.is_empty() || !bytes.len().is_multiple_of(16) {
        return Err(error("Invalid encrypted payload length"));
    }
    let mut out = bytes.to_vec();
    let cipher = Aes128::new(GenericArray::from_slice(key));
    for block in out.chunks_exact_mut(16) {
        cipher.decrypt_block(GenericArray::from_mut_slice(block));
    }
    let padding = usize::from(*out.last().ok_or_else(|| error("Empty encrypted payload"))?);
    if !(1..=16).contains(&padding)
        || !out[out.len() - padding..]
            .iter()
            .all(|b| usize::from(*b) == padding)
    {
        return Err(error(
            "Invalid encrypted payload padding; verify the local key",
        ));
    }
    out.truncate(out.len() - padding);
    Ok(out)
}
pub fn encode_request(
    sequence: u32,
    command: u32,
    payload: &[u8],
    key: &[u8; 16],
) -> Result<Vec<u8>> {
    let encrypted = encrypt(payload, key);
    let has_version = !matches!(command, 9 | 10 | 16 | 18);
    let size = encrypted.len() + if has_version { 15 } else { 0 };
    if size + 8 > MAX_FRAME {
        return Err(error("Tuya request exceeds frame limit"));
    }
    let mut frame = Vec::with_capacity(size + 24);
    for n in [PREFIX, sequence, command, (size + 8) as u32] {
        frame.extend_from_slice(&n.to_be_bytes());
    }
    if has_version {
        frame.extend_from_slice(VERSION);
    }
    frame.extend_from_slice(&encrypted);
    frame.extend_from_slice(&crc32fast::hash(&frame).to_be_bytes());
    frame.extend_from_slice(&SUFFIX.to_be_bytes());
    Ok(frame)
}
pub fn decode_response(frame: &[u8], key: &[u8; 16]) -> Result<Value> {
    if frame.len() < 28
        || number(&frame[..4])? != PREFIX
        || number(&frame[12..16])? as usize != frame.len() - 16
    {
        return Err(error("Invalid Tuya response frame"));
    }
    let end = frame.len();
    if number(&frame[end - 4..])? != SUFFIX
        || number(&frame[end - 8..end - 4])? != crc32fast::hash(&frame[..end - 8])
    {
        return Err(error("Tuya response checksum or suffix mismatch"));
    }
    if number(&frame[16..20])? != 0 {
        return Err(error("Device rejected the command"));
    }
    let mut payload = &frame[20..end - 8];
    if payload.is_empty() {
        return Ok(Value::Null);
    }
    if payload.starts_with(b"3.3") {
        if payload.len() < 15 {
            return Err(error("Truncated Tuya version header"));
        }
        payload = &payload[15..];
    }
    let plaintext = decrypt(payload, key)?;
    serde_json::from_slice(&plaintext).map_err(|_| error("Device returned invalid JSON"))
}
