//! Paired DIGI R2A / Android TV Remote v2: synchronous mutual TLS + protobuf.
use crate::{
    Result,
    config::{lock_configuration, private_permissions, read_bounded},
    device::{Adapter, Field, Hints, Input, Tool},
    error, media,
    protobuf::{self as pb, Message},
    transport::Timed,
};
use rustls::{
    ClientConfig, ClientConnection, DigitallySignedStruct, SignatureScheme, StreamOwned,
    client::{
        Resumption,
        danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    },
    crypto::{CryptoProvider, verify_tls12_signature, verify_tls13_signature},
    pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime, pem::PemObject},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    net::{IpAddr, SocketAddr},
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};
fn port() -> u16 {
    6466
}
fn timeout() -> u64 {
    8
}
fn verify() -> u64 {
    20
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub host: IpAddr,
    #[serde(default = "port")]
    pub port: u16,
    pub server_cert_sha256: String,
    pub client_cert: PathBuf,
    pub client_key: PathBuf,
    #[serde(default = "timeout")]
    pub timeout_seconds: u64,
    #[serde(default = "verify")]
    pub verify_seconds: u64,
    #[serde(default)]
    pub connect: Option<SocketAddr>,
}
#[derive(Debug)]
struct Pinned {
    fingerprint: [u8; 32],
    provider: Arc<CryptoProvider>,
}
impl ServerCertVerifier for Pinned {
    fn verify_server_cert(
        &self,
        end: &CertificateDer<'_>,
        _: &[CertificateDer<'_>],
        _: &ServerName<'_>,
        _: &[u8],
        _: UnixTime,
    ) -> std::result::Result<ServerCertVerified, rustls::Error> {
        if Sha256::digest(end.as_ref()).as_slice() != self.fingerprint {
            return Err(rustls::Error::General(
                "DIGI server certificate differs from paired identity".into(),
            ));
        }
        Ok(ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        msg: &[u8],
        cert: &CertificateDer<'_>,
        sig: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(
            msg,
            cert,
            sig,
            &self.provider.signature_verification_algorithms,
        )
    }
    fn verify_tls13_signature(
        &self,
        msg: &[u8],
        cert: &CertificateDer<'_>,
        sig: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(
            msg,
            cert,
            sig,
            &self.provider.signature_verification_algorithms,
        )
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}
#[derive(Debug, Serialize)]
pub struct Status {
    pub model: String,
    pub manufacturer: String,
    pub power: bool,
}
struct Remote {
    stream: StreamOwned<ClientConnection, Timed>,
    status: Option<Status>,
    features: u64,
    configured: bool,
    active: bool,
}
impl Config {
    fn validate(&self) -> Result<()> {
        media::valid_endpoint(self.port, self.connect, self.timeout_seconds)?;
        media::hex::<32>(&self.server_cert_sha256)?;
        if self.client_cert.as_os_str().is_empty()
            || self.client_key.as_os_str().is_empty()
            || !(1..=60).contains(&self.verify_seconds)
        {
            return Err(error("Invalid DIGI credentials or verification interval"));
        }
        Ok(())
    }
    fn remote(&self, deadline: Instant) -> Result<Remote> {
        private_permissions(&self.client_cert)?;
        private_permissions(&self.client_key)?;
        let cert = CertificateDer::from_pem_slice(&read_bounded(
            &self.client_cert,
            "DIGI client certificate",
        )?)
        .map_err(|_| error("Invalid DIGI client certificate"))?;
        let key =
            PrivateKeyDer::from_pem_slice(&read_bounded(&self.client_key, "DIGI client key")?)
                .map_err(|_| error("Invalid DIGI client key"))?;
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let verifier = Arc::new(Pinned {
            fingerprint: media::hex::<32>(&self.server_cert_sha256)?,
            provider: provider.clone(),
        });
        let mut config = ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()?
            .dangerous()
            .with_custom_certificate_verifier(verifier)
            .with_client_auth_cert(vec![cert], key)
            .map_err(|_| error("DIGI client certificate/key mismatch"))?;
        config.resumption = Resumption::disabled();
        config.max_fragment_size = Some(1024);
        let mut connection =
            ClientConnection::new(Arc::new(config), ServerName::IpAddress(self.host.into()))?;
        connection.set_buffer_limit(Some(8192));
        let socket = Timed::connect(
            self.connect
                .unwrap_or(SocketAddr::new(self.host, self.port)),
            deadline,
        )?;
        let mut remote = Remote {
            stream: StreamOwned::new(connection, socket),
            status: None,
            features: 0,
            configured: false,
            active: false,
        };
        remote.until(|r| r.active && r.status.is_some())?;
        Ok(remote)
    }
}
impl Remote {
    fn until(&mut self, done: impl Fn(&Self) -> bool) -> Result<()> {
        for _ in 0..128 {
            if done(self) {
                return Ok(());
            }
            self.receive()?;
        }
        Err(error(
            "Too many Android remote messages before state confirmation",
        ))
    }
    fn receive(&mut self) -> Result<()> {
        let bytes = pb::read(&mut self.stream)?;
        let message = Message::new(&bytes)?;
        if message.nested(3)?.is_some() {
            return Err(error("DIGI remote reported a protocol error"));
        }
        if let Some(configure) = message.nested(1)? {
            if self.configured {
                return Err(error("Repeated DIGI configure message"));
            }
            let features = configure
                .int(1)?
                .ok_or_else(|| error("Missing DIGI features"))?;
            if features & 35 != 35 {
                return Err(error("DIGI lacks required key/power features"));
            }
            self.features = features & 35;
            let device = configure
                .nested(2)?
                .ok_or_else(|| error("Missing DIGI device identity"))?;
            let model = device.text(1)?.ok_or_else(|| error("Missing DIGI model"))?;
            let vendor = device
                .text(2)?
                .ok_or_else(|| error("Missing DIGI manufacturer"))?;
            if model != "DIGI R2A" || vendor != "ZTE" {
                return Err(error("DIGI live model differs from R2A configuration"));
            }
            self.status = Some(Status {
                model: model.into(),
                manufacturer: vendor.into(),
                power: false,
            });
            let mut info = Vec::new();
            pb::integer(3, 1, &mut info);
            pb::bytes(4, b"1", &mut info);
            pb::bytes(5, b"atvremote", &mut info);
            pb::bytes(6, b"1.0.0", &mut info);
            let mut body = Vec::new();
            pb::integer(1, self.features, &mut body);
            pb::bytes(2, &info, &mut body);
            pb::write(&mut self.stream, 1, &body)?;
            self.configured = true;
            // Device identity alone is not a power reading.
        }
        if let Some(active) = message.nested(2)? {
            if !self.configured || active.int(1)?.unwrap_or(0) > u32::MAX as u64 {
                return Err(error("Unexpected DIGI active message"));
            }
            let mut body = Vec::new();
            pb::integer(1, self.features, &mut body);
            pb::write(&mut self.stream, 2, &body)?;
        }
        if let Some(start) = message.nested(40)? {
            if !self.configured {
                return Err(error("DIGI power reading before identity"));
            }
            let value = start.int(1)?.unwrap_or(0);
            if value > 1 {
                return Err(error("Invalid DIGI power reading"));
            }
            self.status
                .as_mut()
                .ok_or_else(|| error("Missing DIGI identity"))?
                .power = value == 1;
            self.active = true;
        }
        if let Some(ping) = message.nested(8)? {
            let mut body = Vec::new();
            pb::integer(1, ping.int(1)?.unwrap_or(0), &mut body);
            pb::write(&mut self.stream, 9, &body)?;
        }
        Ok(())
    }
    fn key(&mut self, code: u64) -> Result<()> {
        let mut body = Vec::new();
        pb::integer(1, code, &mut body);
        pb::integer(2, 3, &mut body);
        pb::write(&mut self.stream, 10, &body)
    }
}
pub struct Manager {
    config: Config,
    _lock: File,
}
impl Manager {
    pub fn open(path: &Path) -> Result<Self> {
        let mut config: Config = media::load(path)?;
        config.validate()?;
        let parent = path.parent().unwrap_or(Path::new("."));
        if config.client_cert.is_relative() {
            config.client_cert = parent.join(&config.client_cert);
        }
        if config.client_key.is_relative() {
            config.client_key = parent.join(&config.client_key);
        }
        private_permissions(&config.client_cert)?;
        private_permissions(&config.client_key)?;
        Ok(Self {
            config,
            _lock: lock_configuration(path)?,
        })
    }
    pub fn status(&self) -> Result<Status> {
        self.config
            .remote(Instant::now() + Duration::from_secs(self.config.timeout_seconds))?
            .status
            .ok_or_else(|| error("Missing DIGI power state"))
    }
    pub fn apply(&mut self, power: bool) -> Result<Value> {
        let mut remote = self
            .config
            .remote(Instant::now() + Duration::from_secs(self.config.timeout_seconds))?;
        if remote.status.as_ref().is_some_and(|s| s.power == power) {
            return Ok(json!({"applied":false,"verified":true,"status":remote.status}));
        }
        // Absolute wake/sleep keys, never the POWER toggle; send at most once.
        remote.key(if power { 224 } else { 223 })?;
        // A new total verification deadline, still no reconnect/replay of controls.
        remote
            .stream
            .sock
            .set_deadline(Instant::now() + Duration::from_secs(self.config.verify_seconds));
        remote.until(|r| r.status.as_ref().is_some_and(|s| s.power == power))?;
        Ok(json!({"applied":true,"verified":true,"status":remote.status}))
    }
    pub fn key(&mut self, key: &str) -> Result<Value> {
        let code = key_code(key).ok_or_else(|| error("Unsupported DIGI key"))?;
        let mut remote = self
            .config
            .remote(Instant::now() + Duration::from_secs(self.config.timeout_seconds))?;
        remote.key(code)?;
        Ok(json!({"sent":true,"verified":false,"key":key}))
    }
}
fn key_code(key: &str) -> Option<u64> {
    Some(match key {
        "home" => 3,
        "back" => 4,
        "up" => 19,
        "down" => 20,
        "left" => 21,
        "right" => 22,
        "enter" => 23,
        "volume_up" => 24,
        "volume_down" => 25,
        "mute" => 164,
        "play_pause" => 85,
        "play" => 126,
        "pause" => 127,
        "channel_up" => 166,
        "channel_down" => 167,
        _ => return None,
    })
}
const KEY: Input = Input::Field {
    field: Field {
        name: "key",
        schema: || json!({"type":"string","enum":["home","back","up","down","left","right","enter","volume_up","volume_down","mute","play_pause","play","pause","channel_up","channel_down"]}),
        valid: |v| v.as_str().and_then(key_code).is_some(),
        error: "Unsupported DIGI key",
    },
    shape_error: "Expected only key",
};
impl Adapter for Manager {
    const TOOLS: &'static [Tool] = &[
        Tool {
            name: "digi_status",
            description: "Read pinned DIGI R2A identity and fresh remote power state",
            input: Input::Empty,
            hints: Hints::READ,
        },
        Tool {
            name: "digi_apply",
            description: "Apply absolute wake/sleep and verify the reported power state",
            input: media::POWER,
            hints: Hints::CONTROL,
        },
        Tool {
            name: "digi_key",
            description: "Send one supported DIGI remote key (no power toggle)",
            input: KEY,
            hints: Hints {
                idempotent: false,
                ..Hints::CONTROL
            },
        },
    ];
    fn open(path: &Path) -> Result<Self> {
        Self::open(path)
    }
    fn execute(&mut self, name: &str, args: &Value) -> Result<Value> {
        match name {
            "digi_status" => Ok(serde_json::to_value(self.status()?)?),
            "digi_apply" => self.apply(
                args["power"]
                    .as_bool()
                    .ok_or_else(|| error("Expected boolean power"))?,
            ),
            "digi_key" => self.key(args["key"].as_str().ok_or_else(|| error("Expected key"))?),
            _ => Err(error("Unknown DIGI operation")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustls::{RootCertStore, ServerConfig, ServerConnection, server::WebPkiClientVerifier};
    use std::{net::TcpListener, thread};
    mod support {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/support/media.rs"
        ));
    }
    fn server(
        on: bool,
        command: Option<u64>,
        bad_model: bool,
        confirm: bool,
    ) -> Result<(SocketAddr, thread::JoinHandle<Result<()>>, String)> {
        let cert = CertificateDer::from_pem_slice(include_bytes!(
            "../tests/fixtures/media/test-cert.pem"
        ))?;
        let pin = Sha256::digest(cert.as_ref())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let key =
            PrivateKeyDer::from_pem_slice(include_bytes!("../tests/fixtures/media/test-key.pem"))?;
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let mut roots = RootCertStore::empty();
        roots.add(cert.clone())?;
        let verifier =
            WebPkiClientVerifier::builder_with_provider(Arc::new(roots), provider.clone())
                .build()?;
        let config = ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()?
            .with_client_cert_verifier(verifier)
            .with_single_cert(vec![cert], key)?;
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let address = listener.local_addr()?;
        let handle = thread::spawn(move || -> Result<()> {
            let (socket, _) = listener.accept()?;
            socket.set_read_timeout(Some(Duration::from_secs(3)))?;
            socket.set_write_timeout(Some(Duration::from_secs(3)))?;
            let mut stream = StreamOwned::new(ServerConnection::new(Arc::new(config))?, socket);
            let mut info = Vec::new();
            pb::bytes(1, if bad_model { b"wrong" } else { b"DIGI R2A" }, &mut info);
            pb::bytes(2, b"ZTE", &mut info);
            let mut configure = Vec::new();
            pb::integer(1, 35, &mut configure);
            pb::bytes(2, &info, &mut configure);
            // Pin mismatch aborts the handshake before the application can send data.
            if pb::write(&mut stream, 1, &configure).is_err() {
                return Ok(());
            }
            if bad_model {
                return Ok(());
            }
            let response = pb::read(&mut stream)?;
            assert_eq!(
                Message::new(&response)?
                    .nested(1)?
                    .expect("configure")
                    .int(1)?,
                Some(35)
            );
            assert!(
                stream
                    .conn
                    .peer_certificates()
                    .is_some_and(|certs| !certs.is_empty())
            );
            pb::write(&mut stream, 2, &[])?;
            let response = pb::read(&mut stream)?;
            assert_eq!(
                Message::new(&response)?
                    .nested(2)?
                    .expect("active")
                    .int(1)?,
                Some(35)
            );
            let mut start = Vec::new();
            if on {
                pb::integer(1, 1, &mut start);
            }
            pb::write(&mut stream, 40, &start)?;
            if let Some(command) = command {
                let mut ping = Vec::new();
                pb::integer(1, 42, &mut ping);
                pb::write(&mut stream, 8, &ping)?;
                let mut saw_key = false;
                for _ in 0..2 {
                    let response = pb::read(&mut stream)?;
                    let message = Message::new(&response)?;
                    if let Some(key) = message.nested(10)? {
                        assert_eq!(key.int(1)?, Some(command));
                        assert_eq!(key.int(2)?, Some(3));
                        saw_key = true;
                    } else {
                        assert_eq!(message.nested(9)?.expect("pong").int(1)?, Some(42));
                    }
                }
                assert!(saw_key);
                if !confirm {
                    return Ok(());
                }
                let mut start = Vec::new();
                if command == 224 {
                    pb::integer(1, 1, &mut start);
                }
                pb::write(&mut stream, 40, &start)?;
            }
            Ok(())
        });
        Ok((address, handle, pin))
    }
    fn config(temp: &support::Directory, address: SocketAddr, pin: String) -> Result<PathBuf> {
        let (cert, key) = temp.certs()?;
        temp.config("digi.json",&json!({"host":"192.0.2.30","connect":address,"server_cert_sha256":pin,"client_cert":cert,"client_key":key,"timeout_seconds":2,"verify_seconds":2}))
    }
    #[test]
    fn paired_status_and_absolute_wake_sleep_verify_over_mutual_tls() -> Result<()> {
        for (on, command) in [(true, None), (false, Some(224)), (true, Some(223))] {
            let temp = support::Directory::new()?;
            let (address, worker, pin) = server(on, command, false, true)?;
            let mut adapter = Manager::open(&config(&temp, address, pin)?)?;
            if let Some(command) = command {
                let result = adapter.apply(command == 224)?;
                assert_eq!(result["verified"], true);
                assert_eq!(result["status"]["power"], command == 224);
            } else {
                assert!(adapter.status()?.power);
            }
            worker.join().expect("server")?;
        }
        Ok(())
    }
    #[test]
    fn wrong_pin_or_model_blocks_before_any_remote_command() -> Result<()> {
        for bad_model in [true, false] {
            let temp = support::Directory::new()?;
            let (address, worker, pin) = server(true, None, bad_model, true)?;
            let mut adapter = Manager::open(&config(
                &temp,
                address,
                if bad_model { pin } else { "00".repeat(32) },
            )?)?;
            assert!(adapter.apply(false).is_err());
            worker.join().expect("server")?;
        }
        Ok(())
    }
    #[test]
    fn on_already_correct_sends_no_command_and_lock_excludes_duplicates() -> Result<()> {
        let temp = support::Directory::new()?;
        let (address, worker, pin) = server(true, None, false, true)?;
        let path = config(&temp, address, pin)?;
        let mut adapter = Manager::open(&path)?;
        assert!(Manager::open(&path).is_err());
        let result = adapter.apply(true)?;
        assert_eq!(result["applied"], false);
        worker.join().expect("server")?;
        Ok(())
    }
    #[test]
    fn uncertain_sleep_is_not_reconnected_or_replayed() -> Result<()> {
        let temp = support::Directory::new()?;
        let (address, worker, pin) = server(true, Some(223), false, false)?;
        let mut adapter = Manager::open(&config(&temp, address, pin)?)?;
        assert!(adapter.apply(false).is_err());
        worker.join().expect("server")?;
        Ok(())
    }
}
