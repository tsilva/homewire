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
    fs::File,
    net::{IpAddr, SocketAddr},
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
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
    pub connect: Option<SocketAddr>,
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
}
struct Remote<'a> {
    expected_model: &'a str,
    expected_manufacturer: &'static str,
    stream: StreamOwned<ClientConnection, Timed>,
    status: Option<Status>,
    features: u64,
    configured: bool,
    active: bool,
    ime: Option<(u64, u64)>,
    current_app: Option<String>,
}
impl Config {
    fn validate<const TCL: bool>(&self) -> Result<()> {
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
            expected_model: self.model.as_deref().unwrap_or("DIGI R2A"),
            expected_manufacturer: if TCL { "TCL" } else { "ZTE" },
            stream: StreamOwned::new(connection, socket),
            status: None,
            features: 0,
            configured: false,
            active: false,
            ime: None,
            current_app: None,
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
        let bytes = pb::read(&mut self.stream)?;
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
            self.features = features & (35 | 4 | 512);
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
        if let Some(ime) = message.nested(20)?
            && let Some(info) = ime.nested(1)?
        {
            self.current_app = info.text(12)?.filter(|s| s.len() <= 256).map(str::to_owned);
        }
        Ok(())
    }
    fn key(&mut self, code: u64) -> Result<()> {
        let mut body = Vec::new();
        pb::integer(1, code, &mut body);
        pb::integer(2, 3, &mut body);
        pb::write(&mut self.stream, 10, &body)
    }
    fn finish_key(&mut self, operation_deadline: Instant) -> Result<()> {
        // Closing immediately can reset TCP with unread TV messages and discard
        // the key. Keep servicing the session briefly, then close TLS cleanly.
        // Neither pings nor this wait acknowledge execution of the key.
        let deadline = operation_deadline.min(Instant::now() + Duration::from_millis(500));
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
                    "Android TV key was sent but the session failed; outcome unknown; do not replay",
                ));
            }
        }
        if !drained {
            return Err(error(
                "Too many Android remote messages after key; outcome unknown; do not replay",
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
            },
            Err(e) => return Err(e.into()),
        };
        Ok(Self { path, memory })
    }
    fn record_attempt(&mut self) -> Result<()> {
        let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
        if self
            .memory
            .attempt
            .is_some_and(|last| now < last.saturating_add(120))
        {
            return Err(error(
                "Recent TCL shutdown attempt; another power toggle is blocked for 120 seconds",
            ));
        }
        let memory = ShutdownMemory {
            identity: self.memory.identity,
            attempt: Some(now),
        };
        save_private(&self.path, &memory)?;
        self.memory = memory;
        Ok(())
    }
    fn confirmed_off(&mut self) -> Result<()> {
        if self.memory.attempt.is_some() {
            let memory = ShutdownMemory {
                identity: self.memory.identity,
                attempt: None,
            };
            save_private(&self.path, &memory)?;
            self.memory = memory;
        }
        Ok(())
    }
}

pub struct RemoteManager<const TCL: bool> {
    config: Config,
    shutdown_guard: Option<ShutdownGuard>,
    _lock: File,
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
    pub fn status(&self) -> Result<Status> {
        self.config
            .remote::<TCL>(Instant::now() + Duration::from_secs(self.config.timeout_seconds))?
            .status
            .ok_or_else(|| error("Missing Android TV power state"))
    }
    pub fn apply(&mut self, power: bool) -> Result<Value> {
        let mut remote = self
            .config
            .remote::<TCL>(Instant::now() + Duration::from_secs(self.config.timeout_seconds))?;
        if remote.status.as_ref().is_some_and(|s| s.power == power) {
            if !power && let Some(guard) = self.shutdown_guard.as_mut() {
                guard.confirmed_off()?;
            }
            return Ok(json!({"applied":false,"verified":true,"status":remote.status}));
        }
        // TCL ignores SLEEP on some firmware. Use POWER only after a fresh,
        // identity-checked on state and a durable attempt; never expose raw POWER.
        let code = if power {
            224
        } else if let Some(guard) = self.shutdown_guard.as_mut() {
            guard.record_attempt()?;
            26
        } else {
            223
        };
        remote.key(code)?;
        // A new total verification deadline, still no reconnect/replay of controls.
        remote
            .stream
            .sock
            .set_deadline(Instant::now() + Duration::from_secs(self.config.verify_seconds));
        if let Err(failure) = remote.until(|r| r.status.as_ref().is_some_and(|s| s.power == power))
        {
            return if TCL && !power {
                Err(error(
                    "TCL shutdown sent once; off is unverified; another toggle is blocked for 120 seconds",
                ))
            } else {
                Err(failure)
            };
        }
        if !power && let Some(guard) = self.shutdown_guard.as_mut() {
            guard.confirmed_off()?;
        }
        Ok(json!({"applied":true,"verified":true,"status":remote.status}))
    }
    pub fn key(&mut self, key: &str) -> Result<Value> {
        let code = key_code(key).ok_or_else(|| error("Unsupported Android TV key"))?;
        let deadline = Instant::now() + Duration::from_secs(self.config.timeout_seconds);
        let mut remote = self.config.remote::<TCL>(deadline)?;
        remote.current_app = None;
        remote.key(code)?;
        remote.finish_key(deadline)?;
        Ok(json!({"sent":true,"verified":false,"key":key,"current_app":remote.current_app}))
    }
    fn configure(&mut self, settings: &Value) -> Result<Value> {
        crate::settings::validate(SETTINGS, settings)?;
        if settings.as_object().is_none_or(|s| s.len() != 1) {
            return Err(error("Send app launch or text separately"));
        }
        if let Some(link) = settings["app_link"].as_str() {
            validate_app_link(link)?;
        }
        let mut remote = self
            .config
            .remote::<TCL>(Instant::now() + Duration::from_secs(self.config.timeout_seconds))?;
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
        Ok(
            json!({"sent":true,"verified":false,"note":"Android remote does not acknowledge app/text controls; never replay automatically"}),
        )
    }
}
fn validate_app_link(link: &str) -> Result<()> {
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
const KEYS: &[&str] = &[
    "home",
    "back",
    "up",
    "down",
    "left",
    "right",
    "enter",
    "volume_up",
    "volume_down",
    "mute",
    "play_pause",
    "play",
    "pause",
    "channel_up",
    "channel_down",
];
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
    const SUPPORT: crate::entity::Support = crate::entity::Support::MEDIA
        .union(crate::entity::Support::CONFIGURE)
        .union(crate::entity::Support::APP_INPUT);
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
                },
            )
            .configurable(SETTINGS),
        );
    }
    fn read_device(&mut self, index: usize) -> Result<Value> {
        if index != 0 {
            return Err(error("Unknown Android TV device"));
        }
        Ok(serde_json::to_value(self.status()?)?)
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
        let remote = self
            .config
            .remote::<TCL>(Instant::now() + Duration::from_secs(self.config.timeout_seconds))?;
        Ok(
            json!({"app_link":remote.features&512!=0,"text":remote.features&4!=0,"current_app":remote.current_app}),
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
        Text(&'static str),
        NoKey,
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
            let features = if matches!(command, Some(TestControl::Launch(_) | TestControl::Text(_)))
            {
                35 | 512 | 4
            } else {
                35
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
                Some(features)
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
                Some(features)
            );
            let mut start = Vec::new();
            if on {
                pb::integer(1, 1, &mut start);
            }
            pb::write(&mut stream, 40, &start)?;
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
            if let Some(TestControl::Launch(link)) = command {
                let response = pb::read(&mut stream)?;
                assert_eq!(
                    Message::new(&response)?
                        .nested(90)?
                        .expect("app launch")
                        .text(1)?,
                    Some(link)
                );
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
        assert!(manager.apply(false).is_err());
        worker.join().expect("server")?;
        Ok(())
    }
    #[test]
    fn tcl_uncertain_shutdown_is_blocked_after_restart_until_confirmed_off() -> Result<()> {
        let temp = support::Directory::new()?;
        let (address, worker, pin) = server_as(true, Some(26), "Smart TV", "TCL", false, true)?;
        let path = tcl_config(&temp, address, pin)?;
        let mut manager = TclManager::open(&path)?;
        assert!(
            manager
                .apply(false)
                .expect_err("unconfirmed shutdown")
                .to_string()
                .contains("sent once")
        );
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
    fn app_links_reject_installation_and_non_navigation_schemes() {
        for link in [
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
