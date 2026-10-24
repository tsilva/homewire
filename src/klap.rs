//! Tapo KLAP v2 over LAN HTTP: authenticated handshake and AES-CBC messages.
use crate::{Result, error, http};
use aes::Aes128;
use cipher::{BlockDecrypt, BlockEncrypt, KeyInit, generic_array::GenericArray};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    net::SocketAddr,
    time::{Duration, Instant},
};

const MAX_PAYLOAD: usize = 16 * 1024;
pub fn hash(parts: &[&[u8]]) -> [u8; 32] {
    let mut hash = Sha256::new();
    for part in parts {
        hash.update(part);
    }
    hash.finalize().into()
}
fn equal(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0_u8, |diff, (a, b)| diff | (a ^ b)) == 0
}
pub fn auth_hash(value: &str) -> Result<[u8; 32]> {
    if value.len() != 64 || !value.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(error(
            "Tapo auth_hash must be a private 64-digit hexadecimal KLAP v2 hash",
        ));
    }
    let mut key = [0; 32];
    for (i, byte) in key.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&value[i * 2..i * 2 + 2], 16)?;
    }
    Ok(key)
}
pub struct Session {
    key: [u8; 16],
    iv: [u8; 12],
    signature: [u8; 28],
    sequence: i32,
}
impl Session {
    pub fn new(local: &[u8; 16], remote: &[u8; 16], auth: &[u8; 32]) -> Result<Self> {
        let key = hash(&[b"lsk", local, remote, auth]);
        let iv = hash(&[b"iv", local, remote, auth]);
        let signature = hash(&[b"ldk", local, remote, auth]);
        Ok(Self {
            key: key[..16].try_into()?,
            iv: iv[..12].try_into()?,
            signature: signature[..28].try_into()?,
            sequence: i32::from_be_bytes(iv[28..].try_into()?),
        })
    }
    fn iv(&self) -> [u8; 16] {
        let mut iv = [0; 16];
        iv[..12].copy_from_slice(&self.iv);
        iv[12..].copy_from_slice(&self.sequence.to_be_bytes());
        iv
    }
    pub fn encrypt(&mut self, bytes: &[u8]) -> Result<(Vec<u8>, i32)> {
        if bytes.len() > MAX_PAYLOAD {
            return Err(error("KLAP request exceeds 16 KiB"));
        }
        self.sequence = self
            .sequence
            .checked_add(1)
            .ok_or_else(|| error("KLAP sequence exhausted"))?;
        let padding = 16 - bytes.len() % 16;
        // Signature and padded ciphertext share one exactly sized allocation.
        let mut result = Vec::with_capacity(32 + bytes.len() + padding);
        result.resize(32, 0);
        result.extend_from_slice(bytes);
        result.resize(32 + bytes.len() + padding, padding as u8);
        let cipher = Aes128::new(GenericArray::from_slice(&self.key));
        let mut previous = self.iv();
        for block in result[32..].as_chunks_mut::<16>().0 {
            for (byte, prev) in block.iter_mut().zip(previous) {
                *byte ^= prev;
            }
            cipher.encrypt_block(GenericArray::from_mut_slice(block));
            previous.copy_from_slice(block);
        }
        let signature = hash(&[&self.signature, &self.sequence.to_be_bytes(), &result[32..]]);
        result[..32].copy_from_slice(&signature);
        Ok((result, self.sequence))
    }
    pub fn decrypt(&self, bytes: &[u8]) -> Result<Vec<u8>> {
        Self::validate_size(bytes.len())?;
        let mut out = bytes.to_vec();
        let length = self.decrypt_in_place(&mut out)?.len();
        out.copy_within(32..32 + length, 0);
        out.truncate(length);
        Ok(out)
    }
    /// Authenticate before changing the frame, then reuse its ciphertext storage.
    /// On success the returned slice contains only unpadded plaintext. A frame
    /// must not be reused after this call, including on a padding error.
    pub fn decrypt_in_place<'a>(&self, bytes: &'a mut [u8]) -> Result<&'a mut [u8]> {
        Self::validate_size(bytes.len())?;
        if !equal(
            &bytes[..32],
            &hash(&[&self.signature, &self.sequence.to_be_bytes(), &bytes[32..]]),
        ) {
            return Err(error("KLAP response signature mismatch"));
        }
        let body = &mut bytes[32..];
        let cipher = Aes128::new(GenericArray::from_slice(&self.key));
        let mut previous = self.iv();
        for block in body.as_chunks_mut::<16>().0 {
            let mut next = [0; 16];
            next.copy_from_slice(block);
            cipher.decrypt_block(GenericArray::from_mut_slice(block));
            for (byte, prev) in block.iter_mut().zip(previous) {
                *byte ^= prev;
            }
            previous = next;
        }
        let padding = usize::from(*body.last().ok_or_else(|| error("Empty KLAP response"))?);
        if !(1..=16).contains(&padding)
            || !body[body.len() - padding..]
                .iter()
                .all(|b| usize::from(*b) == padding)
        {
            return Err(error("Invalid KLAP padding"));
        }
        let length = body.len() - padding;
        Ok(&mut body[..length])
    }
    fn validate_size(length: usize) -> Result<()> {
        if !(48..=MAX_PAYLOAD + 48).contains(&length) || !(length - 32).is_multiple_of(16) {
            return Err(error("Invalid KLAP response size"));
        }
        Ok(())
    }
}
#[derive(Debug)]
struct RejectedHandshake {
    stage: &'static str,
    code: u16,
}
impl std::fmt::Display for RejectedHandshake {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "KLAP {} failed: Device returned HTTP {}",
            self.stage, self.code
        )
    }
}
impl std::error::Error for RejectedHandshake {}
fn handshake_error(
    stage: &'static str,
    e: Box<dyn std::error::Error + Send + Sync>,
) -> Box<dyn std::error::Error + Send + Sync> {
    if let Some(status) = e.downcast_ref::<http::StatusError>() {
        Box::new(RejectedHandshake {
            stage,
            code: status.0,
        })
    } else {
        error(format!("KLAP {stage} failed: {e}"))
    }
}
#[derive(Debug)]
pub(crate) struct RejectedRequest {
    pub method: String,
    pub code: i64,
}
impl std::fmt::Display for RejectedRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Tapo rejected {} request (code {}); no automatic retry",
            self.method, self.code
        )
    }
}
impl std::error::Error for RejectedRequest {}
pub struct Client {
    address: SocketAddr,
    authority: String,
    deadline: Instant,
    cookie: String,
    session: Session,
    auth: [u8; 32],
}
impl Client {
    pub fn connect(
        address: SocketAddr,
        authority: String,
        auth: &[u8; 32],
        timeout: Duration,
    ) -> Result<Self> {
        Self::connect_until(address, authority, auth, Instant::now() + timeout)
    }
    pub(crate) fn connect_until(
        address: SocketAddr,
        authority: String,
        auth: &[u8; 32],
        deadline: Instant,
    ) -> Result<Self> {
        // Authentication traffic only; no application/device command has been sent.
        match Self::handshake_until(address, authority.clone(), auth, deadline) {
            Err(e)
                if e.downcast_ref::<RejectedHandshake>()
                    .is_some_and(|e| matches!(e.code, 400 | 403)) =>
            {
                Self::handshake_until(address, authority, auth, deadline)
            }
            result => result,
        }
    }
    fn handshake_until(
        address: SocketAddr,
        authority: String,
        auth: &[u8; 32],
        deadline: Instant,
    ) -> Result<Self> {
        let mut local = [0; 16];
        getrandom::getrandom(&mut local).map_err(|_| error("Cannot obtain KLAP random seed"))?;
        // Bulbs label binary responses text/html; framing and KLAP validate them.
        let response = http::send(
            address,
            &authority,
            deadline,
            http::Request {
                path: "/app/handshake1",
                content_type: None,
                body: Some(&local),
                cookie: None,
                max_response_bytes: 48,
            },
        )
        .map_err(|e| handshake_error("handshake1", e))?;
        if response.body.len() != 48 {
            return Err(error("Invalid KLAP handshake response"));
        }
        let remote: [u8; 16] = response.body[..16].try_into()?;
        if !equal(&response.body[16..], &hash(&[&local, &remote, auth])) {
            return Err(error(
                "KLAP authentication failed; check the private auth hash",
            ));
        }
        let cookie = response
            .session_cookie
            .ok_or_else(|| error("Missing KLAP session cookie"))?;
        let proof = hash(&[&remote, &local, auth]);
        let response = http::send(
            address,
            &authority,
            deadline,
            http::Request {
                path: "/app/handshake2",
                content_type: None,
                body: Some(&proof),
                cookie: Some(&cookie),
                max_response_bytes: 0,
            },
        )
        .map_err(|e| handshake_error("handshake2", e))?;
        if !response.body.is_empty() {
            return Err(error("Invalid KLAP handshake acknowledgement"));
        }
        Ok(Self {
            address,
            authority,
            deadline,
            cookie,
            session: Session::new(&local, &remote, auth)?,
            auth: *auth,
        })
    }
    /// A single read-only refresh on security rejection, within the original deadline.
    /// Never refresh/replay a set_device_info command.
    pub fn read_info(&mut self) -> Result<Value> {
        self.read("get_device_info")
    }
    /// Only allow these read-only methods to refresh/replay a security-rejected read.
    pub fn read(&mut self, method: &str) -> Result<Value> {
        if !matches!(
            method,
            "get_device_info" | "get_energy_usage" | "get_emeter_data"
        ) {
            return Err(error("Unsupported KLAP read-only method"));
        }
        match self.query(method, None) {
            Err(e)
                if e.downcast_ref::<http::StatusError>()
                    .is_some_and(|e| e.0 == 403) =>
            {
                *self = Self::connect_until(
                    self.address,
                    self.authority.clone(),
                    &self.auth,
                    self.deadline,
                )?;
                self.query(method, None)
            }
            result => result,
        }
    }
    pub fn query(&mut self, method: &str, params: Option<Value>) -> Result<Value> {
        #[derive(serde::Serialize)]
        struct Request<'a> {
            method: &'a str,
            #[serde(skip_serializing_if = "Option::is_none")]
            params: Option<&'a Value>,
        }
        let request = Request {
            method,
            params: params.as_ref(),
        };
        let (body, sequence) = self.session.encrypt(&serde_json::to_vec(&request)?)?;
        let path = format!("/app/request?seq={sequence}");
        let mut response = http::send(
            self.address,
            &self.authority,
            self.deadline,
            http::Request {
                path: &path,
                content_type: None,
                body: Some(&body),
                cookie: Some(&self.cookie),
                max_response_bytes: MAX_PAYLOAD + 48,
            },
        )
        .map_err(|e| {
            if e.is::<http::StatusError>() {
                e
            } else {
                error(format!("KLAP request failed: {e}; no automatic retry"))
            }
        })?;
        let decoded = self.session.decrypt_in_place(&mut response.body)?;
        #[derive(serde::Deserialize)]
        struct Reply {
            error_code: Option<i64>,
            result: Option<Value>,
        }
        let value: Reply =
            serde_json::from_slice(decoded).map_err(|_| error("Invalid KLAP response JSON"))?;
        match value.error_code {
            Some(0) => Ok(value.result.unwrap_or(Value::Null)),
            Some(code) => Err(Box::new(RejectedRequest {
                method: method.to_owned(),
                code,
            })),
            None => Err(error("Missing KLAP response error code")),
        }
    }
}
