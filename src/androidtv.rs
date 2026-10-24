//! Paired DIGI and TCL Android TV Remote v2: synchronous mutual TLS + protobuf.
use crate::{
    Result,
    config::{lock_configuration, private_permissions, read_bounded, save_private},
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
    net::{IpAddr, SocketAddr},
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
mod activity;
pub mod companion;
mod input;
mod power;
use input::{KEYS, key_code};
fn port() -> u16 {
    6466
}
fn timeout() -> u64 {
    8
}
fn verify() -> u64 {
    20
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub host: IpAddr,
    /// Exact live TCL model. DIGI retains its fixed R2A identity.
    #[serde(default)]
    pub model: Option<String>,
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
    pub wake: Option<power::Wake>,
    #[serde(default)]
    pub cast_wake: Option<crate::cast::Wake>,
    #[serde(default)]
    pub power_on_key: power::PowerOnKey,
    #[serde(default)]
    pub connect: Option<SocketAddr>,
    #[serde(default)]
    pub companion: Option<companion::Config>,
    #[serde(skip)]
    gate: crate::transport::Gate,
    #[serde(skip)]
    schedule: Option<crate::brightness::Control>,
}
#[derive(Debug)]
pub(crate) struct Pinned {
    pub(crate) fingerprint: [u8; 32],
    pub(crate) provider: Arc<CryptoProvider>,
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
                "Server certificate differs from paired identity".into(),
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
    pub current_app: Option<String>,
    pub volume: Option<Volume>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub picture: Option<companion::Status>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub picture_error: Option<String>,
}
#[derive(Debug, Serialize)]
pub struct Volume {
    pub level: u16,
    pub max: u16,
    pub muted: bool,
}
struct Remote<'a> {
    _lease: crate::transport::Lease,
    expected_model: &'a str,
    expected_manufacturer: &'static str,
    stream: StreamOwned<ClientConnection, Timed>,
    frames: pb::Framed,
    status: Option<Status>,
    features: u64,
    configured: bool,
    active: bool,
    ime: Option<(u64, u64)>,
    supported_features: u64,
    requested_features: u64,
    voice_session: Option<u64>,
    ime_context_seen: bool,
}
impl Config {
    fn validate<const TCL: bool>(&self) -> Result<()> {
        if let Some(companion) = &self.companion {
            if !TCL {
                return Err(error("Picture companion is TCL-only"));
            }
            companion.validate()?;
        }
        if TCL {
            if !self.model.as_deref().is_some_and(|m| {
                !m.trim().is_empty() && m.len() <= 128 && !m.chars().any(char::is_control)
            }) {
                return Err(error("TCL requires an exact live model (1–128 bytes)"));
            }
        } else if self.model.is_some() {
            return Err(error("DIGI uses its fixed ZTE DIGI R2A identity"));
        }
        media::valid_endpoint(self.port, self.connect, self.timeout_seconds)?;
        if !TCL
            && (self.wake.is_some()
                || self.cast_wake.is_some()
                || self.power_on_key != power::PowerOnKey::Wakeup)
        {
            return Err(error(
                "Network wake and configurable power keys are TCL-only",
            ));
        }
        if let Some(wake) = &self.wake {
            wake.validate()?;
        }
        if let Some(wake) = &self.cast_wake {
            wake.validate()?;
        }
        media::hex::<32>(&self.server_cert_sha256)?;
        if self.client_cert.as_os_str().is_empty()
            || self.client_key.as_os_str().is_empty()
            || !(1..=60).contains(&self.verify_seconds)
        {
            return Err(error(
                "Invalid Android TV credentials or verification interval",
            ));
        }
        Ok(())
    }
    fn remote<const TCL: bool>(&self, deadline: Instant) -> Result<Remote<'_>> {
        self.remote_input::<TCL>(deadline, false)
    }
    fn remote_input<const TCL: bool>(&self, deadline: Instant, voice: bool) -> Result<Remote<'_>> {
        let lease = self.gate.enter()?;
        private_permissions(&self.client_cert)?;
        private_permissions(&self.client_key)?;
        let cert = CertificateDer::from_pem_slice(&read_bounded(
            &self.client_cert,
            "Android TV client certificate",
        )?)
        .map_err(|_| error("Invalid Android TV client certificate"))?;
        let key = PrivateKeyDer::from_pem_slice(&read_bounded(
            &self.client_key,
            "Android TV client key",
        )?)
        .map_err(|_| error("Invalid Android TV client key"))?;
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
            .map_err(|_| error("Android TV client certificate/key mismatch"))?;
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
            _lease: lease,
            expected_model: self.model.as_deref().unwrap_or("DIGI R2A"),
            expected_manufacturer: if TCL { "TCL" } else { "ZTE" },
            stream: StreamOwned::new(connection, socket),
            frames: pb::Framed::default(),
            status: None,
            features: 0,
            configured: false,
            active: false,
            ime: None,
            supported_features: 0,
            requested_features: 35 | 4 | 64 | 512 | if voice { 8 } else { 0 },
            voice_session: None,
            ime_context_seen: false,
        };
        remote.until(|r| r.active && r.status.is_some())?;
        Ok(remote)
    }
}
impl Remote<'_> {
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
        let bytes = self.frames.read(&mut self.stream)?;
        let message = Message::new(&bytes)?;
        if message.nested(3)?.is_some() {
            return Err(error("Android TV remote reported a protocol error"));
        }
        if let Some(configure) = message.nested(1)? {
            if self.configured {
                return Err(error("Repeated Android TV configure message"));
            }
            let features = configure
                .int(1)?
                .ok_or_else(|| error("Missing Android TV features"))?;
            if features & 35 != 35 {
                return Err(error("Android TV lacks required key/power features"));
            }
            if features > u32::MAX as u64 {
                return Err(error("Invalid Android TV feature flags"));
            }
            self.supported_features = features;
            self.features = features & self.requested_features;
            let device = configure
                .nested(2)?
                .ok_or_else(|| error("Missing Android TV device identity"))?;
            let model = device
                .text(1)?
                .ok_or_else(|| error("Missing Android TV model"))?;
            let vendor = device
                .text(2)?
                .ok_or_else(|| error("Missing Android TV manufacturer"))?;
            if model != self.expected_model || vendor != self.expected_manufacturer {
                return Err(error(
                    "Android TV live model/vendor differs from configuration",
                ));
            }
            self.status = Some(Status {
                model: model.into(),
                manufacturer: vendor.into(),
                power: false,
                current_app: None,
                volume: None,
                picture: None,
                picture_error: None,
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
                return Err(error("Unexpected Android TV active message"));
            }
            let mut body = Vec::new();
            pb::integer(1, self.features, &mut body);
            pb::write(&mut self.stream, 2, &body)?;
        }
        if let Some(start) = message.nested(40)? {
            if !self.configured {
                return Err(error("Android TV power reading before identity"));
            }
            let value = start.int(1)?.unwrap_or(0);
            if value > 1 {
                return Err(error("Invalid Android TV power reading"));
            }
            self.status
                .as_mut()
                .ok_or_else(|| error("Missing Android TV identity"))?
                .power = value == 1;
            self.active = true;
        }
        if let Some(ping) = message.nested(8)? {
            let mut body = Vec::new();
            pb::integer(1, ping.int(1)?.unwrap_or(0), &mut body);
            pb::write(&mut self.stream, 9, &body)?;
        }
        if let Some(ime) = message.nested(21)? {
            self.ime = Some((ime.int(1)?.unwrap_or(0), ime.int(2)?.unwrap_or(0)));
        }
        if let Some(ime) = message.nested(20)? {
            self.ime_context_seen = true;
            if let Some(info) = ime.nested(1)? {
                let app = info.text(12)?.filter(|s| s.len() <= 256).map(str::to_owned);
                if let Some(status) = self.status.as_mut() {
                    status.current_app = app;
                }
            }
        }
        if let Some(volume) = message.nested(50)? {
            if self.features & 64 == 0 {
                return Err(error("Android TV sent unnegotiated volume information"));
            }
            let max = volume.int(6)?.unwrap_or(0);
            let level = volume.int(7)?.unwrap_or(0);
            let muted = volume.int(8)?.unwrap_or(0);
            if max == 0 || max > u16::MAX as u64 || level > max || muted > 1 {
                return Err(error("Invalid Android TV volume reading"));
            }
            if let Some(status) = self.status.as_mut() {
                status.volume = Some(Volume {
                    level: level as u16,
                    max: max as u16,
                    muted: muted == 1,
                });
            }
        }
        if let Some(voice) = message.nested(30)? {
            if self.features & 8 == 0 || self.voice_session.is_some() {
                return Err(error("Unexpected Android TV voice session"));
            }
            let session = voice.int(1)?.unwrap_or(0);
            if session > i32::MAX as u64 {
                return Err(error("Invalid Android TV voice session"));
            }
            self.voice_session = Some(session);
        }
        Ok(())
    }
    fn key(&mut self, code: u64) -> Result<()> {
        self.key_direction(code, 3)
    }
    /// TCL reports power before its input receiver is ready. Collect the
    /// negotiated initial IME/volume reports before issuing any on-state control.
    /// These are session-local signals, never cached across connections.
    fn prepare_control<const TCL: bool>(&mut self, operation_deadline: Instant) -> Result<()> {
        if !TCL || !self.status.as_ref().is_some_and(|s| s.power) {
            return Ok(());
        }
        let deadline = operation_deadline.min(Instant::now() + Duration::from_secs(1));
        self.stream.sock.set_deadline(deadline);
        let ready = self.until(|r| {
            (r.features & 4 == 0 || r.ime_context_seen)
                && (r.features & 64 == 0 || r.status.as_ref().is_some_and(|s| s.volume.is_some()))
        });
        self.stream.sock.set_deadline(operation_deadline);
        ready.map_err(|_| error("TCL input readiness was not confirmed; no control sent"))
    }
    fn key_direction(&mut self, code: u64, direction: u64) -> Result<()> {
        let mut body = Vec::new();
        pb::integer(1, code, &mut body);
        pb::integer(2, direction, &mut body);
        pb::write(&mut self.stream, 10, &body)
    }
    /// Collect supplemental state after remote_start without treating EOF as off.
    fn read_details(&mut self, operation_deadline: Instant) -> Result<()> {
        let deadline = operation_deadline.min(Instant::now() + Duration::from_millis(500));
        self.stream.sock.set_deadline(deadline);
        for _ in 0..128 {
            if Instant::now() >= deadline {
                return Ok(());
            }
            if let Err(failure) = self.receive() {
                if failure.downcast_ref::<std::io::Error>().is_some_and(|e| {
                    matches!(
                        e.kind(),
                        std::io::ErrorKind::TimedOut
                            | std::io::ErrorKind::WouldBlock
                            | std::io::ErrorKind::UnexpectedEof
                            | std::io::ErrorKind::ConnectionReset
                    )
                }) {
                    return Ok(());
                }
                return Err(failure);
            }
        }
        Err(error("Too many Android remote state messages"))
    }
    fn finish_key(&mut self, operation_deadline: Instant) -> Result<()> {
        self.finish_control(operation_deadline, Duration::from_millis(500))
    }
    fn finish_control(&mut self, operation_deadline: Instant, observe: Duration) -> Result<()> {
        // Closing immediately can reset TCP with unread TV messages and discard
        // the key. Keep servicing the session briefly, then close TLS cleanly.
        // Neither pings nor this wait acknowledge execution of the key.
        let deadline =
            (operation_deadline - Duration::from_millis(100)).min(Instant::now() + observe);
        self.stream.sock.set_deadline(deadline);
        let mut drained = false;
        for _ in 0..128 {
            if Instant::now() >= deadline {
                drained = true;
                break;
            }
            if let Err(failure) = self.receive() {
                if failure.downcast_ref::<std::io::Error>().is_some_and(|e| {
                    matches!(
                        e.kind(),
                        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                    )
                }) {
                    drained = true;
                    break;
                }
                return Err(error(
                    "Android TV control was sent but the session failed; outcome unknown; do not replay",
                ));
            }
        }
        if !drained {
            return Err(error(
                "Too many Android remote messages after control; outcome unknown; do not replay",
            ));
        }
        self.stream.conn.send_close_notify();
        // The read deadline has elapsed; allow a bounded TLS close write.
        self.stream
            .sock
            .set_deadline(operation_deadline.min(Instant::now() + Duration::from_millis(100)));
        std::io::Write::flush(&mut self.stream)?;
        Ok(())
    }
}
/// DIGI preserves its fixed identity and compatibility tools.
pub type Manager = RemoteManager<false>;
/// TCL uses an exact configured model and the standard media tools.
pub type TclManager = RemoteManager<true>;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ShutdownMemory {
    identity: [u8; 32],
    attempt: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    requested_power: Option<bool>,
}
struct ShutdownGuard {
    path: PathBuf,
    memory: ShutdownMemory,
}
impl ShutdownGuard {
    fn open(path: &Path, config: &Config) -> Result<Self> {
        let identity: [u8; 32] = Sha256::digest(format!(
            "{}:{}:{}:{}",
            config.host,
            config.port,
            config.model.as_deref().unwrap_or_default(),
            config.server_cert_sha256.to_ascii_lowercase()
        ))
        .into();
        let path = path.with_extension("power.state.json");
        let memory = match read_bounded(&path, "TCL shutdown state") {
            Ok(bytes) => {
                private_permissions(&path)?;
                let memory: ShutdownMemory = serde_json::from_slice(&bytes)
                    .map_err(|_| error("Invalid TCL shutdown state"))?;
                if memory.identity != identity {
                    return Err(error("TCL shutdown state belongs to a different device"));
                }
                memory
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => ShutdownMemory {
                identity,
                attempt: None,
                requested_power: None,
            },
            Err(e) => return Err(e.into()),
        };
        Ok(Self { path, memory })
    }
    fn record_attempt(&mut self, power: bool) -> Result<()> {
        let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
        if self
            .memory
            .attempt
            .is_some_and(|last| now < last.saturating_add(120))
        {
            return Err(error(
                "Recent TCL shutdown or wake attempt; another power control is blocked for 120 seconds",
            ));
        }
        let memory = ShutdownMemory {
            identity: self.memory.identity,
            attempt: Some(now),
            requested_power: Some(power),
        };
        save_private(&self.path, &memory)?;
        self.memory = memory;
        Ok(())
    }
    fn confirmed(&mut self, power: bool) -> Result<()> {
        if self.memory.attempt.is_some() && self.memory.requested_power.unwrap_or(false) == power {
            let memory = ShutdownMemory {
                identity: self.memory.identity,
                attempt: None,
                requested_power: None,
            };
            save_private(&self.path, &memory)?;
            self.memory = memory;
        }
        Ok(())
    }
    /// Cancel only our newly persisted attempt when no device-changing write
    /// was attempted. Never use this after a partial write or verification error.
    fn cancel_unsent(&mut self) -> Result<()> {
        let memory = ShutdownMemory {
            identity: self.memory.identity,
            attempt: None,
            requested_power: None,
        };
        save_private(&self.path, &memory)?;
        self.memory = memory;
        Ok(())
    }
}

pub struct RemoteManager<const TCL: bool> {
    config: Config,
    shutdown_guard: Option<ShutdownGuard>,
    _lock: crate::config::FileLock,
}
impl<const TCL: bool> RemoteManager<TCL> {
    pub fn open(path: &Path) -> Result<Self> {
        Self::from_source(&crate::config::Source::file(path))
    }
    pub fn from_source(source: &crate::config::Source<'_>) -> Result<Self> {
        let path = source.path();
        let mut config: Config = source.decode()?;
        config.validate::<TCL>()?;
        let parent = path.parent().unwrap_or(Path::new("."));
        if config.client_cert.is_relative() {
            config.client_cert = parent.join(&config.client_cert);
        }
        if config.client_key.is_relative()
            && !crate::config::environment_secret_path(&config.client_key)
        {
            config.client_key = parent.join(&config.client_key);
        }
        private_permissions(&config.client_cert)?;
        private_permissions(&config.client_key)?;
        let lock = lock_configuration(path)?;
        let shutdown_guard = if TCL {
            Some(ShutdownGuard::open(path, &config)?)
        } else {
            None
        };
        Ok(Self {
            config,
            shutdown_guard,
            _lock: lock,
        })
    }
    pub fn backlight_schedule_identity(&self) -> Result<String> {
        let companion = self
            .config
            .companion
            .as_ref()
            .filter(|_| TCL)
            .ok_or_else(|| error("TV curve follower requires a configured TCL companion"))?;
        let identity = format!(
            "{}:{}:{}:{}:{}:{}:{}",
            self.config.host,
            self.config.port,
            self.config.server_cert_sha256,
            companion.port,
            companion.server_cert_sha256,
            companion.fingerprint,
            companion.incremental
        );
        Ok(Sha256::digest(identity.as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect())
    }
    pub fn status(&self) -> Result<Status> {
        let deadline = Instant::now() + Duration::from_secs(self.config.timeout_seconds);
        let mut remote = self.config.remote::<TCL>(deadline)?;
        remote.read_details(deadline)?;
        let mut status = remote
            .status
            .ok_or_else(|| error("Missing Android TV power state"))?;
        if let Some(companion) = &self.config.companion {
            match companion.status(self.config.host, &status.model) {
                Ok(picture) => status.picture = Some(picture),
                Err(e) => status.picture_error = Some(e.to_string()),
            }
        }
        Ok(status)
    }
    pub fn apply(&mut self, power: bool) -> Result<Value> {
        if TCL {
            return power::apply(self, power);
        }
        let deadline = Instant::now() + Duration::from_secs(self.config.timeout_seconds);
        let mut remote = self.config.remote::<TCL>(deadline)?;
        remote.prepare_control::<TCL>(deadline)?;
        if remote.status.as_ref().is_some_and(|s| s.power == power) {
            return Ok(json!({"applied":false,"verified":true,"status":remote.status}));
        }
        // DIGI retains absolute keys. TCL's guarded firmware-specific path above
        // also owns its bounded network recovery and structured outcomes.
        let code = if power { 224 } else { 223 };
        remote.key(code)?;
        // A new total verification deadline, still no reconnect/replay of controls.
        remote
            .stream
            .sock
            .set_deadline(Instant::now() + Duration::from_secs(self.config.verify_seconds));
        remote.until(|r| r.status.as_ref().is_some_and(|s| s.power == power))?;
        Ok(json!({"applied":true,"verified":true,"status":remote.status}))
    }
    pub fn key(&mut self, key: &str) -> Result<Value> {
        let code = key_code(key).ok_or_else(|| error("Unsupported Android TV key"))?;
        let deadline = Instant::now() + Duration::from_secs(self.config.timeout_seconds);
        let mut remote = self.config.remote::<TCL>(deadline)?;
        remote.prepare_control::<TCL>(deadline)?;
        if let Some(status) = remote.status.as_mut() {
            status.current_app = None;
        }
        remote.key(code)?;
        remote.finish_key(deadline)?;
        Ok(
            json!({"sent":true,"verified":false,"key":key,"current_app":remote.status.as_ref().and_then(|s|s.current_app.as_deref())}),
        )
    }
    fn navigate(&mut self, action: &str) -> Result<Value> {
        let _lease = self.config.gate.enter()?;
        self.config
            .companion
            .as_ref()
            .filter(|_| TCL)
            .ok_or_else(|| error("Screen navigation requires a configured TV companion"))?
            .navigate(
                self.config.host,
                self.config.model.as_deref().unwrap_or(""),
                action,
            )
    }
    fn configure(&mut self, settings: &Value) -> Result<Value> {
        crate::settings::validate(self.settings(), settings)?;
        if settings.as_object().is_none_or(|s| s.len() != 1) {
            return Err(error("Send one Android TV setting at a time"));
        }
        if let Some(level) = settings["backlight_level"].as_u64() {
            if let Some(control) = &self.config.schedule {
                control.manual(level as u8, chrono::Utc::now())?;
            }
            let deadline = Instant::now() + Duration::from_secs(self.config.timeout_seconds);
            let mut remote = self.config.remote::<TCL>(deadline)?;
            remote.read_details(deadline)?;
            if !remote.status.as_ref().is_some_and(|s| s.power) {
                return Err(error("TV must be confirmed on for picture controls"));
            }
            return self
                .config
                .companion
                .as_ref()
                .ok_or_else(|| error("TV companion is not configured"))?
                .set(
                    self.config.host,
                    self.config.model.as_deref().unwrap_or(""),
                    level as u8,
                );
        }
        if let Some(link) = settings["app_link"].as_str() {
            validate_app_link(link)?;
        }
        let deadline = Instant::now() + Duration::from_secs(self.config.timeout_seconds);
        let mut remote = self.config.remote::<TCL>(deadline)?;
        remote.prepare_control::<TCL>(deadline)?;
        if let Some(link) = settings["app_link"].as_str() {
            if remote.features & 512 == 0 {
                return Err(error("Android TV does not support app links"));
            }
            let mut body = Vec::new();
            pb::bytes(1, link.as_bytes(), &mut body);
            pb::write(&mut remote.stream, 90, &body)?;
        } else if let Some(text) = settings["text"].as_str() {
            if remote.features & 4 == 0 {
                return Err(error("Android TV does not support text input"));
            }
            remote.until(|r| r.ime.is_some())?;
            let (ime, field) = remote
                .ime
                .ok_or_else(|| error("No focused Android TV text field"))?;
            let mut object = Vec::new();
            let cursor = text.chars().count().saturating_sub(1) as u64;
            pb::integer(1, cursor, &mut object);
            pb::integer(2, cursor, &mut object);
            pb::bytes(3, text.as_bytes(), &mut object);
            let mut edit = Vec::new();
            pb::integer(1, 1, &mut edit);
            pb::bytes(2, &object, &mut edit);
            let mut batch = Vec::new();
            pb::integer(1, ime, &mut batch);
            pb::integer(2, field, &mut batch);
            pb::bytes(3, &edit, &mut batch);
            pb::write(&mut remote.stream, 21, &batch)?;
        }
        remote.finish_key(deadline)?;
        Ok(
            json!({"sent":true,"verified":false,"note":"Android remote does not acknowledge app/text controls; never replay automatically"}),
        )
    }
    fn settings(&self) -> &'static [crate::settings::Setting] {
        if TCL && self.config.companion.is_some() {
            PICTURE_SETTINGS
        } else {
            SETTINGS
        }
    }
}
fn validate_app_link(link: &str) -> Result<()> {
    if link == "netflix://" {
        return Ok(());
    }
    if let Some(page) = link.strip_prefix("stremio:///") {
        // Only documented content/navigation pages, never addon installation.
        let (path, query) = page.split_once('?').unwrap_or((page, ""));
        let parts: Vec<_> = path.split('/').collect();
        let safe = |s: &str| {
            !s.is_empty()
                && s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_-.:,%".contains(&b))
        };
        let valid = match parts.as_slice() {
            ["board" | "discover" | "library"] => query.is_empty(),
            ["search"] => query
                .strip_prefix("search=")
                .is_some_and(|q| safe(q) && !q.contains(',')),
            ["detail", kind, id] | ["detail", kind, id, ""] => {
                safe(kind) && safe(id) && query.is_empty()
            }
            ["detail", kind, id, video] => {
                safe(kind)
                    && safe(id)
                    && safe(video)
                    && matches!(query, "" | "autoPlay=true" | "autoPlay=false")
            }
            _ => false,
        };
        if valid {
            return Ok(());
        }
        return Err(error(
            "Unsupported Stremio page link; addon installation is unavailable",
        ));
    }
    crate::web::Url::parse(link).map(|_| ())
}
const SETTINGS: &[crate::settings::Setting] = &[
    crate::settings::Setting {
        name: "app_link",
        kind: crate::settings::Kind::Text { max: 2048 },
    },
    crate::settings::Setting {
        name: "text",
        kind: crate::settings::Kind::Text { max: 512 },
    },
];
const PICTURE_SETTINGS: &[crate::settings::Setting] = &[
    SETTINGS[0],
    SETTINGS[1],
    crate::settings::Setting::integer("backlight_level", 0, 100, 1),
];
const KEY: Input = Input::Field {
    field: Field {
        name: "key",
        schema: || json!({"type":"string","enum":KEYS}),
        valid: |v| v.as_str().and_then(key_code).is_some(),
        error: "Unsupported Android TV key",
    },
    shape_error: "Expected only key",
};
impl<const TCL: bool> Adapter for RemoteManager<TCL> {
    fn brightness_control(&self, index: usize) -> Option<&crate::brightness::Control> {
        if index == 0 {
            self.config.schedule.as_ref()
        } else {
            None
        }
    }
    fn set_brightness_control(
        &mut self,
        index: usize,
        control: crate::brightness::Control,
    ) -> Result<()> {
        if !TCL || index != 0 || self.config.companion.is_none() {
            return Err(error("Unsupported brightness schedule"));
        }
        self.config.schedule = Some(control);
        Ok(())
    }
    fn brightness_device(&self, index: usize) -> Result<Option<crate::device::BrightnessDevice>> {
        if !TCL || index != 0 || self.config.companion.is_none() {
            return Ok(None);
        }
        Ok(Some(crate::device::BrightnessDevice::Tv(Scheduled {
            config: Arc::new(self.config.clone()),
            identity: self.backlight_schedule_identity()?,
        })))
    }

    const SUPPORT: crate::entity::Support = crate::entity::Support::MEDIA
        .union(crate::entity::Support::CONFIGURE)
        .union(crate::entity::Support::APP_INPUT)
        .union(crate::entity::Support::REMOTE_HOLD)
        .union(crate::entity::Support::VOICE)
        .union(crate::entity::Support::ACTIVITY)
        .union(if TCL {
            crate::entity::Support::WAKE
                .union(crate::entity::Support::ROUTINES)
                .union(crate::entity::Support::SCREEN)
        } else {
            crate::entity::Support::NONE
        });
    fn wake_targets<'a>(&'a self, visit: &mut impl FnMut(crate::wol::Target<'a>)) {
        if TCL && let Some(wake) = &self.config.wake {
            visit(crate::wol::Target {
                device: crate::entity::Device {
                    family: crate::device::Family::Tcl,
                    index: 0,
                },
                name: "tcl",
                mac: crate::wol::mac_bytes(&wake.mac).expect("validated TCL wake MAC"),
                verified: true,
            });
        }
    }
    fn wake_device(&mut self, index: usize) -> Result<Value> {
        if !TCL || index != 0 || self.config.wake.is_none() {
            return Err(error("Unknown Android TV wake target"));
        }
        self.apply(true)
    }
    fn entities<'a>(&'a self, visit: &mut impl FnMut(crate::entity::Entity<'a>)) {
        use crate::{
            device::Family,
            entity::{Capabilities, Domain, Entity},
        };
        visit(
            Entity::new(
                if TCL { Family::Tcl } else { Family::Digi },
                0,
                Domain::MediaPlayer,
                if TCL { "tcl" } else { "digi" },
                if TCL { "TCL TV" } else { "DIGI R2A" },
                Capabilities::Media {
                    keys: KEYS,
                    verified_off: true,
                    hold: true,
                    voice: true,
                    activities: activity::NAMES,
                    screen: TCL && self.config.companion.is_some(),
                },
            )
            .configurable(self.settings()),
        );
    }
    fn read_device(&mut self, index: usize) -> Result<Value> {
        if index != 0 {
            return Err(error("Unknown Android TV device"));
        }
        Ok(serde_json::to_value(self.status()?)?)
    }
    fn screen(&mut self, index: usize, capture: bool) -> Result<Value> {
        if index != 0 {
            return Err(error("Unknown Android TV device"));
        }
        let _lease = self.config.gate.enter()?;
        self.config
            .companion
            .as_ref()
            .filter(|_| TCL)
            .ok_or_else(|| error("Screen capture requires a configured TV companion"))?
            .screen(
                self.config.host,
                self.config.model.as_deref().unwrap_or(""),
                capture,
            )
    }
    fn control_device(
        &mut self,
        index: usize,
        command: crate::entity::Command,
        args: &Value,
    ) -> Result<Value> {
        if index != 0 {
            return Err(error("Unknown Android TV device"));
        }
        match command {
            crate::entity::Command::Navigate => self.navigate(
                args["action"]
                    .as_str()
                    .ok_or_else(|| error("Expected navigation action"))?,
            ),
            crate::entity::Command::Activity => self.activity(&args["activity"]),
            crate::entity::Command::Hold => self.hold(args),
            crate::entity::Command::Voice => self.voice(&args["audio"]),
            crate::entity::Command::Launch => self.configure(&json!({"app_link":args["url"]})),
            crate::entity::Command::Text => self.configure(&json!({"text":args["text"]})),
            crate::entity::Command::Configure => self.configure(&args["settings"]),
            crate::entity::Command::Power => self.apply(
                args["power"]
                    .as_bool()
                    .ok_or_else(|| error("Expected absolute power"))?,
            ),
            crate::entity::Command::Key => self.key(
                args["key"]
                    .as_str()
                    .ok_or_else(|| error("Expected remote key"))?,
            ),
            _ => Err(error("Unsupported Android TV command")),
        }
    }
    fn options(&mut self, index: usize) -> Result<Value> {
        if index != 0 {
            return Err(error("Unknown Android TV device"));
        }
        let deadline = Instant::now() + Duration::from_secs(self.config.timeout_seconds);
        let mut remote = self.config.remote::<TCL>(deadline)?;
        remote.read_details(deadline)?;
        Ok(
            json!({"app_link":remote.features&512!=0,"text":remote.features&4!=0,"voice":remote.supported_features&8!=0,"volume_supported":remote.features&64!=0,"current_app":remote.status.as_ref().and_then(|s|s.current_app.as_deref()),"volume":remote.status.as_ref().and_then(|s|s.volume.as_ref()),"remote_hold_max_ms":crate::media_input::MAX_HOLD_MS,"hdmi_keys_require_firmware_validation":true,"activity_parameters":activity::options(),"settings":crate::settings::schema(self.settings())}),
        )
    }

    const TOOLS: &'static [Tool] = if TCL {
        &[]
    } else {
        &[
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
        ]
    };
    fn from_source(source: &crate::config::Source<'_>) -> Result<Self> {
        Self::from_source(source)
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
            _ => Err(error("Unknown Android TV operation")),
        }
    }
}

/// Protocol-owning payload for a generic brightness lane.
pub struct Scheduled {
    config: Arc<Config>,
    identity: String,
}
impl Scheduled {
    pub(crate) fn state_key(&self) -> String {
        "schedule.state.backlight.json".into()
    }
    pub(crate) fn identity(&self) -> String {
        self.identity.clone()
    }
    pub(crate) fn run(
        &self,
        state: &mut crate::brightness::State,
        cycle: &crate::brightness::Cycle,
    ) -> Result<bool> {
        Ok(state
            .tick(
                cycle,
                &mut ScheduledIo {
                    config: &self.config,
                    deadline: cycle
                        .deadline
                        .min(Instant::now() + Duration::from_secs(self.config.timeout_seconds)),
                },
            )?
            .is_some())
    }
}
struct ScheduledIo<'a> {
    config: &'a Config,
    deadline: Instant,
}
impl crate::brightness::Io for ScheduledIo<'_> {
    type Applied = ();
    fn minimum_brightness(&self) -> u8 {
        30
    }
    fn read(&mut self) -> Result<crate::brightness::Observation> {
        let mut remote = self.config.remote::<true>(self.deadline)?;
        remote.read_details(self.deadline)?;
        let status = remote
            .status
            .as_ref()
            .ok_or_else(|| error("Unknown TV power"))?;
        if !status.power {
            return Ok(crate::brightness::Observation {
                power: false,
                brightness: 0,
                temperature: None,
            });
        }
        let picture = self
            .config
            .companion
            .as_ref()
            .ok_or_else(|| error("Missing TV companion"))?
            .status_until(self.config.host, &status.model, self.deadline)?;
        Ok(crate::brightness::Observation {
            power: true,
            brightness: picture.backlight_level,
            temperature: None,
        })
    }
    fn apply(
        &mut self,
        change: &crate::bulb::Change,
        permit: &crate::brightness::Permit,
    ) -> Result<()> {
        if change.power.is_some() || change.color_temperature_k.is_some() {
            return Err(error(
                "TV brightness lane cannot change power or temperature",
            ));
        }
        let mut remote = self.config.remote::<true>(self.deadline)?;
        remote.read_details(self.deadline)?;
        let status = remote
            .status
            .as_ref()
            .filter(|s| s.power)
            .ok_or_else(|| error("TV must be confirmed on for brightness controls"))?;
        self.config
            .companion
            .as_ref()
            .ok_or_else(|| error("Missing TV companion"))?
            .set_scheduled_until(
                self.config.host,
                &status.model,
                change
                    .brightness_percent
                    .ok_or_else(|| error("Missing brightness"))?,
                self.deadline,
                permit,
            )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustls::{RootCertStore, ServerConfig, ServerConnection, server::WebPkiClientVerifier};
    use std::{net::TcpListener, thread};
    mod support {
        include!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/support/mod.rs"));
    }
    fn server(
        on: bool,
        command: Option<u64>,
        bad_model: bool,
        confirm: bool,
    ) -> Result<(SocketAddr, thread::JoinHandle<Result<()>>, String)> {
        server_as(
            on,
            command,
            if bad_model { "wrong" } else { "DIGI R2A" },
            "ZTE",
            confirm,
            !bad_model,
        )
    }
    fn server_as(
        on: bool,
        command: Option<u64>,
        model: &'static str,
        manufacturer: &'static str,
        confirm: bool,
        valid_identity: bool,
    ) -> Result<(SocketAddr, thread::JoinHandle<Result<()>>, String)> {
        server_control(
            on,
            command.map(TestControl::Key),
            model,
            manufacturer,
            confirm,
            valid_identity,
        )
    }
    #[derive(Clone, Copy)]
    enum TestControl {
        Key(u64),
        Launch(&'static str),
        Activity(&'static str, Option<&'static str>),
        Text(&'static str),
        NoKey,
        Details(bool),
        Hold(bool, u64, bool),
        Voice(&'static [u8]),
        UnsupportedVoice,
        ReadyKey(bool),
    }
    fn server_control(
        on: bool,
        command: Option<TestControl>,
        model: &'static str,
        manufacturer: &'static str,
        confirm: bool,
        valid_identity: bool,
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
            pb::bytes(1, model.as_bytes(), &mut info);
            pb::bytes(2, manufacturer.as_bytes(), &mut info);
            let mut configure = Vec::new();
            let features = match command {
                Some(
                    TestControl::Launch(_) | TestControl::Activity(_, _) | TestControl::Text(_),
                ) => 35 | 512 | 4,
                Some(
                    TestControl::Voice(_) | TestControl::Details(_) | TestControl::ReadyKey(_),
                ) => 35 | 512 | 4 | 64 | 8,
                _ => 35,
            };
            let negotiated = if matches!(command, Some(TestControl::Voice(_))) {
                features
            } else {
                features & !8
            };
            pb::integer(1, features, &mut configure);
            pb::bytes(2, &info, &mut configure);
            // Pin mismatch aborts the handshake before the application can send data.
            if pb::write(&mut stream, 1, &configure).is_err() {
                return Ok(());
            }
            if !valid_identity {
                return Ok(());
            }
            let response = pb::read(&mut stream)?;
            assert_eq!(
                Message::new(&response)?
                    .nested(1)?
                    .expect("configure")
                    .int(1)?,
                Some(negotiated)
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
                Some(negotiated)
            );
            let mut start = Vec::new();
            if on {
                pb::integer(1, 1, &mut start);
            }
            pb::write(&mut stream, 40, &start)?;
            if features & 64 != 0 && !matches!(command, Some(TestControl::Details(_))) {
                let mut volume = Vec::new();
                pb::integer(6, 100, &mut volume);
                pb::integer(7, 27, &mut volume);
                pb::write(&mut stream, 50, &volume)?;
            }
            if let Some(TestControl::ReadyKey(complete)) = command {
                stream
                    .sock
                    .set_read_timeout(Some(Duration::from_millis(100)))?;
                assert!(
                    pb::read(&mut stream).is_err(),
                    "no key before initial IME report"
                );
                stream.sock.set_read_timeout(Some(Duration::from_secs(3)))?;
                if !complete {
                    assert!(
                        pb::read(&mut stream).is_err(),
                        "missing readiness must send no key"
                    );
                    return Ok(());
                }
            }
            if features & 4 != 0 && !matches!(command, Some(TestControl::Details(_))) {
                let mut app = Vec::new();
                pb::bytes(12, b"example.player", &mut app);
                let mut ime = Vec::new();
                pb::bytes(1, &app, &mut ime);
                pb::write(&mut stream, 20, &ime)?;
            }
            let command = if matches!(command, Some(TestControl::ReadyKey(_))) {
                Some(TestControl::Key(3))
            } else {
                command
            };
            if let Some(TestControl::Details(valid)) = command {
                let mut volume = Vec::new();
                pb::integer(6, 100, &mut volume);
                pb::integer(7, if valid { 27 } else { 101 }, &mut volume);
                pb::integer(8, 1, &mut volume);
                pb::write(&mut stream, 50, &volume)?;
                if valid {
                    let mut app = Vec::new();
                    pb::bytes(12, b"example.player", &mut app);
                    let mut ime = Vec::new();
                    pb::bytes(1, &app, &mut ime);
                    pb::write(&mut stream, 20, &ime)?;
                }
                return Ok(());
            }
            if let Some(TestControl::Hold(fail, ping_delay_ms, split)) = command {
                let key = pb::read(&mut stream)?;
                let key = Message::new(&key)?.nested(10)?.expect("press");
                assert_eq!(key.int(1)?, Some(23));
                assert_eq!(key.int(2)?, Some(1));
                let pressed_at = Instant::now();
                if fail {
                    pb::write(&mut stream, 3, &[])?;
                } else {
                    thread::sleep(Duration::from_millis(ping_delay_ms));
                    let mut ping = Vec::new();
                    pb::integer(1, 42, &mut ping);
                    if split {
                        use std::io::Write;
                        let mut wire = Vec::new();
                        pb::write(&mut wire, 8, &ping)?;
                        stream.write_all(&wire[..3])?;
                        stream.flush()?;
                        thread::sleep(Duration::from_millis(120));
                        stream.write_all(&wire[3..])?;
                        stream.flush()?;
                    } else {
                        pb::write(&mut stream, 8, &ping)?;
                    }
                }
                // A ping can cross the hold deadline: the one release may
                // precede its pong. Both orders are valid on the same session.
                let (mut pong, mut released) = (fail, false);
                for _ in 0..if fail { 1 } else { 2 } {
                    let frame = pb::read(&mut stream)?;
                    let message = Message::new(&frame)?;
                    if let Some(reply) = message.nested(9)? {
                        assert!(!pong, "duplicate pong");
                        assert_eq!(reply.int(1)?, Some(42));
                        pong = true;
                    } else {
                        let release = message.nested(10)?.expect("release");
                        assert!(!released, "duplicate release");
                        assert_eq!(release.int(1)?, Some(23));
                        assert_eq!(release.int(2)?, Some(2));
                        released = true;
                    }
                }
                assert!(pong && released);
                if !fail {
                    assert!(pressed_at.elapsed() >= Duration::from_millis(60));
                    let mut byte = [0];
                    assert_eq!(std::io::Read::read(&mut stream, &mut byte)?, 0);
                }
                return Ok(());
            }
            if let Some(TestControl::Voice(audio)) = command {
                let search = pb::read(&mut stream)?;
                let search = Message::new(&search)?.nested(10)?.expect("search");
                assert_eq!(search.int(1)?, Some(84));
                assert_eq!(search.int(2)?, Some(3));
                let mut begin = Vec::new();
                pb::integer(1, 27, &mut begin);
                pb::write(&mut stream, 30, &begin)?;
                let ack = pb::read(&mut stream)?;
                assert_eq!(
                    Message::new(&ack)?.nested(30)?.expect("voice ack").int(1)?,
                    Some(27)
                );
                for samples in audio.chunks(8192) {
                    use std::io::Read;
                    let mut size = 0;
                    for shift in (0..21).step_by(7) {
                        let mut byte = [0];
                        stream.read_exact(&mut byte)?;
                        size |= ((byte[0] & 127) as usize) << shift;
                        if byte[0] < 128 {
                            break;
                        }
                    }
                    assert!(size <= 8256);
                    let mut actual = vec![0; size];
                    stream.read_exact(&mut actual)?;
                    let mut padded = [0; 8192];
                    padded[..samples.len()].copy_from_slice(samples);
                    let mut payload = Vec::new();
                    pb::integer(1, 27, &mut payload);
                    pb::bytes(2, &padded, &mut payload);
                    let mut expected = Vec::new();
                    pb::bytes(31, &payload, &mut expected);
                    assert_eq!(actual, expected);
                }
                let end = pb::read(&mut stream)?;
                assert_eq!(
                    Message::new(&end)?.nested(32)?.expect("voice end").int(1)?,
                    Some(27)
                );
                let mut byte = [0];
                assert_eq!(std::io::Read::read(&mut stream, &mut byte)?, 0);
                return Ok(());
            }
            if matches!(command, Some(TestControl::UnsupportedVoice)) {
                assert!(pb::read(&mut stream).is_err());
                return Ok(());
            }
            if matches!(command, Some(TestControl::NoKey)) {
                let failure = pb::read(&mut stream).expect_err("no control may be sent");
                assert!(failure.downcast_ref::<std::io::Error>().is_some_and(|e| {
                    matches!(
                        e.kind(),
                        std::io::ErrorKind::UnexpectedEof | std::io::ErrorKind::ConnectionReset
                    )
                }));
                return Ok(());
            }
            if let Some(TestControl::Launch(link) | TestControl::Activity(link, _)) = command {
                let response = pb::read(&mut stream)?;
                assert_eq!(
                    Message::new(&response)?
                        .nested(90)?
                        .expect("app launch")
                        .text(1)?,
                    Some(link)
                );
            }
            if let Some(TestControl::Activity(_, Some(package))) = command {
                let mut app = Vec::new();
                pb::bytes(12, package.as_bytes(), &mut app);
                let mut ime = Vec::new();
                pb::bytes(1, &app, &mut ime);
                pb::write(&mut stream, 20, &ime)?;
            }
            if let Some(TestControl::Text(text)) = command {
                let mut ime = Vec::new();
                pb::integer(1, 7, &mut ime);
                pb::integer(2, 9, &mut ime);
                pb::write(&mut stream, 21, &ime)?;
                let response = pb::read(&mut stream)?;
                let message = Message::new(&response)?;
                let batch = message.nested(21)?.expect("text batch");
                assert_eq!(batch.int(1)?, Some(7));
                assert_eq!(batch.int(2)?, Some(9));
                assert_eq!(
                    batch
                        .nested(3)?
                        .expect("edit")
                        .nested(2)?
                        .expect("text")
                        .text(3)?,
                    Some(text)
                );
            }
            if matches!(
                command,
                Some(TestControl::Text(_) | TestControl::Launch(_) | TestControl::Activity(_, _))
            ) {
                let mut ping = Vec::new();
                pb::integer(1, 42, &mut ping);
                pb::write(&mut stream, 8, &ping)?;
                let pong = pb::read(&mut stream)?;
                assert_eq!(
                    Message::new(&pong)?
                        .nested(9)?
                        .expect("pong after input")
                        .int(1)?,
                    Some(42)
                );
                let mut byte = [0];
                assert_eq!(std::io::Read::read(&mut stream, &mut byte)?, 0);
                return Ok(());
            }
            if let Some(TestControl::Key(command)) = command {
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
                if !matches!(command, 26 | 223 | 224) {
                    let mut app = Vec::new();
                    pb::bytes(12, b"example.launcher", &mut app);
                    let mut ime = Vec::new();
                    pb::bytes(1, &app, &mut ime);
                    pb::write(&mut stream, 20, &ime)?;
                    // A short-lived client must service pending messages rather
                    // than resetting a socket with an unread ping after its key.
                    let mut byte = [0];
                    assert_eq!(std::io::Read::read(&mut stream, &mut byte)?, 0);
                    return Ok(());
                }
                let mut start = Vec::new();
                if command == 224 || (command == 26 && !on) {
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
    fn tcl_config(temp: &support::Directory, address: SocketAddr, pin: String) -> Result<PathBuf> {
        let (cert, key) = temp.certs()?;
        temp.config("tcl.json", &json!({"host":"192.0.2.40", "model":"Smart TV", "connect":address,
            "server_cert_sha256":pin,"client_cert":cert,"client_key":key,"timeout_seconds":2,"verify_seconds":2}))
    }
    #[test]
    fn tcl_power_uses_guarded_power_off_and_absolute_wake() -> Result<()> {
        use crate::{device::Devices, entity};
        for (on, command) in [(true, None), (true, Some(26)), (false, Some(224))] {
            let temp = support::Directory::new()?;
            let (address, worker, pin) = server_as(on, command, "Smart TV", "TCL", true, true)?;
            let path = tcl_config(&temp, address, pin)?;
            let mut devices = Devices {
                tcl: Some(TclManager::open(&path)?),
                ..Devices::default()
            };
            devices.validate()?;
            let list = entity::list(&devices).to_string();
            assert!(list.contains("media_player.tcl"));
            assert!(!list.contains("server_cert_sha256"));
            assert!(devices.call("digi_apply", &json!({"power":false})).is_err());
            let power = command.map(|c| c == 224).unwrap_or(on);
            let result = entity::call(
                &mut devices,
                "media_power",
                &json!({"entity":"media_player.tcl","power":power}),
            )?;
            assert_eq!(result["result"]["verified"], true);
            assert_eq!(result["result"]["applied"], command.is_some());
            assert_eq!(result["result"]["status"]["model"], "Smart TV");
            assert_eq!(result["result"]["status"]["manufacturer"], "TCL");
            worker.join().expect("server")?;
        }
        Ok(())
    }
    #[test]
    fn tcl_rejects_wrong_vendor_model_pin_and_uncertain_shutdown() -> Result<()> {
        for (model, vendor, wrong_pin) in [
            ("wrong", "TCL", false),
            ("Smart TV", "wrong", false),
            ("Smart TV", "TCL", true),
        ] {
            let temp = support::Directory::new()?;
            let (address, worker, pin) = server_as(true, None, model, vendor, true, false)?;
            let mut manager = TclManager::open(&tcl_config(
                &temp,
                address,
                if wrong_pin { "00".repeat(32) } else { pin },
            )?)?;
            assert!(manager.apply(false).is_err());
            worker.join().expect("server")?;
        }
        let temp = support::Directory::new()?;
        let (address, worker, pin) = server_as(true, Some(26), "Smart TV", "TCL", false, true)?;
        let mut manager = TclManager::open(&tcl_config(&temp, address, pin)?)?;
        let result = manager.apply(false)?;
        assert_eq!(result["verified"], false);
        assert_eq!(result["command_sent"], true);
        assert_eq!(result["phase"], "verify");
        worker.join().expect("server")?;
        Ok(())
    }
    #[test]
    fn tcl_uncertain_shutdown_is_blocked_after_restart_until_confirmed_off() -> Result<()> {
        let temp = support::Directory::new()?;
        let (address, worker, pin) = server_as(true, Some(26), "Smart TV", "TCL", false, true)?;
        let path = tcl_config(&temp, address, pin)?;
        let mut manager = TclManager::open(&path)?;
        let result = manager.apply(false)?;
        assert_eq!(result["verified"], false);
        assert_eq!(result["command_sent"], true);
        worker.join().expect("server")?;
        drop(manager);
        let memory: ShutdownMemory =
            serde_json::from_slice(&std::fs::read(path.with_extension("power.state.json"))?)?;
        assert!(memory.attempt.is_some());
        for on in [true, false] {
            let (address, worker, pin) =
                server_control(on, Some(TestControl::NoKey), "Smart TV", "TCL", true, true)?;
            tcl_config(&temp, address, pin)?;
            let mut manager = TclManager::open(&path)?;
            if on {
                assert!(
                    manager
                        .apply(false)
                        .expect_err("repeat toggle")
                        .to_string()
                        .contains("Recent TCL shutdown")
                );
            } else {
                let result = manager.apply(false)?;
                assert_eq!(result["applied"], false);
                assert_eq!(result["verified"], true);
            }
            worker.join().expect("server")?;
        }
        let memory: ShutdownMemory =
            serde_json::from_slice(&std::fs::read(path.with_extension("power.state.json"))?)?;
        assert!(memory.attempt.is_none());
        Ok(())
    }
    #[test]
    fn tcl_power_key_wake_is_state_checked_and_guarded_after_restart() -> Result<()> {
        for confirm in [true, false] {
            let temp = support::Directory::new()?;
            let (address, worker, pin) =
                server_as(false, Some(26), "Smart TV", "TCL", confirm, true)?;
            let path = tcl_config(&temp, address, pin)?;
            let mut value: Value = serde_json::from_slice(&std::fs::read(&path)?)?;
            value["power_on_key"] = json!("power");
            temp.config("tcl.json", &value)?;
            let mut manager = TclManager::open(&path)?;
            let result = manager.apply(true)?;
            assert_eq!(result["verified"], confirm);
            assert_eq!(result["command_sent"], true);
            worker.join().expect("server")?;
            drop(manager);
            if !confirm {
                let (address, worker, pin) = server_control(
                    false,
                    Some(TestControl::NoKey),
                    "Smart TV",
                    "TCL",
                    true,
                    true,
                )?;
                value["connect"] = json!(address);
                value["server_cert_sha256"] = json!(pin);
                temp.config("tcl.json", &value)?;
                let mut manager = TclManager::open(&path)?;
                assert!(
                    manager
                        .apply(true)
                        .expect_err("uncertain wake cannot toggle again")
                        .to_string()
                        .contains("Recent TCL")
                );
                worker.join().expect("server")?;
            }
        }
        Ok(())
    }
    #[test]
    fn tcl_network_recovery_sends_one_packet_and_preserves_unknown_attempt() -> Result<()> {
        use std::net::UdpSocket;
        let temp = support::Directory::new()?;
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let address = listener.local_addr()?;
        drop(listener);
        let udp = UdpSocket::bind("127.0.0.1:0")?;
        udp.set_read_timeout(Some(Duration::from_millis(200)))?;
        let path = tcl_config(&temp, address, "00".repeat(32))?;
        let mut value: Value = serde_json::from_slice(&std::fs::read(&path)?)?;
        value["wake"] =
            json!({"mac":"02:00:00:00:00:40","wake_address":udp.local_addr()?,"ready_seconds":1});
        temp.config("tcl.json", &value)?;
        let mut manager = TclManager::open(&path)?;
        let result = manager.apply(true)?;
        assert_eq!(result["verified"], false);
        assert_eq!(result["phase"], "network_recovery");
        assert_eq!(result["command_sent"], false);
        assert_eq!(result["wake_packet_sent"], true);
        let mut bytes = [0; 128];
        let len = udp.recv(&mut bytes)?;
        assert_eq!(len, 102);
        assert_eq!(&bytes[..6], &[0xff; 6]);
        for block in bytes[6..len].as_chunks::<6>().0 {
            assert_eq!(block, &[2, 0, 0, 0, 0, 64]);
        }
        drop(manager);
        let mut manager = TclManager::open(&path)?;
        assert!(manager.apply(true).is_err());
        assert!(udp.recv(&mut bytes).is_err(), "no replay after restart");
        let off = manager.apply(false)?;
        assert_eq!(off["outcome"], "not_sent");
        assert_eq!(off["command_sent"], false);
        let memory: ShutdownMemory =
            serde_json::from_slice(&std::fs::read(path.with_extension("power.state.json"))?)?;
        assert_eq!(memory.requested_power, Some(true));
        Ok(())
    }
    #[test]
    fn tcl_network_wake_never_follows_identity_failure_or_uses_invalid_endpoints() -> Result<()> {
        use std::net::UdpSocket;
        let temp = support::Directory::new()?;
        let udp = UdpSocket::bind("127.0.0.1:0")?;
        udp.set_read_timeout(Some(Duration::from_millis(100)))?;
        let (address, worker, _) = server_as(false, None, "Smart TV", "TCL", true, false)?;
        let path = tcl_config(&temp, address, "00".repeat(32))?;
        let mut value: Value = serde_json::from_slice(&std::fs::read(&path)?)?;
        value["wake"] = json!({"mac":"02:00:00:00:00:40","wake_address":udp.local_addr()?});
        temp.config("tcl.json", &value)?;
        let mut manager = TclManager::open(&path)?;
        assert!(manager.apply(true).is_err());
        let mut bytes = [0; 128];
        assert!(udp.recv(&mut bytes).is_err());
        worker.join().expect("server")?;
        drop(manager);
        for bad in [
            json!({"mac":"ff:ff:ff:ff:ff:ff","wake_address":"192.0.2.255:9"}),
            json!({"mac":"02:00:00:00:00:40","wake_address":"0.0.0.0:9"}),
            json!({"mac":"02:00:00:00:00:40","wake_address":"192.0.2.255:9","ready_seconds":121}),
        ] {
            value["wake"] = bad;
            temp.config("tcl.json", &value)?;
            assert!(TclManager::open(&path).is_err());
        }
        Ok(())
    }
    #[test]
    fn tcl_cast_activation_requires_independent_pinned_power_confirmation() -> Result<()> {
        for reported_on in [true, false] {
            let temp = support::Directory::new()?;
            let (address, worker, pin) = server_control(
                reported_on,
                Some(TestControl::NoKey),
                "Smart TV",
                "TCL",
                true,
                true,
            )?;
            let (cast_address, cast_worker, cast_pin) =
                crate::cast::tests::server(Some(json!({"action":"wake","standby":true})), false)?;
            let path = tcl_config(&temp, address, pin)?;
            let mut value: Value = serde_json::from_slice(&std::fs::read(&path)?)?;
            value["host"] = json!("127.0.0.1");
            value["cast_wake"] = json!({"server_cert_sha256":cast_pin,
                "port":cast_address.port(),"timeout_seconds":2});
            value["wake"] = json!({"mac":"02:00:00:00:00:40",
                "wake_address":"127.0.0.1:9","ready_seconds":1});
            temp.config("tcl.json", &value)?;
            let mut manager = TclManager::open(&path)?;
            let result = power::cast_wake(&mut manager, power::Outcome::new(true))?;
            assert_eq!(result["verified"], reported_on);
            assert_eq!(result["cast_command_sent"], true);
            assert_eq!(result["command_sent"], false);
            assert_eq!(result["wake_packet_sent"], false);
            worker.join().expect("remote server")?;
            cast_worker.join().expect("Cast server")?;
            let memory: ShutdownMemory =
                serde_json::from_slice(&std::fs::read(path.with_extension("power.state.json"))?)?;
            assert_eq!(memory.attempt.is_none(), reported_on);
        }
        Ok(())
    }
    #[test]
    fn tcl_cast_retries_precontrol_connections_until_the_service_returns() -> Result<()> {
        let temp = support::Directory::new()?;
        let (address, remote_worker, pin) = server_control(
            true,
            Some(TestControl::NoKey),
            "Smart TV",
            "TCL",
            true,
            true,
        )?;
        let reserve = TcpListener::bind("127.0.0.1:0")?;
        let cast_address = reserve.local_addr()?;
        drop(reserve);
        // A refused connection is a transient service outage, not permission to
        // repeat a device-changing command. Bring the fake service up later.
        let worker = thread::spawn(move || -> Result<()> {
            thread::sleep(Duration::from_millis(200));
            let (_, worker, _) = crate::cast::tests::server_at(
                Some(json!({"action":"wake","standby":true})),
                false,
                cast_address,
            )?;
            worker.join().expect("Cast server")
        });
        let path = tcl_config(&temp, address, pin.clone())?;
        let mut value: Value = serde_json::from_slice(&std::fs::read(&path)?)?;
        value["host"] = json!("127.0.0.1");
        value["cast_wake"] = json!({"server_cert_sha256":pin,
            "port":cast_address.port(),"timeout_seconds":2});
        temp.config("tcl.json", &value)?;
        let result = power::cast_wake(&mut TclManager::open(&path)?, power::Outcome::new(true))?;
        assert_eq!(result["verified"], true);
        assert_eq!(result["cast_command_sent"], true);
        assert_eq!(result["wake_packet_sent"], false);
        assert_eq!(result["command_sent"], false);
        worker.join().expect("delayed Cast service")?;
        remote_worker.join().expect("remote server")?;
        Ok(())
    }
    #[test]
    fn tcl_cast_outage_sends_one_network_wake_and_skips_launch_if_already_on() -> Result<()> {
        use std::net::UdpSocket;
        let temp = support::Directory::new()?;
        let (address, worker, pin) = server_control(
            true,
            Some(TestControl::NoKey),
            "Smart TV",
            "TCL",
            true,
            true,
        )?;
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let cast_address = listener.local_addr()?;
        drop(listener);
        let udp = UdpSocket::bind("127.0.0.1:0")?;
        udp.set_read_timeout(Some(Duration::from_millis(100)))?;
        let path = tcl_config(&temp, address, pin.clone())?;
        let mut value: Value = serde_json::from_slice(&std::fs::read(&path)?)?;
        value["host"] = json!("127.0.0.1");
        value["cast_wake"] = json!({"server_cert_sha256":pin,
            "port":cast_address.port(),"timeout_seconds":2});
        value["wake"] = json!({"mac":"02:00:00:00:00:40",
            "wake_address":udp.local_addr()?,"ready_seconds":2});
        temp.config("tcl.json", &value)?;
        let result = power::cast_wake(&mut TclManager::open(&path)?, power::Outcome::new(true))?;
        assert_eq!(result["verified"], true);
        assert_eq!(result["wake_packet_sent"], true);
        assert_eq!(result["cast_command_sent"], false);
        assert_eq!(result["command_sent"], false);
        let mut bytes = [0; 128];
        assert_eq!(udp.recv(&mut bytes)?, 102);
        assert!(
            udp.recv(&mut bytes).is_err(),
            "network wake must not be replayed"
        );
        worker.join().expect("remote server")?;
        Ok(())
    }
    #[test]
    fn tcl_cast_lost_launch_reply_is_verified_without_replay_or_network_fallback() -> Result<()> {
        use std::net::UdpSocket;
        let temp = support::Directory::new()?;
        let (address, remote_worker, pin) = server_control(
            true,
            Some(TestControl::NoKey),
            "Smart TV",
            "TCL",
            true,
            true,
        )?;
        let (cast_address, cast_worker, cast_pin) = crate::cast::tests::server(
            Some(json!({"action":"wake","standby":true,"disconnect_launch":true})),
            false,
        )?;
        let udp = UdpSocket::bind("127.0.0.1:0")?;
        udp.set_read_timeout(Some(Duration::from_millis(100)))?;
        let path = tcl_config(&temp, address, pin)?;
        let mut value: Value = serde_json::from_slice(&std::fs::read(&path)?)?;
        value["host"] = json!("127.0.0.1");
        value["cast_wake"] = json!({"server_cert_sha256":cast_pin,
            "port":cast_address.port(),"timeout_seconds":2});
        value["wake"] = json!({"mac":"02:00:00:00:00:40",
            "wake_address":udp.local_addr()?,"ready_seconds":2});
        temp.config("tcl.json", &value)?;
        let result = power::cast_wake(&mut TclManager::open(&path)?, power::Outcome::new(true))?;
        assert_eq!(result["verified"], true);
        assert_eq!(result["outcome"], "confirmed");
        assert!(result["cast_command_sent"].is_null());
        assert_eq!(result["wake_packet_sent"], false);
        assert_eq!(result["command_sent"], false);
        let mut bytes = [0; 128];
        assert!(udp.recv(&mut bytes).is_err());
        cast_worker.join().expect("Cast server")?;
        remote_worker.join().expect("remote server")?;
        Ok(())
    }
    #[test]
    fn tcl_cast_identity_protocol_and_uncertain_launch_errors_never_trigger_network_fallback()
    -> Result<()> {
        use std::net::UdpSocket;
        for mode in ["pin", "standby", "launch"] {
            let temp = support::Directory::new()?;
            let mut expected = json!({"action":"wake","standby":true});
            if mode == "standby" {
                expected["omit_standby"] = json!(true);
            }
            if mode == "launch" {
                expected["disconnect_launch"] = json!(true);
            }
            let (address, worker, pin) = crate::cast::tests::server(Some(expected), false)?;
            let udp = UdpSocket::bind("127.0.0.1:0")?;
            udp.set_read_timeout(Some(Duration::from_millis(100)))?;
            let path = tcl_config(&temp, address, pin.clone())?;
            let mut value: Value = serde_json::from_slice(&std::fs::read(&path)?)?;
            value["host"] = json!("127.0.0.1");
            value["cast_wake"] = json!({"server_cert_sha256":if mode=="pin" {"00".repeat(32)} else {pin},
                "port":address.port(),"timeout_seconds":2});
            value["wake"] = json!({"mac":"02:00:00:00:00:40",
                "wake_address":udp.local_addr()?,"ready_seconds":1});
            temp.config("tcl.json", &value)?;
            let mut manager = TclManager::open(&path)?;
            let result = power::cast_wake(&mut manager, power::Outcome::new(true))?;
            assert_eq!(result["verified"], false);
            assert_eq!(result["wake_packet_sent"], false);
            assert_eq!(result["command_sent"], false);
            let mut bytes = [0; 128];
            assert!(
                udp.recv(&mut bytes).is_err(),
                "no network fallback after {mode}"
            );
            let state: ShutdownMemory =
                serde_json::from_slice(&std::fs::read(path.with_extension("power.state.json"))?)?;
            assert_eq!(state.attempt.is_some(), mode == "launch");
            if mode == "launch" {
                assert!(result["cast_command_sent"].is_null());
                assert!(power::cast_wake(&mut manager, power::Outcome::new(true)).is_err());
            } else {
                assert_eq!(result["cast_command_sent"], false);
            }
            let served = worker.join().expect("Cast server");
            if mode == "pin" {
                assert!(served.is_err());
            } else {
                served?;
            }
        }
        Ok(())
    }
    #[test]
    fn tcl_shutdown_persistence_failure_sends_no_key() -> Result<()> {
        let temp = support::Directory::new()?;
        let (address, worker, pin) = server_control(
            true,
            Some(TestControl::NoKey),
            "Smart TV",
            "TCL",
            true,
            true,
        )?;
        let path = tcl_config(&temp, address, pin)?;
        let mut manager = TclManager::open(&path)?;
        std::fs::create_dir(path.with_extension("power.state.json"))?;
        assert!(manager.apply(false).is_err());
        worker.join().expect("server")?;
        Ok(())
    }
    #[test]
    fn tcl_navigation_services_pending_ping_before_closing() -> Result<()> {
        use crate::{device::Devices, entity};
        for (key, code) in [("home", 3), ("back", 4)] {
            let temp = support::Directory::new()?;
            let (address, worker, pin) =
                server_as(true, Some(code), "Smart TV", "TCL", true, true)?;
            let mut devices = Devices {
                tcl: Some(TclManager::open(&tcl_config(&temp, address, pin)?)?),
                ..Devices::default()
            };
            let result = entity::call(
                &mut devices,
                "remote_key",
                &json!({"entity":"media_player.tcl","key":key}),
            )?;
            assert_eq!(result["result"]["sent"], true);
            assert_eq!(result["result"]["verified"], false);
            assert_eq!(result["result"]["current_app"], "example.launcher");
            worker.join().expect("server")?;
        }
        Ok(())
    }
    #[test]
    fn disconnected_navigation_is_uncertain_and_not_replayed() -> Result<()> {
        let temp = support::Directory::new()?;
        let (address, worker, pin) = server_as(true, Some(3), "Smart TV", "TCL", false, true)?;
        let mut manager = TclManager::open(&tcl_config(&temp, address, pin)?)?;
        let failure = manager.key("home").expect_err("lost session is uncertain");
        assert!(failure.to_string().contains("outcome unknown"));
        worker.join().expect("server")?;
        Ok(())
    }
    #[test]
    fn app_links_and_text_use_narrow_tools_over_pinned_tls() -> Result<()> {
        use crate::{device::Devices, entity};
        for (tool, args, expected) in [
            (
                "media_launch",
                json!({"entity":"media_player.tcl","url":"https://www.netflix.com/title/12345"}),
                TestControl::Launch("https://www.netflix.com/title/12345"),
            ),
            (
                "media_launch",
                json!({"entity":"media_player.tcl","url":"stremio:///detail/movie/tt0066921/tt0066921?autoPlay=true"}),
                TestControl::Launch("stremio:///detail/movie/tt0066921/tt0066921?autoPlay=true"),
            ),
            (
                "media_text",
                json!({"entity":"media_player.tcl","text":"A movie title"}),
                TestControl::Text("A movie title"),
            ),
        ] {
            let temp = support::Directory::new()?;
            let (address, worker, pin) =
                server_control(true, Some(expected), "Smart TV", "TCL", true, true)?;
            let mut devices = Devices {
                tcl: Some(TclManager::open(&tcl_config(&temp, address, pin)?)?),
                ..Devices::default()
            };
            let result = entity::call(&mut devices, tool, &args)?;
            assert_eq!(result["result"]["sent"], true);
            assert_eq!(result["result"]["verified"], false);
            worker.join().expect("server")?;
        }
        Ok(())
    }
    #[test]
    fn activity_observes_only_fresh_app_reports_and_remains_unverified() -> Result<()> {
        use crate::{device::Devices, entity};
        for (package, confirmed) in [
            (Some("com.netflix.ninja"), json!(true)),
            (Some("example.other"), json!(false)),
            (None, Value::Null),
        ] {
            let temp = support::Directory::new()?;
            let (address, worker, pin) = server_control(
                true,
                Some(TestControl::Activity(
                    "https://www.netflix.com/title/12345?source=30",
                    package,
                )),
                "Smart TV",
                "TCL",
                true,
                true,
            )?;
            let mut devices = Devices {
                tcl: Some(TclManager::open(&tcl_config(&temp, address, pin)?)?),
                ..Devices::default()
            };
            let result = entity::call(
                &mut devices,
                "media_activity",
                &json!({"entity":"media_player.tcl","activity":{"name":"netflix","content_id":"12345"}}),
            )?;
            assert_eq!(result["result"]["app_confirmed"], confirmed);
            assert_eq!(result["result"]["current_app"], json!(package));
            assert_eq!(result["result"]["verified"], false);
            worker.join().expect("server")?;
        }
        Ok(())
    }
    #[test]
    fn live_volume_and_app_are_collected_after_power_without_inference() -> Result<()> {
        for valid in [true, false] {
            let temp = support::Directory::new()?;
            let (address, worker, pin) = server_control(
                true,
                Some(TestControl::Details(valid)),
                "Smart TV",
                "TCL",
                true,
                true,
            )?;
            let manager = TclManager::open(&tcl_config(&temp, address, pin)?)?;
            let result = manager.status();
            if valid {
                let status = result?;
                assert!(status.power);
                assert_eq!(status.current_app.as_deref(), Some("example.player"));
                let volume = status.volume.expect("fresh volume");
                assert_eq!((volume.level, volume.max, volume.muted), (27, 100, true));
            } else {
                assert!(result.is_err());
            }
            worker.join().expect("server")?;
        }
        Ok(())
    }
    #[test]
    fn held_key_services_ping_and_releases_once_even_after_protocol_error() -> Result<()> {
        use crate::{device::Devices, entity};
        for (fail, ping_delay_ms, split) in [
            (false, 0, false),
            (false, 120, false),
            (false, 0, true),
            (true, 0, false),
        ] {
            let temp = support::Directory::new()?;
            let (address, worker, pin) = server_control(
                true,
                Some(TestControl::Hold(fail, ping_delay_ms, split)),
                "Smart TV",
                "TCL",
                true,
                true,
            )?;
            let mut devices = Devices {
                tcl: Some(TclManager::open(&tcl_config(&temp, address, pin)?)?),
                ..Devices::default()
            };
            let result = entity::call(
                &mut devices,
                "remote_hold",
                &json!({"entity":"media_player.tcl","key":"enter","hold_ms":80}),
            );
            if fail {
                assert!(
                    result
                        .expect_err("uncertain hold")
                        .to_string()
                        .contains("do not replay")
                );
            } else {
                assert_eq!(result?["result"]["verified"], false);
            }
            worker.join().expect("server")?;
        }
        Ok(())
    }
    #[test]
    fn voice_negotiates_session_streams_bounded_pcm_and_ends_once() -> Result<()> {
        use crate::{device::Devices, entity};
        use base64::{Engine, engine::general_purpose::STANDARD};
        static AUDIO: [u8; 16_386] = [42; 16_386];
        for supported in [true, false] {
            let temp = support::Directory::new()?;
            let command = if supported {
                TestControl::Voice(&AUDIO)
            } else {
                TestControl::UnsupportedVoice
            };
            let (address, worker, pin) =
                server_control(true, Some(command), "Smart TV", "TCL", true, true)?;
            let mut devices = Devices {
                tcl: Some(TclManager::open(&tcl_config(&temp, address, pin)?)?),
                ..Devices::default()
            };
            let result = entity::call(
                &mut devices,
                "media_voice",
                &json!({"entity":"media_player.tcl","audio":STANDARD.encode(AUDIO)}),
            );
            if supported {
                assert_eq!(result?["result"]["verified"], false);
            } else {
                assert!(
                    result
                        .expect_err("unsupported voice")
                        .to_string()
                        .contains("does not support voice")
                );
            }
            worker.join().expect("server")?;
        }
        Ok(())
    }
    #[test]
    fn extended_input_keeps_mcp_grants_bounds_and_power_guards() -> Result<()> {
        use crate::{device::Devices, entity, mcp};
        use base64::{Engine, engine::general_purpose::STANDARD};
        let temp = support::Directory::new()?;
        let mut devices = Devices {
            tcl: Some(TclManager::open(&tcl_config(
                &temp,
                "127.0.0.1:1".parse()?,
                "00".repeat(32),
            )?)?),
            ..Devices::default()
        };
        for (tool, args) in [
            (
                "remote_hold",
                json!({"entity":"media_player.tcl","key":"enter","hold_ms":80}),
            ),
            (
                "media_voice",
                json!({"entity":"media_player.tcl","audio":STANDARD.encode([0,0])}),
            ),
            (
                "media_activity",
                json!({"entity":"media_player.tcl","activity":{"name":"home"}}),
            ),
        ] {
            let response=mcp::handle_entities(json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":tool,"arguments":args}}),&mut devices,false).expect("reply");
            assert!(response["error"].is_object() || response["result"]["isError"] == true);
        }
        for args in [
            json!({"entity":"media_player.tcl","key":"power","hold_ms":80}),
            json!({"entity":"media_player.tcl","key":"enter","hold_ms":0}),
            json!({"entity":"media_player.tcl","key":"enter","hold_ms":2001}),
            json!({"entity":"media_player.tcl","key":"enter","hold_ms":80,"extra":true}),
        ] {
            assert!(entity::call(&mut devices, "remote_hold", &args).is_err());
        }
        for audio in [
            json!(""),
            json!("bad!"),
            json!(STANDARD.encode([0])),
            json!(STANDARD.encode([0; 48_002])),
            json!(STANDARD.encode(b"RIFF0000")),
        ] {
            assert!(
                entity::call(
                    &mut devices,
                    "media_voice",
                    &json!({"entity":"media_player.tcl","audio":audio})
                )
                .is_err()
            );
        }
        assert!(crate::media_input::valid_voice(&json!(
            STANDARD.encode([0; 48_000])
        )));
        assert!(key_code("power").is_none());
        for key in [
            "hdmi_1",
            "hdmi_4",
            "stop",
            "next",
            "settings",
            "captions",
            "audio_track",
            "0",
            "9",
        ] {
            assert!(key_code(key).is_some());
        }
        Ok(())
    }
    #[test]
    fn tcl_waits_for_initial_input_reports_and_fails_before_control_if_missing() -> Result<()> {
        for complete in [true, false] {
            let temp = support::Directory::new()?;
            let (address, worker, pin) = server_control(
                true,
                Some(TestControl::ReadyKey(complete)),
                "Smart TV",
                "TCL",
                true,
                true,
            )?;
            let mut manager = TclManager::open(&tcl_config(&temp, address, pin)?)?;
            let result = manager.key("home");
            if complete {
                assert_eq!(result?["sent"], true);
            } else {
                assert!(
                    result
                        .expect_err("readiness missing")
                        .to_string()
                        .contains("no control sent")
                );
            }
            worker.join().expect("server")?;
        }
        Ok(())
    }
    #[test]
    fn app_links_reject_installation_and_non_navigation_schemes() {
        for link in [
            "netflix://",
            "stremio:///board",
            "stremio:///search?search=A%20Movie",
            "stremio:///detail/series/tt0108778/tt0108778:1:1",
            "https://www.netflix.com/title/12345",
        ] {
            assert!(validate_app_link(link).is_ok(), "{link}");
        }
        for link in [
            "intent://app/#Intent;end",
            "file:///data/private",
            "netflix://arbitrary-action",
            "stremio://addon.example/manifest.json",
            "stremio:///addons",
            "stremio:///search?search=x&install=1",
            "stremio:///detail/movie/id/video?autoPlay=true&install=1",
        ] {
            assert!(validate_app_link(link).is_err(), "{link}");
        }
    }
    #[test]
    fn app_and_text_require_live_support() -> Result<()> {
        use crate::{device::Devices, entity};
        for (tool, args) in [
            (
                "media_launch",
                json!({"entity":"media_player.tcl","url":"https://www.netflix.com"}),
            ),
            (
                "media_text",
                json!({"entity":"media_player.tcl","text":"movie"}),
            ),
        ] {
            let temp = support::Directory::new()?;
            let (address, worker, pin) = server_as(true, None, "Smart TV", "TCL", true, true)?;
            let mut devices = Devices {
                tcl: Some(TclManager::open(&tcl_config(&temp, address, pin)?)?),
                ..Devices::default()
            };
            assert!(
                entity::call(&mut devices, tool, &args)
                    .expect_err("unsupported feature")
                    .to_string()
                    .contains("does not support")
            );
            worker.join().expect("server")?;
        }
        Ok(())
    }
    #[test]
    fn backlight_discovery_is_opt_in_and_mcp_write_is_guarded() -> Result<()> {
        use crate::{device::Devices, entity, mcp};
        let temp = support::Directory::new()?;
        let path = tcl_config(&temp, "127.0.0.1:1".parse()?, "00".repeat(32))?;
        let mut config: Value = serde_json::from_slice(&std::fs::read(&path)?)?;
        for enabled in [false, true] {
            if enabled {
                config["companion"] = json!({"server_cert_sha256":"00".repeat(32),
                    "token":"ab".repeat(32),"fingerprint":"fixture-build","incremental":"fixture"});
                temp.config("tcl.json", &config)?;
            }
            let mut devices = Devices {
                tcl: Some(TclManager::open(&path)?),
                ..Devices::default()
            };
            let listing =
                entity::call(&mut devices, "entity_list", &json!({"details":true}))?.to_string();
            assert_eq!(listing.contains("backlight_level"), enabled);
            let reply = mcp::handle_entities(
                json!({"jsonrpc":"2.0","id":1,"method":"tools/call",
                "params":{"name":"entity_configure","arguments":{"entity":"media_player.tcl",
                    "settings":{"backlight_level":50}}}}),
                &mut devices,
                false,
            )
            .expect("reply");
            assert_eq!(reply["result"]["isError"], true);
            assert!(reply.to_string().contains("Device control is disabled"));
            for invalid in [json!(101), json!(-1), json!(0.5), json!("50")] {
                assert!(
                    entity::call(
                        &mut devices,
                        "entity_configure",
                        &json!({"entity":"media_player.tcl","settings":{"backlight_level":invalid}})
                    )
                    .is_err()
                );
            }
        }
        Ok(())
    }
    #[test]
    fn tcl_configuration_and_mcp_controls_are_guarded_without_io() -> Result<()> {
        use crate::{device::Devices, mcp};
        let temp = support::Directory::new()?;
        let path = tcl_config(&temp, "127.0.0.1:1".parse()?, "00".repeat(32))?;
        let original: Value = serde_json::from_slice(&std::fs::read(&path)?)?;
        for model in [
            Value::Null,
            json!(""),
            json!(" "),
            json!("x".repeat(129)),
            json!("bad\nmodel"),
        ] {
            let mut config = original.clone();
            config["model"] = model;
            temp.config("tcl.json", &config)?;
            assert!(TclManager::open(&path).is_err());
        }
        temp.config("tcl.json", &original)?;
        assert!(Manager::open(&path).is_err()); // DIGI identity cannot be overridden.
        let mut devices = Devices {
            tcl: Some(TclManager::open(&path)?),
            ..Devices::default()
        };
        assert!(TclManager::open(&path).is_err());
        let list = mcp::handle_entities(
            json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}),
            &mut devices,
            false,
        )
        .expect("reply");
        assert!(!list.to_string().contains("media_power"));
        for (tool, args) in [
            (
                "media_power",
                json!({"entity":"media_player.tcl","power":false}),
            ),
            (
                "remote_key",
                json!({"entity":"media_player.tcl","key":"home"}),
            ),
            (
                "entity_configure",
                json!({"entity":"media_player.tcl","settings":{"text":"hello"}}),
            ),
            (
                "media_launch",
                json!({"entity":"media_player.tcl","url":"https://www.netflix.com"}),
            ),
            (
                "media_text",
                json!({"entity":"media_player.tcl","text":"hello"}),
            ),
        ] {
            let reply = mcp::handle_entities(json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":tool,"arguments":args}}), &mut devices, false).expect("reply");
            assert_eq!(reply["result"]["isError"], true);
            assert!(reply.to_string().contains("Device control is disabled"));
        }
        assert!(
            crate::entity::call(
                &mut devices,
                "remote_key",
                &json!({"entity":"media_player.tcl","key":"power"})
            )
            .is_err()
        );
        for (tool, args) in [
            (
                "media_launch",
                json!({"entity":"media_player.tcl","url":"file:///data/private"}),
            ),
            (
                "media_launch",
                json!({"entity":"media_player.tcl","url":"https://www.netflix.com","settings":{"auto_off":true}}),
            ),
            ("media_text", json!({"entity":"media_player.tcl","text":""})),
            (
                "media_text",
                json!({"entity":"media_player.tcl","text":"x".repeat(513)}),
            ),
            ("media_text", json!({"entity":"switch.tv","text":"movie"})),
        ] {
            assert!(crate::entity::call(&mut devices, tool, &args).is_err());
        }
        Ok(())
    }
}
