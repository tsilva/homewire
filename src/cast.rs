//! Google Cast v2 framing over certificate-pinned TLS, without a protobuf runtime.
use crate::{
    Result,
    androidtv::Pinned,
    device::{Adapter, Tool},
    entity::{Capabilities, Command, Domain, Entity, Support},
    error, media,
    protobuf::{self as pb, Message},
    transport::Timed,
};
use rustls::{
    ClientConfig, ClientConnection, StreamOwned, client::Resumption, pki_types::ServerName,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    fs::File,
    io::{Read, Write},
    net::{IpAddr, SocketAddr},
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};
const RECEIVER: &str = "urn:x-cast:com.google.cast.receiver";
const MEDIA: &str = "urn:x-cast:com.google.cast.media";
const CONNECTION: &str = "urn:x-cast:com.google.cast.tp.connection";
const HEARTBEAT: &str = "urn:x-cast:com.google.cast.tp.heartbeat";
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub name: String,
    pub host: IpAddr,
    pub server_cert_sha256: String,
    #[serde(default = "port")]
    pub port: u16,
    #[serde(default = "timeout")]
    pub timeout_seconds: u64,
    #[serde(default)]
    pub connect: Option<SocketAddr>,
}
fn port() -> u16 {
    8009
}
fn timeout() -> u64 {
    8
}
pub struct Manager {
    config: Config,
    _lock: File,
}
struct Channel {
    stream: StreamOwned<ClientConnection, Timed>,
    next: u64,
}
impl Channel {
    fn open(config: &Config) -> Result<Self> {
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let verifier = Arc::new(Pinned {
            fingerprint: media::hex::<32>(&config.server_cert_sha256)?,
            provider: provider.clone(),
        });
        let mut tls = ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()?
            .dangerous()
            .with_custom_certificate_verifier(verifier)
            .with_no_client_auth();
        tls.resumption = Resumption::disabled();
        tls.max_fragment_size = Some(1024);
        let mut conn =
            ClientConnection::new(Arc::new(tls), ServerName::IpAddress(config.host.into()))?;
        conn.set_buffer_limit(Some(8192));
        let socket = Timed::connect(
            config
                .connect
                .unwrap_or(SocketAddr::new(config.host, config.port)),
            Instant::now() + Duration::from_secs(config.timeout_seconds),
        )?;
        let mut channel = Self {
            stream: StreamOwned::new(conn, socket),
            next: 1,
        };
        channel.send(
            CONNECTION,
            "receiver-0",
            json!({"type":"CONNECT","origin":{}}),
        )?;
        Ok(channel)
    }
    fn send(&mut self, namespace: &str, dest: &str, payload: Value) -> Result<()> {
        let mut bytes = Vec::new();
        pb::integer(1, 0, &mut bytes);
        pb::bytes(2, b"sender-0", &mut bytes);
        pb::bytes(3, dest.as_bytes(), &mut bytes);
        pb::bytes(4, namespace.as_bytes(), &mut bytes);
        pb::integer(5, 0, &mut bytes);
        pb::bytes(6, payload.to_string().as_bytes(), &mut bytes);
        if bytes.len() > pb::MAX {
            return Err(error("Cast message exceeds limit"));
        }
        self.stream.write_all(&(bytes.len() as u32).to_be_bytes())?;
        self.stream.write_all(&bytes)?;
        self.stream.flush()?;
        Ok(())
    }
    fn call(&mut self, namespace: &str, dest: &str, mut payload: Value) -> Result<Value> {
        let id = self.next;
        self.next += 1;
        payload["requestId"] = json!(id);
        self.send(namespace, dest, payload)?;
        for _ in 0..64 {
            let mut length = [0; 4];
            self.stream.read_exact(&mut length)?;
            let length = u32::from_be_bytes(length) as usize;
            if length == 0 || length > pb::MAX {
                return Err(error("Invalid bounded Cast frame"));
            }
            let mut bytes = vec![0; length];
            self.stream.read_exact(&mut bytes)?;
            let m = Message::new(&bytes)?;
            let ns = m.text(4)?.ok_or_else(|| error("Missing Cast namespace"))?;
            let value: Value =
                serde_json::from_str(m.text(6)?.ok_or_else(|| error("Missing Cast payload"))?)?;
            if ns == HEARTBEAT && value["type"] == "PING" {
                self.send(
                    HEARTBEAT,
                    m.text(2)?.unwrap_or("receiver-0"),
                    json!({"type":"PONG"}),
                )?;
                continue;
            }
            if ns == namespace && value["requestId"] == id {
                if value["type"]
                    .as_str()
                    .is_some_and(|s| s.ends_with("ERROR") || s == "INVALID_REQUEST")
                {
                    return Err(error("Cast request rejected; no retry"));
                }
                return Ok(value);
            }
        }
        Err(error("Cast reply frame limit exceeded"))
    }
    fn receiver(&mut self) -> Result<Value> {
        let mut response = self.call(RECEIVER, "receiver-0", json!({"type":"GET_STATUS"}))?;
        if !response["status"].is_object() {
            return Err(error("Missing Cast receiver status"));
        }
        Ok(response["status"].take())
    }
    fn transport(status: &Value) -> Option<&str> {
        status["applications"]
            .as_array()?
            .iter()
            .find_map(|a| a["transportId"].as_str().filter(|s| s.len() <= 128))
    }
    fn media_status(&mut self, transport: &str) -> Result<Value> {
        self.send(CONNECTION, transport, json!({"type":"CONNECT","origin":{}}))?;
        let response = self.call(MEDIA, transport, json!({"type":"GET_STATUS"}))?;
        Ok(response["status"]
            .as_array()
            .and_then(|s| s.first())
            .cloned()
            .unwrap_or(Value::Null))
    }
}
impl Manager {
    pub fn open(path: &Path) -> Result<Self> {
        let config: Config = media::load(path)?;
        media::valid_endpoint(config.port, config.connect, config.timeout_seconds)?;
        media::hex::<32>(&config.server_cert_sha256)?;
        if !crate::config::valid_unique_labels(std::iter::once(config.name.as_str()), 32, b"-_") {
            return Err(error("Invalid Cast name"));
        }
        Ok(Self {
            config,
            _lock: crate::config::lock_configuration(path)?,
        })
    }
    fn status(&self) -> Result<Value> {
        let mut c = Channel::open(&self.config)?;
        let receiver = c.receiver()?;
        let status = if let Some(t) = Channel::transport(&receiver) {
            c.media_status(t)?
        } else {
            Value::Null
        };
        Ok(json!({"receiver":receiver,"media":status}))
    }
    fn control(&self, command: Command, args: &Value) -> Result<Value> {
        let mut c = Channel::open(&self.config)?;
        let mut receiver = c.receiver()?;
        let result = if matches!(command, Command::Play) {
            let m = &args["media"];
            if !crate::playback::valid_play(m) {
                return Err(error("Invalid media request"));
            }
            if !receiver["applications"]
                .as_array()
                .is_some_and(|a| a.iter().any(|a| a["appId"] == "CC1AD845"))
            {
                receiver = c.call(
                    RECEIVER,
                    "receiver-0",
                    json!({"type":"LAUNCH","appId":"CC1AD845"}),
                )?["status"]
                    .take();
            }
            let transport = Channel::transport(&receiver)
                .ok_or_else(|| error("Default Cast media receiver did not launch"))?;
            c.send(CONNECTION, transport, json!({"type":"CONNECT","origin":{}}))?;
            c.call(MEDIA,transport,json!({"type":"LOAD","autoplay":true,"media":{"contentId":m["url"],"contentType":m["content_type"],"streamType":"BUFFERED"}}))?
        } else {
            let a = &args["command"];
            if !crate::playback::valid_command(a) {
                return Err(error("Invalid playback command"));
            }
            let action = a["action"].as_str().unwrap_or_default();
            if matches!(action, "volume" | "mute") {
                let volume = if action == "volume" {
                    json!({"level":a["percent"].as_f64().ok_or_else(||error("Expected volume"))?/100.0})
                } else {
                    json!({"muted":a["muted"]})
                };
                c.call(
                    RECEIVER,
                    "receiver-0",
                    json!({"type":"SET_VOLUME","volume":volume}),
                )?
            } else {
                let t = Channel::transport(&receiver)
                    .ok_or_else(|| error("Cast has no active media application"))?;
                let current = c.media_status(t)?;
                let session = current["mediaSessionId"]
                    .as_u64()
                    .ok_or_else(|| error("Cast has no media session"))?;
                let mut body = json!({"type":action.to_ascii_uppercase(),"mediaSessionId":session});
                if action == "seek" {
                    body["currentTime"] = a["seconds"].clone();
                }
                c.call(MEDIA, t, body)?
            }
        };
        let observed = result["status"]
            .as_array()
            .and_then(|a| a.first())
            .unwrap_or(&result["status"]);
        let a = &args["command"];
        let verified = if matches!(command, Command::Play) {
            observed["media"]["contentId"] == args["media"]["url"]
                && observed["playerState"] == "PLAYING"
        } else {
            match a["action"].as_str().unwrap_or_default() {
                "play" => observed["playerState"] == "PLAYING",
                "pause" => observed["playerState"] == "PAUSED",
                "stop" => observed.is_null() || observed["playerState"] == "IDLE",
                "seek" => observed["currentTime"]
                    .as_f64()
                    .zip(a["seconds"].as_f64())
                    .is_some_and(|(x, y)| (x - y).abs() <= 2.0),
                "volume" => observed["volume"]["level"]
                    .as_f64()
                    .zip(a["percent"].as_f64())
                    .is_some_and(|(x, y)| (x - y / 100.0).abs() < 0.011),
                "mute" => observed["volume"]["muted"] == a["muted"],
                _ => false,
            }
        };
        Ok(json!({"sent":true,"verified":verified,"status":result}))
    }
}
impl Adapter for Manager {
    const TOOLS: &'static [Tool] = &[];
    const SUPPORT: Support = Support::PLAYBACK;
    fn open(p: &Path) -> Result<Self> {
        Self::open(p)
    }
    fn execute(&mut self, _: &str, _: &Value) -> Result<Value> {
        Err(error("Use media entity tools"))
    }
    fn entities<'a>(&'a self, v: &mut impl FnMut(Entity<'a>)) {
        v(Entity::new(
            crate::device::Family::Cast,
            0,
            Domain::MediaPlayer,
            &self.config.name,
            "Google Cast",
            Capabilities::Playback,
        ));
    }
    fn read_device(&mut self, i: usize) -> Result<Value> {
        if i != 0 {
            return Err(error("Unknown Cast receiver"));
        }
        self.status()
    }
    fn control_device(&mut self, i: usize, c: Command, a: &Value) -> Result<Value> {
        if i != 0 || !matches!(c, Command::Play | Command::Playback) {
            return Err(error("Unsupported Cast capability"));
        }
        self.control(c, a)
    }
}
