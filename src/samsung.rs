//! Samsung UE40H6200 encrypted Socket.IO control using an existing PIN pairing.
use crate::{
    Result,
    config::{lock_configuration, read_bounded, save_private},
    device::{Adapter, Field, Hints, Input, Tool},
    error, http, media,
    websocket::WebSocket,
};
use aes::Aes128;
use cipher::{BlockEncrypt, KeyInit};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    net::{IpAddr, SocketAddr},
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
fn port() -> u16 {
    8000
}
fn info_port() -> u16 {
    8001
}
fn timeout() -> u64 {
    8
}
fn verify() -> u64 {
    20
}
fn off_key() -> String {
    "KEY_POWEROFF".into()
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub host: IpAddr,
    #[serde(default = "port")]
    pub port: u16,
    #[serde(default = "info_port")]
    pub info_port: u16,
    pub device_id: String,
    pub mac: String,
    pub token: String,
    pub session_id: u32,
    pub wake_address: SocketAddr,
    #[serde(default = "off_key")]
    pub off_key: String,
    #[serde(default = "timeout")]
    pub timeout_seconds: u64,
    #[serde(default = "verify")]
    pub verify_seconds: u64,
    #[serde(default)]
    pub connect: Option<SocketAddr>,
    #[serde(default)]
    pub info_connect: Option<SocketAddr>,
}
#[derive(Serialize)]
pub struct Status {
    pub model: &'static str,
    pub power: Option<bool>,
    pub evidence: &'static str,
}
#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Memory {
    device_id: String,
    shutdown_attempt: Option<u64>,
}
pub struct Manager {
    config: Config,
    path: PathBuf,
    memory: Memory,
    _lock: crate::config::FileLock,
}
impl Config {
    fn validate(&self) -> Result<()> {
        media::valid_endpoint(self.port, self.connect, self.timeout_seconds)?;
        media::valid_endpoint(self.info_port, self.info_connect, self.timeout_seconds)?;
        media::hex::<16>(&self.token)?;
        mac_bytes(&self.mac)?;
        if self.device_id.is_empty()
            || self.device_id.len() > 64
            || !self
                .device_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
            || self.wake_address.port() == 0
            || !(1..=60).contains(&self.verify_seconds)
            || !matches!(self.off_key.as_str(), "KEY_POWEROFF" | "KEY_POWER")
        {
            return Err(error(
                "Invalid Samsung identity, wake address, off key or verification interval",
            ));
        }
        Ok(())
    }
    fn address(&self) -> SocketAddr {
        self.connect
            .unwrap_or(SocketAddr::new(self.host, self.port))
    }
    fn identity(&self, deadline: Instant) -> Result<Option<bool>> {
        let address = SocketAddr::new(self.host, self.info_port);
        let body = http::send(
            self.info_connect.unwrap_or(address),
            &address.to_string(),
            deadline,
            http::Request {
                path: "/api/v2/",
                content_type: None,
                body: None,
                cookie: None,
                max_response_bytes: 16384,
            },
        )?
        .body;
        let info: Value = serde_json::from_slice(&body)
            .map_err(|_| error("Invalid Samsung device information"))?;
        let device = &info["device"];
        if device["modelName"] != "UE40H6200" || device["id"].as_str() != Some(&self.device_id) {
            return Err(error(
                "Samsung live model or identity differs from configuration",
            ));
        }
        match device["PowerState"].as_str() {
            Some("on") => Ok(Some(true)),
            Some("standby" | "off") => Ok(Some(false)),
            None => Ok(None),
            _ => Err(error("Unknown Samsung power state")),
        }
    }
    fn remote(&self, deadline: Instant) -> Result<WebSocket> {
        let address = SocketAddr::new(self.host, self.port);
        let handshake = http::send(
            self.address(),
            &address.to_string(),
            deadline,
            http::Request {
                path: "/socket.io/1/",
                content_type: None,
                body: None,
                cookie: None,
                max_response_bytes: 1024,
            },
        )?
        .body;
        let text = std::str::from_utf8(&handshake)
            .map_err(|_| error("Invalid Samsung Socket.IO handshake"))?;
        let app = text.split(':').next().unwrap_or_default();
        if app.is_empty()
            || app.len() > 128
            || !app
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return Err(error("Invalid Samsung Socket.IO session"));
        }
        let mut ws = WebSocket::open(
            self.address(),
            &address.to_string(),
            &format!("/socket.io/1/websocket/{app}"),
            deadline,
        )?;
        ws.text("1::/com.samsung.companion")?;
        for _ in 0..32 {
            match ws.read()?.as_str() {
                "1::/com.samsung.companion" => return Ok(ws),
                "2::" => ws.text("2::")?,
                "1::" => {}
                _ => return Err(error("Samsung companion channel rejected")),
            }
        }
        Err(error("Samsung companion channel did not start"))
    }
}
impl Manager {
    pub fn open(path: &Path) -> Result<Self> {
        Self::from_source(&crate::config::Source::file(path))
    }
    pub fn from_source(source: &crate::config::Source<'_>) -> Result<Self> {
        let path = source.path();
        let config: Config = source.decode()?;
        config.validate()?;
        let lock = lock_configuration(path)?;
        let state_path = path.with_extension("state.json");
        let memory = match read_bounded(&state_path, "Samsung state") {
            Ok(b) => {
                let state: Memory =
                    serde_json::from_slice(&b).map_err(|_| error("Invalid Samsung state"))?;
                if state.device_id != config.device_id {
                    return Err(error("Samsung state identity changed"));
                }
                state
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Memory {
                device_id: config.device_id.clone(),
                ..Memory::default()
            },
            Err(e) => return Err(e.into()),
        };
        Ok(Self {
            config,
            path: state_path,
            memory,
            _lock: lock,
        })
    }
    pub fn status(&self) -> Result<Status> {
        self.status_until(Instant::now() + Duration::from_secs(self.config.timeout_seconds))
    }
    fn status_until(&self, deadline: Instant) -> Result<Status> {
        if let Some(power) = self.config.identity(deadline)? {
            return Ok(Status {
                model: "UE40H6200",
                power: Some(power),
                evidence: "device_power_state",
            });
        }
        self.config.remote(deadline)?;
        Ok(Status {
            model: "UE40H6200",
            power: Some(true),
            evidence: "identity_and_active_companion_channel",
        })
    }
    pub fn apply(&mut self, power: bool) -> Result<Value> {
        let before = self.status();
        if let Ok(status) = &before
            && status.power == Some(power)
        {
            return Ok(json!({"applied":false,"verified":true,"status":status}));
        }
        // An identity/protocol error must not be mistaken for a sleeping TV.
        if let Err(ref e) = before
            && !network_error(e.as_ref())
        {
            return Err(error("Samsung state unavailable; control blocked"));
        }
        if power {
            crate::wol::send(mac_bytes(&self.config.mac)?, self.config.wake_address)?;
            let until = Instant::now() + Duration::from_secs(self.config.verify_seconds);
            while Instant::now() < until {
                if let Ok(status) = self.status_until(
                    until.min(Instant::now() + Duration::from_secs(self.config.timeout_seconds)),
                ) && status.power == Some(true)
                {
                    return Ok(json!({"applied":true,"verified":true,"status":status}));
                }
                std::thread::sleep(Duration::from_millis(500));
            }
            return Err(error("Samsung wake sent once; on state not verified"));
        }
        // Never resend a potentially toggling shutdown, including after restart.
        let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
        if self
            .memory
            .shutdown_attempt
            .is_some_and(|last| now < last.saturating_add(120))
        {
            return Err(error(
                "Recent Samsung shutdown attempt; automatic resend blocked for 120 seconds",
            ));
        }
        if before.as_ref().ok().and_then(|s| s.power) != Some(true) {
            return Err(error("Samsung is not confirmed on; shutdown blocked"));
        }
        let deadline = Instant::now() + Duration::from_secs(self.config.timeout_seconds);
        let mut remote = self.config.remote(deadline)?;
        self.memory.shutdown_attempt = Some(now);
        save_private(&self.path, &self.memory)?;
        remote.text(&encrypted_command(
            &self.config.token,
            self.config.session_id,
            &self.config.off_key,
        )?)?;
        drop(remote);
        // H6200 normally supplies no explicit PowerState. Network loss is ambiguous.
        std::thread::sleep(Duration::from_secs(1));
        let status = self.status().ok();
        let verified = status.as_ref().is_some_and(|s| s.power == Some(false));
        Ok(
            json!({"applied":true,"verified":verified,"status":status,"evidence":if verified{"device_power_state"}else{"shutdown_sent_once_no_confirmed_off_state"}}),
        )
    }
    pub fn key(&mut self, key: &str) -> Result<Value> {
        let wire = key_code(key).ok_or_else(|| error("Unsupported Samsung key"))?;
        if self.status()?.power != Some(true) {
            return Err(error("Samsung must be on for remote keys"));
        }
        let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
        if self
            .memory
            .shutdown_attempt
            .is_some_and(|last| now < last.saturating_add(120))
        {
            return Err(error("Samsung shutdown cooldown blocks remote keys"));
        }
        let mut remote = self
            .config
            .remote(Instant::now() + Duration::from_secs(self.config.timeout_seconds))?;
        remote.text(&encrypted_command(
            &self.config.token,
            self.config.session_id,
            wire,
        )?)?;
        Ok(json!({"sent":true,"verified":false,"key":key}))
    }
}
fn network_error(e: &(dyn std::error::Error + 'static)) -> bool {
    e.downcast_ref::<std::io::Error>().is_some_and(|e| {
        matches!(
            e.kind(),
            std::io::ErrorKind::ConnectionRefused
                | std::io::ErrorKind::TimedOut
                | std::io::ErrorKind::NotConnected
                | std::io::ErrorKind::HostUnreachable
                | std::io::ErrorKind::NetworkUnreachable
        )
    })
}
fn mac_bytes(text: &str) -> Result<[u8; 6]> {
    crate::wol::mac_bytes(text)
}
fn encrypted_command(token: &str, session: u32, key: &str) -> Result<String> {
    let mut bytes = serde_json::to_vec(
        &json!({"method":"POST","body":{"plugin":"RemoteControl","param1":"uuid:12345","param2":"Click","param3":key,"param4":false,"api":"SendRemoteKey","version":"1.000"}}),
    )?;
    let pad = 16 - bytes.len() % 16;
    bytes.resize(bytes.len() + pad, pad as u8);
    let cipher = Aes128::new_from_slice(&media::hex::<16>(token)?)
        .map_err(|_| error("Invalid Samsung token"))?;
    for block in bytes.chunks_exact_mut(16) {
        cipher.encrypt_block(cipher::generic_array::GenericArray::from_mut_slice(block));
    }
    Ok(format!(
        "5::/com.samsung.companion:{}",
        json!({"name":"callCommon","args":[{"Session_Id":session,"body":serde_json::to_string(&bytes)?}]})
    ))
}
fn key_code(key: &str) -> Option<&'static str> {
    Some(match key {
        "home" => "KEY_MENU",
        "back" => "KEY_RETURN",
        "up" => "KEY_UP",
        "down" => "KEY_DOWN",
        "left" => "KEY_LEFT",
        "right" => "KEY_RIGHT",
        "enter" => "KEY_ENTER",
        "hdmi" => "KEY_HDMI",
        "volume_up" => "KEY_VOLUP",
        "volume_down" => "KEY_VOLDOWN",
        "mute" => "KEY_MUTE",
        "play" => "KEY_PLAY",
        "pause" => "KEY_PAUSE",
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
    "hdmi",
    "volume_up",
    "volume_down",
    "mute",
    "play",
    "pause",
];
const KEY: Input = Input::Field {
    field: Field {
        name: "key",
        schema: || json!({"type":"string","enum":KEYS}),
        valid: |v| v.as_str().and_then(key_code).is_some(),
        error: "Unsupported Samsung key",
    },
    shape_error: "Expected only key",
};
impl Adapter for Manager {
    const SUPPORT: crate::entity::Support =
        crate::entity::Support::MEDIA.union(crate::entity::Support::WAKE);
    fn wake_targets<'a>(&'a self, visit: &mut impl FnMut(crate::wol::Target<'a>)) {
        if let Ok(mac) = mac_bytes(&self.config.mac) {
            visit(crate::wol::Target {
                device: crate::entity::Device {
                    family: crate::device::Family::Samsung,
                    index: 0,
                },
                name: "samsung",
                mac,
                verified: true,
            });
        }
    }
    fn wake_device(&mut self, index: usize) -> Result<Value> {
        if index != 0 {
            return Err(error("Unknown Samsung device"));
        }
        self.apply(true)
    }
    fn entities<'a>(&'a self, visit: &mut impl FnMut(crate::entity::Entity<'a>)) {
        use crate::{
            device::Family,
            entity::{Capabilities, Domain, Entity},
        };
        visit(Entity::new(
            Family::Samsung,
            0,
            Domain::MediaPlayer,
            "samsung",
            "UE40H6200",
            Capabilities::Media {
                keys: KEYS,
                verified_off: false,
                hold: false,
                voice: false,
                activities: &[],
            },
        ));
    }
    fn read_device(&mut self, index: usize) -> Result<Value> {
        if index != 0 {
            return Err(error("Unknown Samsung device"));
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
            return Err(error("Unknown Samsung device"));
        }
        match command {
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
            _ => Err(error("Unsupported Samsung command")),
        }
    }

    const TOOLS: &'static [Tool] = &[
        Tool {
            name: "samsung_status",
            description: "Read Samsung identity and fresh remote/power state",
            input: Input::Empty,
            hints: Hints::READ,
        },
        Tool {
            name: "samsung_apply",
            description: "Wake or request shutdown once; inspect verified before assuming physical power",
            input: media::POWER,
            hints: Hints {
                idempotent: false,
                ..Hints::CONTROL
            },
        },
        Tool {
            name: "samsung_key",
            description: "Send one supported Samsung remote key (no power toggle)",
            input: KEY,
            hints: Hints {
                idempotent: false,
                ..Hints::CONTROL
            },
        },
    ];
    fn from_source(source: &crate::config::Source<'_>) -> Result<Self> {
        Self::from_source(source)
    }
    fn execute(&mut self, name: &str, args: &Value) -> Result<Value> {
        match name {
            "samsung_status" => Ok(serde_json::to_value(self.status()?)?),
            "samsung_apply" => self.apply(
                args["power"]
                    .as_bool()
                    .ok_or_else(|| error("Expected boolean power"))?,
            ),
            "samsung_key" => self.key(args["key"].as_str().ok_or_else(|| error("Expected key"))?),
            _ => Err(error("Unknown Samsung operation")),
        }
    }
}
