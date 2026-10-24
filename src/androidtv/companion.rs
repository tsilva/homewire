//! Optional pinned HTTPS picture controls supplied by the installed TV companion.
use super::Pinned;
use crate::{Result, error, media, transport::Timed};
use rustls::{ClientConfig, ClientConnection, StreamOwned, pki_types::ServerName};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    io::{BufReader, Write},
    net::{IpAddr, SocketAddr},
    sync::Arc,
    time::{Duration, Instant},
};

fn port() -> u16 {
    8766
}
fn timeout() -> u64 {
    8
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default = "port")]
    pub port: u16,
    pub server_cert_sha256: String,
    pub token: String,
    pub fingerprint: String,
    pub incremental: String,
    #[serde(default = "timeout")]
    pub timeout_seconds: u64,
    #[serde(default)]
    pub connect: Option<SocketAddr>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustls::{
        ServerConfig, ServerConnection,
        pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject},
    };
    use sha2::{Digest, Sha256};
    use std::{
        io::{BufRead, Read},
        net::TcpListener,
        thread,
    };

    fn status(level: u8) -> Value {
        json!({"protocol_version":1,"manufacturer":"TCL","model":"Fixture TV","device":"fixture",
            "fingerprint":"verified-build","incremental":"revision","backlight_level":level})
    }
    fn fixture(
        replies: Vec<Option<Value>>,
    ) -> Result<(Config, thread::JoinHandle<Result<Vec<String>>>)> {
        let cert = CertificateDer::from_pem_slice(include_bytes!(
            "../../tests/fixtures/media/test-cert.pem"
        ))?;
        let key = PrivateKeyDer::from_pem_slice(include_bytes!(
            "../../tests/fixtures/media/test-key.pem"
        ))?;
        let pin = Sha256::digest(&cert)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let tls = Arc::new(
            ServerConfig::builder()
                .with_no_client_auth()
                .with_single_cert(vec![cert], key)?,
        );
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let address = listener.local_addr()?;
        let worker = thread::spawn(move || -> Result<Vec<String>> {
            let mut requests = Vec::new();
            for reply in replies {
                let (socket, _) = listener.accept()?;
                socket.set_read_timeout(Some(Duration::from_secs(2)))?;
                socket.set_write_timeout(Some(Duration::from_secs(2)))?;
                let mut stream = StreamOwned::new(ServerConnection::new(tls.clone())?, socket);
                let mut reader = BufReader::new(&mut stream);
                let mut first = String::new();
                if reader.read_line(&mut first).is_err() {
                    continue;
                }
                let mut length = 0;
                let mut auth = false;
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line)?;
                    if line == "\r\n" {
                        break;
                    }
                    if let Some(value) = line.strip_prefix("Content-Length: ") {
                        length = value.trim().parse::<usize>()?;
                    }
                    auth |= line.trim_end() == format!("Authorization: Bearer {}", "a".repeat(64));
                    if line.is_empty() {
                        return Err(error("Truncated fixture request"));
                    }
                }
                assert!(auth);
                assert!(length <= 256);
                let mut body = vec![0; length];
                reader.read_exact(&mut body)?;
                requests.push(first.trim().into());
                drop(reader);
                if let Some(mut value) = reply {
                    if first.starts_with("POST ") {
                        let request: Value = serde_json::from_slice(&body)?;
                        assert!(
                            request["request_id"]
                                .as_str()
                                .is_some_and(|id| id.len() == 32)
                        );
                        value["request_id"] = request["request_id"].clone();
                    }
                    let body = serde_json::to_vec(&value)?;
                    write!(
                        stream,
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    )?;
                    stream.write_all(&body)?;
                    stream.flush()?;
                }
            }
            Ok(requests)
        });
        Ok((
            Config {
                port: 8766,
                server_cert_sha256: pin,
                token: "a".repeat(64),
                fingerprint: "verified-build".into(),
                incremental: "revision".into(),
                timeout_seconds: 2,
                connect: Some(address),
            },
            worker,
        ))
    }
    fn finish(worker: thread::JoinHandle<Result<Vec<String>>>) -> Result<Vec<String>> {
        worker
            .join()
            .map_err(|_| error("Companion fixture panicked"))?
    }
    #[test]
    fn verified_status_and_control() -> Result<()> {
        let (cfg, worker) = fixture(vec![
            Some(status(50)),
            Some(json!({"verified":true,"applied":true,"status":status(0)})),
        ])?;
        cfg.validate()?;
        let result = cfg.set("192.0.2.40".parse()?, "Fixture TV", 0)?;
        assert_eq!(result["picture"]["backlight_level"], 0);
        assert_eq!(finish(worker)?.len(), 2);
        Ok(())
    }
    #[test]
    fn matching_level_skips_write() -> Result<()> {
        let (cfg, worker) = fixture(vec![Some(status(100))])?;
        assert_eq!(
            cfg.set("192.0.2.40".parse()?, "Fixture TV", 100)?["applied"],
            false
        );
        assert_eq!(finish(worker)?.len(), 1);
        Ok(())
    }
    #[test]
    fn lost_control_response_is_not_replayed() -> Result<()> {
        let (cfg, worker) = fixture(vec![Some(status(50)), None])?;
        assert!(cfg.set("192.0.2.40".parse()?, "Fixture TV", 10).is_err());
        let requests = finish(worker)?;
        assert_eq!(requests.iter().filter(|r| r.starts_with("POST")).count(), 1);
        Ok(())
    }
    #[test]
    fn identity_mismatch_blocks_write() -> Result<()> {
        let (cfg, worker) = fixture(vec![Some(status(50))])?;
        assert!(cfg.set("192.0.2.40".parse()?, "Other TV", 10).is_err());
        assert_eq!(finish(worker)?.len(), 1);
        Ok(())
    }
    #[test]
    fn certificate_mismatch_never_sends_token() -> Result<()> {
        let (mut cfg, worker) = fixture(vec![Some(status(50))])?;
        cfg.server_cert_sha256 = "00".repeat(32);
        assert!(cfg.status("192.0.2.40".parse()?, "Fixture TV").is_err());
        assert!(finish(worker)?.is_empty());
        Ok(())
    }
    #[test]
    fn malformed_config_and_out_of_range_are_rejected() -> Result<()> {
        let cfg = Config {
            port: 8766,
            server_cert_sha256: "00".repeat(32),
            token: "bad\r\ntoken".into(),
            fingerprint: "build".into(),
            incremental: "revision".into(),
            timeout_seconds: 2,
            connect: None,
        };
        assert!(cfg.validate().is_err());
        assert!(cfg.set("192.0.2.40".parse()?, "Fixture TV", 101).is_err());
        Ok(())
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Status {
    pub protocol_version: u8,
    pub manufacturer: String,
    pub model: String,
    pub device: String,
    pub fingerprint: String,
    pub incremental: String,
    pub backlight_level: u8,
}
impl Config {
    pub(super) fn validate(&self) -> Result<()> {
        media::valid_endpoint(self.port, self.connect, self.timeout_seconds)?;
        media::hex::<32>(&self.server_cert_sha256)?;
        media::hex::<32>(&self.token)?;
        for value in [&self.fingerprint, &self.incremental] {
            if value.is_empty() || value.len() > 256 || value.chars().any(char::is_control) {
                return Err(error("Invalid TV companion firmware identity"));
            }
        }
        Ok(())
    }
    fn check(&self, status: &Status, model: &str) -> Result<()> {
        if status.protocol_version != 1
            || status.manufacturer != "TCL"
            || status.model != model
            || status.fingerprint != self.fingerprint
            || status.incremental != self.incremental
            || status.device.is_empty()
            || status.device.len() > 128
            || status.backlight_level > 100
        {
            return Err(error(
                "TV companion identity, protocol or backlight range mismatch",
            ));
        }
        Ok(())
    }
    fn request_until(
        &self,
        host: IpAddr,
        path: &str,
        body: Option<&[u8]>,
        deadline: Instant,
    ) -> Result<Value> {
        self.request_guarded_until(host, path, body, deadline, None)
    }
    fn request_guarded_until(
        &self,
        host: IpAddr,
        path: &str,
        body: Option<&[u8]>,
        deadline: Instant,
        permit: Option<(&crate::brightness::Permit, u8)>,
    ) -> Result<Value> {
        let deadline = deadline.min(Instant::now() + Duration::from_secs(self.timeout_seconds));
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let tls = ClientConfig::builder_with_provider(provider.clone())
            .with_safe_default_protocol_versions()?
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(Pinned {
                fingerprint: media::hex::<32>(&self.server_cert_sha256)?,
                provider,
            }))
            .with_no_client_auth();
        let connection = ClientConnection::new(Arc::new(tls), ServerName::IpAddress(host.into()))?;
        let socket = Timed::connect(
            self.connect.unwrap_or(SocketAddr::new(host, self.port)),
            deadline,
        )?;
        let mut stream = StreamOwned::new(connection, socket);
        // Authenticate TLS before constructing or sending the credential-bearing request.
        while stream.conn.is_handshaking() {
            stream.conn.complete_io(&mut stream.sock)?;
        }
        if let Some((permit, level)) = permit {
            permit.check(level)?;
        }
        let method = if body.is_some() { "POST" } else { "GET" };
        write!(
            stream,
            "{method} {path} HTTP/1.1\r\nHost: {host}:{}\r\nAuthorization: Bearer {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            self.port,
            self.token,
            body.map_or(0, <[u8]>::len)
        )?;
        if let Some(body) = body {
            stream.write_all(body)?;
        }
        stream.flush()?;
        let response = crate::http::read_response(
            &mut BufReader::with_capacity(1024, stream),
            Some("application/json"),
            4096,
        )?
        .body;
        serde_json::from_slice(&response).map_err(|_| error("Invalid TV companion response"))
    }
    pub(super) fn status(&self, host: IpAddr, model: &str) -> Result<Status> {
        self.status_until(
            host,
            model,
            Instant::now() + Duration::from_secs(self.timeout_seconds),
        )
    }
    pub(super) fn status_until(
        &self,
        host: IpAddr,
        model: &str,
        deadline: Instant,
    ) -> Result<Status> {
        let status: Status =
            serde_json::from_value(self.request_until(host, "/v1/status", None, deadline)?)
                .map_err(|_| error("Invalid TV companion status"))?;
        self.check(&status, model)?;
        Ok(status)
    }
    pub(super) fn set(&self, host: IpAddr, model: &str, level: u8) -> Result<Value> {
        self.set_until(
            host,
            model,
            level,
            Instant::now() + Duration::from_secs(self.timeout_seconds),
        )
    }
    pub(super) fn set_until(
        &self,
        host: IpAddr,
        model: &str,
        level: u8,
        deadline: Instant,
    ) -> Result<Value> {
        self.set_with_permit(host, model, level, deadline, None)
    }
    pub(super) fn set_scheduled_until(
        &self,
        host: IpAddr,
        model: &str,
        level: u8,
        deadline: Instant,
        permit: &crate::brightness::Permit,
    ) -> Result<Value> {
        self.set_with_permit(host, model, level, deadline, Some(permit))
    }
    fn set_with_permit(
        &self,
        host: IpAddr,
        model: &str,
        level: u8,
        deadline: Instant,
        permit: Option<&crate::brightness::Permit>,
    ) -> Result<Value> {
        if level > 100 {
            return Err(error("Backlight level must be 0–100"));
        }
        let before = self.status_until(host, model, deadline)?;
        if let Some(permit) = permit {
            permit.check(before.backlight_level)?;
        }
        if before.backlight_level == level {
            return Ok(json!({"applied":false,"verified":true,"picture":before}));
        }
        let mut random = [0u8; 16];
        getrandom::getrandom(&mut random)?;
        let id: String = random.iter().map(|b| format!("{b:02x}")).collect();
        let body = serde_json::to_vec(&json!({"backlight_level":level,"request_id":id}))?;
        // Exactly one write request: transport or verification errors never replay it.
        let value = self.request_guarded_until(
            host,
            "/v1/backlight",
            Some(&body),
            deadline,
            permit.map(|p| (p, before.backlight_level)),
        )?;
        if value["request_id"] != id || value["verified"] != true {
            return Err(error("TV companion control is unconfirmed; no replay"));
        }
        let status: Status = serde_json::from_value(value["status"].clone())
            .map_err(|_| error("Invalid TV companion control status; no replay"))?;
        self.check(&status, model)?;
        if status.backlight_level != level {
            return Err(error("TV backlight verification failed; no replay"));
        }
        Ok(json!({"applied":value["applied"],"verified":true,"picture":status}))
    }
}
