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
/// A second, independently pinned interface on the same physical TV. This
/// contains no media URL and cannot target a host other than its TV adapter.
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Wake {
    server_cert_sha256: String,
    #[serde(default = "port")]
    port: u16,
    #[serde(default = "timeout")]
    timeout_seconds: u64,
}
pub(crate) struct WakeFailure {
    pub command_sent: Option<bool>,
    pub cause: Box<dyn std::error::Error + Send + Sync>,
}
impl Wake {
    pub(crate) fn validate(&self) -> Result<()> {
        media::valid_endpoint(self.port, None, self.timeout_seconds)?;
        media::hex::<32>(&self.server_cert_sha256).map(|_| ())
    }
    pub(crate) fn activate(
        &self,
        host: IpAddr,
        deadline: Instant,
    ) -> std::result::Result<bool, WakeFailure> {
        let config = Config {
            name: "tv_wake".into(),
            host,
            server_cert_sha256: self.server_cert_sha256.clone(),
            port: self.port,
            timeout_seconds: self.timeout_seconds,
            connect: None,
        };
        let mut sent = Some(false);
        (|| -> Result<bool> {
            let mut channel = Channel::open_until(&config, deadline)?;
            let receiver = channel.receiver()?;
            // Do not interrupt an already active receiver merely because its
            // Android remote interface is temporarily unavailable.
            if receiver["isStandBy"].as_bool() == Some(false) {
                return Ok(false);
            }
            if receiver["isStandBy"].as_bool() != Some(true) {
                return Err(error("Cast wake requires an explicit standby reading"));
            }
            channel.activate_default(&receiver, &mut sent)?;
            Ok(true)
        })()
        .map_err(|cause| WakeFailure {
            command_sent: sent,
            cause,
        })
    }
}
pub struct Manager {
    config: Config,
    _lock: crate::config::FileLock,
}
struct Channel {
    stream: StreamOwned<ClientConnection, Timed>,
    next: u64,
}
impl Channel {
    fn open(config: &Config) -> Result<Self> {
        Self::open_until(
            config,
            Instant::now() + Duration::from_secs(config.timeout_seconds),
        )
    }
    fn open_until(config: &Config, deadline: Instant) -> Result<Self> {
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
            deadline.min(Instant::now() + Duration::from_secs(config.timeout_seconds)),
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
        let wait_for_load = namespace == MEDIA && payload["type"] == "LOAD";
        let id = self.next;
        self.next += 1;
        payload["requestId"] = json!(id);
        let requested_url = payload["media"]["contentId"].as_str().map(str::to_owned);
        self.send(namespace, dest, payload)?;
        let mut loading_session = None;
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
            let completed_load = wait_for_load
                && value["type"] == "MEDIA_STATUS"
                && value.get("requestId").is_none_or(|v| *v == 0)
                && loading_session.is_some_and(|session| {
                    value["status"][0]["mediaSessionId"].as_u64() == Some(session)
                })
                && value["status"][0]["media"]
                    .get("contentId")
                    .is_none_or(|url| {
                        requested_url
                            .as_deref()
                            .is_some_and(|expected| *url == expected)
                    })
                && matches!(
                    value["status"][0]["playerState"].as_str(),
                    Some("PLAYING" | "PAUSED")
                );
            if ns == namespace && (value["requestId"] == id || completed_load) {
                if value["type"]
                    .as_str()
                    .is_some_and(|s| s.ends_with("ERROR") || s == "INVALID_REQUEST")
                {
                    return Err(error("Cast request rejected; no retry"));
                }
                // TCL first echoes the LOAD request id with IDLE/LOADING, then
                // completes it with PLAYING, sometimes as an unsolicited status
                // without the request id. Match that only to the new session
                // and requested URL. Closing the sender
                // on the initial echo can cancel the load. Service heartbeats
                // and wait on this connection; never issue LOAD again.
                if wait_for_load
                    && (value["status"][0]["extendedStatus"]["playerState"] == "LOADING"
                        || matches!(
                            value["status"][0]["playerState"].as_str(),
                            Some("BUFFERING" | "LOADING")
                        ))
                {
                    if let Some(url) =
                        value["status"][0]["media"]["contentId"]
                            .as_str()
                            .or_else(|| {
                                value["status"][0]["extendedStatus"]["media"]["contentId"].as_str()
                            })
                    {
                        if requested_url.as_deref() != Some(url) {
                            return Err(error(
                                "Cast LOAD acknowledged a different URL; do not replay",
                            ));
                        }
                        loading_session =
                            value["status"][0]["mediaSessionId"].as_u64().or_else(|| {
                                value["status"][0]["extendedStatus"]["mediaSessionId"].as_u64()
                            });
                    }
                    continue;
                }
                if completed_load && value["status"][0]["media"].get("contentId").is_none() {
                    // Adaptive-stream updates are incremental. Read full metadata
                    // once on this same connection instead of inferring a URL
                    // or retaining/merging snapshots. This is never another LOAD.
                    let fresh = self.call(MEDIA, dest, json!({"type":"GET_STATUS"}))?;
                    if fresh["status"][0]["mediaSessionId"].as_u64() != loading_session
                        || !requested_url
                            .as_deref()
                            .is_some_and(|url| fresh["status"][0]["media"]["contentId"] == url)
                    {
                        return Err(error(
                            "Cast load session changed before verification; do not replay",
                        ));
                    }
                    return Ok(fresh);
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
    fn activate_default(&mut self, receiver: &Value, sent: &mut Option<bool>) -> Result<Value> {
        // A registered background receiver may not become foreground on LAUNCH.
        // End only that default receiver, confirm removal, and launch once.
        if let Some(app) = receiver["applications"]
            .as_array()
            .and_then(|a| a.iter().find(|a| a["appId"] == "CC1AD845"))
        {
            let session = app["sessionId"]
                .as_str()
                .filter(|s| !s.is_empty() && s.len() <= 128)
                .ok_or_else(|| {
                    error("Missing bounded default Cast receiver session; no replacement sent")
                })?;
            *sent = None;
            let stopped = self.call(RECEIVER, "receiver-0", json!({"type":"STOP","sessionId":session}))
                .map_err(|failure| error(format!("Cast receiver stop attempted once; outcome unknown; no launch or load sent; do not replay: {failure}")))?;
            *sent = Some(true);
            if !stopped["status"].is_object()
                || !stopped["status"].get("applications").is_none_or(|apps| {
                    apps.as_array()
                        .is_some_and(|a| a.iter().all(|a| a["appId"] != "CC1AD845"))
                })
            {
                return Err(error(
                    "Default Cast receiver stop not confirmed; no launch or load sent; do not replay",
                ));
            }
        }
        *sent = None;
        let receiver = self.call(
            RECEIVER,
            "receiver-0",
            json!({"type":"LAUNCH","appId":"CC1AD845"}),
        )?["status"]
            .take();
        *sent = Some(true);
        if Self::transport(&receiver).is_none() {
            return Err(error("Default Cast media receiver did not launch"));
        }
        Ok(receiver)
    }
    fn transport(status: &Value) -> Option<&str> {
        status["applications"]
            .as_array()?
            .iter()
            .filter(|a| a["appId"] == "CC1AD845")
            .find_map(|a| {
                a["transportId"]
                    .as_str()
                    .filter(|s| !s.is_empty() && s.len() <= 128)
            })
    }
    fn media_transport(status: &Value) -> Option<&str> {
        status["applications"].as_array()?.iter().find_map(|app| {
            app["namespaces"]
                .as_array()?
                .iter()
                .any(|n| n["name"] == MEDIA)
                .then(|| {
                    app["transportId"]
                        .as_str()
                        .filter(|s| !s.is_empty() && s.len() <= 128)
                })
                .flatten()
        })
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
        Self::from_source(&crate::config::Source::file(path))
    }
    pub fn from_source(source: &crate::config::Source<'_>) -> Result<Self> {
        let path = source.path();
        let config: Config = source.decode()?;
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
        let status = if let Some(t) = Channel::media_transport(&receiver) {
            c.media_status(t)?
        } else {
            Value::Null
        };
        Ok(json!({"receiver":receiver,"media":status}))
    }
    fn control(&self, command: Command, args: &Value) -> Result<Value> {
        let mut c = Channel::open(&self.config)?;
        let receiver = c.receiver()?;
        let result = if matches!(command, Command::Play) {
            let m = &args["media"];
            if !crate::playback::valid_play(m) {
                return Err(error("Invalid media request"));
            }
            let receiver = c.activate_default(&receiver, &mut Some(false))?;
            let transport = Channel::transport(&receiver)
                .ok_or_else(|| error("Default Cast media receiver did not launch"))?;
            c.send(CONNECTION, transport, json!({"type":"CONNECT","origin":{}}))?;
            c.call(MEDIA,transport,json!({"type":"LOAD","autoplay":true,"media":{"contentId":m["url"],"contentType":m["content_type"],"streamType":"BUFFERED"}}))
                .map_err(|failure| error(format!("Cast LOAD attempted once; outcome unknown; do not replay: {failure}")))?
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
                let t = Channel::media_transport(&receiver).ok_or_else(|| {
                    error("Cast application does not advertise media playback support")
                })?;
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
    fn from_source(source: &crate::config::Source<'_>) -> Result<Self> {
        Self::from_source(source)
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

#[cfg(test)]
pub(crate) mod tests;
