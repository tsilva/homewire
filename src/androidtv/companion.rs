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
    #[serde(default)]
    pub readback: bool,
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
    enum Reply {
        Json(Value),
        Bytes(&'static str, Vec<u8>),
        Lost,
    }
    fn fixture(
        replies: Vec<Option<Value>>,
    ) -> Result<(Config, thread::JoinHandle<Result<Vec<String>>>)> {
        fixture_responses(
            replies
                .into_iter()
                .map(|value| value.map_or(Reply::Lost, Reply::Json))
                .collect(),
        )
    }
    fn fixture_responses(
        replies: Vec<Reply>,
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
                let (content_type, body) = match reply {
                    Reply::Lost => continue,
                    Reply::Bytes(content_type, body) => (content_type, body),
                    Reply::Json(mut value) => {
                        if first.starts_with("POST ") {
                            let request: Value = serde_json::from_slice(&body)?;
                            assert!(
                                request["request_id"]
                                    .as_str()
                                    .is_some_and(|id| id.len() == 32)
                            );
                            value["request_id"] = request["request_id"].clone();
                        }
                        ("application/json", serde_json::to_vec(&value)?)
                    }
                };
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )?;
                stream.write_all(&body)?;
                stream.flush()?;
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
                readback: false,
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
    fn screenshot_requires_live_identity_and_accepts_only_bounded_jpeg() -> Result<()> {
        let mut jpeg = crate::screen::tests::jpeg(1280, 720);
        jpeg.truncate(jpeg.len() - 2);
        jpeg.resize(crate::screen::MAX_JPEG - 2, 0);
        jpeg.extend_from_slice(&[0xff, 0xd9]);
        let (cfg, worker) = fixture_responses(vec![
            Reply::Json(status(50)),
            Reply::Bytes("image/jpeg", jpeg),
        ])?;
        let value = cfg.screen("192.0.2.40".parse()?, "Fixture TV", true)?;
        assert_eq!(value["width"], 1280);
        assert_eq!(value["height"], 720);
        assert_eq!(
            finish(worker)?,
            [
                "GET /v1/status HTTP/1.1",
                "GET /v1/prototype/screenshot HTTP/1.1"
            ]
        );
        let (cfg, worker) = fixture(vec![Some(status(50))])?;
        assert!(cfg.screen("192.0.2.40".parse()?, "Other TV", true).is_err());
        assert_eq!(finish(worker)?.len(), 1);
        for reply in [
            Reply::Bytes("image/jpeg", vec![0; crate::screen::MAX_JPEG + 1]),
            Reply::Bytes("text/plain", crate::screen::tests::jpeg(1, 1)),
            Reply::Bytes("image/jpeg", b"not a JPEG".to_vec()),
        ] {
            let (cfg, worker) = fixture_responses(vec![Reply::Json(status(50)), reply])?;
            assert!(
                cfg.screen("192.0.2.40".parse()?, "Fixture TV", true)
                    .is_err()
            );
            let _ = finish(worker);
        }
        Ok(())
    }
    #[test]
    fn navigation_is_sent_once_and_requires_an_accepted_response() -> Result<()> {
        for reply in [
            Some(json!({"accepted":true,"verified":false})),
            Some(json!({"accepted":false,"verified":false})),
            None,
        ] {
            let expected = reply.as_ref().is_some_and(|v| v["accepted"] == true);
            let (cfg, worker) = fixture(vec![Some(status(50)), reply])?;
            assert_eq!(
                cfg.navigate("192.0.2.40".parse()?, "Fixture TV", "right")
                    .is_ok(),
                expected
            );
            let requests = finish(worker)?;
            assert_eq!(
                requests
                    .iter()
                    .filter(|line| line.starts_with("POST "))
                    .count(),
                1
            );
        }
        let (cfg, worker) = fixture(vec![Some(status(50))])?;
        assert!(
            cfg.navigate("192.0.2.40".parse()?, "Other TV", "right")
                .is_err()
        );
        assert_eq!(finish(worker)?.len(), 1);
        assert!(
            cfg.navigate("192.0.2.40".parse()?, "Fixture TV", "power")
                .is_err()
        );
        Ok(())
    }
    #[test]
    fn screen_status_accepts_disabled_and_rejects_malformed_focus() -> Result<()> {
        for (response, expected) in [
            (json!({"enabled":false}), true),
            (
                json!({"enabled":true,"navigation_method":"accessibility_focus_actions","focus":{"available":false}}),
                true,
            ),
            (
                json!({"enabled":true,"navigation_method":"accessibility_focus_actions","focus":{"available":true}}),
                false,
            ),
            (json!({"enabled":false,"unexpected":"field"}), false),
        ] {
            let (cfg, worker) = fixture(vec![Some(status(50)), Some(response)])?;
            assert_eq!(
                cfg.screen("192.0.2.40".parse()?, "Fixture TV", false)
                    .is_ok(),
                expected
            );
            assert_eq!(finish(worker)?.len(), 2);
        }
        Ok(())
    }
    fn readable() -> Value {
        json!({"foreground_package":"example.player","playback":{"access_enabled":true,"truncated":false,"sessions":[{"package":"example.player","state":3,"position_ms":12000,"speed":1.0,"updated_age_ms":50,"title":null,"display_title":null,"media_id":null,"duration_ms":null}]}})
    }
    #[test]
    fn readback_keeps_unknown_titles_and_rejects_invalid_sessions() -> Result<()> {
        let value = readable();
        assert!(serde_json::from_value::<Readback>(value.clone())?.valid());
        let mut cases = Vec::new();
        let mut bad = value.clone();
        bad["playback"]["sessions"][0]["state"] = json!(12);
        cases.push(bad);
        let mut bad = value.clone();
        bad["playback"]["sessions"][0]["title"] = json!("x".repeat(1025));
        cases.push(bad);
        let mut bad = value.clone();
        bad["playback"]["access_enabled"] = json!(false);
        cases.push(bad);
        let mut bad = value.clone();
        bad["playback"]["sessions"][0]["state"] = Value::Null;
        cases.push(bad);
        let mut bad = value.clone();
        bad["playback"]["sessions"] = json!(vec![value["playback"]["sessions"][0].clone(); 5]);
        cases.push(bad);
        for case in cases {
            assert!(!serde_json::from_value::<Readback>(case)?.valid());
        }
        let response = json!({"enabled":true,"navigation_method":"accessibility_focus_actions","focus":{"available":false},"readback":value});
        let (mut cfg, worker) = fixture(vec![Some(status(50)), Some(response.clone())])?;
        cfg.readback = true;
        assert_eq!(
            cfg.screen("192.0.2.40".parse()?, "Fixture TV", false)?,
            response
        );
        assert_eq!(finish(worker)?[1], "GET /v1/readback HTTP/1.1");
        let (mut cfg, worker) = fixture(vec![
            Some(status(50)),
            Some(
                json!({"enabled":true,"navigation_method":"accessibility_focus_actions","focus":{"available":false}}),
            ),
        ])?;
        cfg.readback = true;
        assert!(
            cfg.screen("192.0.2.40".parse()?, "Fixture TV", false)
                .is_err()
        );
        finish(worker)?;
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
            readback: false,
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
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ScreenStatus {
    enabled: bool,
    navigation_method: Option<String>,
    focus: Option<Focus>,
    readback: Option<Readback>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Readback {
    foreground_package: Option<String>,
    playback: Playback,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Playback {
    access_enabled: bool,
    truncated: bool,
    sessions: Vec<PlaybackSession>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PlaybackSession {
    package: String,
    state: Option<u8>,
    position_ms: Option<i64>,
    speed: Option<f32>,
    updated_age_ms: Option<u64>,
    title: Option<String>,
    display_title: Option<String>,
    media_id: Option<String>,
    duration_ms: Option<i64>,
}
fn bounded_text(value: &str) -> bool {
    value.len() <= 1024 && !value.chars().any(char::is_control)
}
impl Readback {
    fn valid(&self) -> bool {
        let playback = &self.playback;
        self.foreground_package
            .as_ref()
            .is_none_or(|s| bounded_text(s))
            && playback.sessions.len() <= 4
            && (playback.access_enabled || (playback.sessions.is_empty() && !playback.truncated))
            && playback.sessions.iter().all(|session| {
                !session.package.is_empty()
                    && bounded_text(&session.package)
                    && session.state.is_none_or(|s| s <= 11)
                    && session.position_ms.is_none_or(|p| p >= -1)
                    && session.duration_ms.is_none_or(|d| d >= 0)
                    && session.speed.is_none_or(|s| s.is_finite())
                    && (session.state.is_some()
                        || (session.position_ms.is_none()
                            && session.speed.is_none()
                            && session.updated_age_ms.is_none()))
                    && [&session.title, &session.display_title, &session.media_id]
                        .iter()
                        .all(|s| s.as_ref().is_none_or(|s| bounded_text(s)))
            })
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Focus {
    available: bool,
    package: Option<String>,
    class: Option<String>,
    text: Option<String>,
    description: Option<String>,
    bounds: Option<String>,
}
impl Config {
    pub(super) fn screen(&self, host: IpAddr, model: &str, capture: bool) -> Result<Value> {
        let deadline = Instant::now() + Duration::from_secs(self.timeout_seconds);
        self.status_until(host, model, deadline)?;
        if capture {
            let bytes = self.request_bytes_until(
                host,
                "/v1/prototype/screenshot",
                None,
                deadline,
                None,
                true,
            )?;
            return crate::screen::image(&bytes);
        }
        let path = if self.readback {
            "/v1/readback"
        } else {
            "/v1/prototype/status"
        };
        let value = self.request_until(host, path, None, deadline)?;
        let state: ScreenStatus =
            serde_json::from_value(value.clone()).map_err(|_| error("Invalid TV screen status"))?;
        let valid = if state.enabled {
            state.navigation_method.as_deref() == Some("accessibility_focus_actions")
                && (state.readback.as_ref().is_none_or(Readback::valid))
                && (!self.readback || state.readback.is_some())
                && state.focus.is_some_and(|focus| {
                    [
                        &focus.package,
                        &focus.class,
                        &focus.text,
                        &focus.description,
                        &focus.bounds,
                    ]
                    .iter()
                    .all(|value| match value {
                        Some(text) => text.len() <= 640,
                        None => !focus.available,
                    })
                })
        } else {
            state.navigation_method.is_none() && state.focus.is_none() && state.readback.is_none()
        };
        if !valid {
            return Err(error("Invalid TV screen status"));
        }
        Ok(value)
    }
    pub(super) fn navigate(&self, host: IpAddr, model: &str, action: &str) -> Result<Value> {
        if !crate::screen::ACTIONS.contains(&action) {
            return Err(error("Unsupported screen navigation action"));
        }
        let deadline = Instant::now() + Duration::from_secs(self.timeout_seconds);
        self.status_until(host, model, deadline)?;
        let mut random = [0u8; 16];
        getrandom::getrandom(&mut random)?;
        let id: String = random.iter().map(|b| format!("{b:02x}")).collect();
        let body = serde_json::to_vec(&json!({"action":action,"request_id":id}))?;
        // Exactly one POST; no fallback or replay after any transport or action failure.
        let response =
            self.request_until(host, "/v1/prototype/navigation", Some(&body), deadline)?;
        if response["request_id"] != id
            || response["accepted"] != true
            || response["verified"] != false
        {
            return Err(error(
                "Screen navigation was rejected or is unconfirmed; inspect a fresh screenshot, never replay automatically",
            ));
        }
        Ok(json!({"action":action,"accepted":true,"verified":false,"request_id":id}))
    }
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
        let response = self.request_bytes_until(host, path, body, deadline, permit, false)?;
        serde_json::from_slice(&response).map_err(|_| error("Invalid TV companion response"))
    }
    fn request_bytes_until(
        &self,
        host: IpAddr,
        path: &str,
        body: Option<&[u8]>,
        deadline: Instant,
        permit: Option<(&crate::brightness::Permit, u8)>,
        jpeg: bool,
    ) -> Result<Vec<u8>> {
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
        let mut reader = BufReader::with_capacity(1024, stream);
        let response = if jpeg {
            crate::http::read_bounded_response(
                &mut reader,
                Some("image/jpeg"),
                crate::screen::MAX_JPEG,
            )?
        } else {
            crate::http::read_response(&mut reader, Some("application/json"), 4096)?
        };
        Ok(response.body)
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
