//! Pinned UPnP identity and bounded SOAP AVTransport/RenderingControl.
use crate::{
    Result,
    device::{Adapter, Tool},
    entity::{Capabilities, Command, Domain, Entity, Support},
    error, web,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{path::Path, time::Duration};
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub name: String,
    pub description_url: String,
    pub udn: String,
    #[serde(default = "timeout")]
    pub timeout_seconds: u64,
}
fn timeout() -> u64 {
    8
}
pub struct Manager {
    config: Config,
    _lock: crate::config::FileLock,
}
struct Services {
    av: String,
    render: String,
}
fn elements(bytes: &[u8]) -> Result<Vec<(String, String)>> {
    use quick_xml::{Reader, events::Event};
    let mut r = Reader::from_reader(bytes);
    r.config_mut().trim_text(true);
    let mut stack = Vec::new();
    let mut rows = Vec::new();
    loop {
        match r.read_event()? {
            Event::Start(e) => {
                if stack.len() >= 24 {
                    return Err(error("XML nesting exceeds limit"));
                }
                stack.push(String::from_utf8(e.local_name().as_ref().to_vec())?);
                if rows.len() >= 1024 {
                    return Err(error("XML node limit exceeded"));
                }
                rows.push((stack.join("/"), String::new()));
            }
            Event::Empty(e) => {
                if rows.len() >= 1024 {
                    return Err(error("XML node limit exceeded"));
                }
                rows.push((
                    format!(
                        "{}/{}",
                        stack.join("/"),
                        std::str::from_utf8(e.local_name().as_ref())?
                    ),
                    String::new(),
                ));
            }
            Event::End(_) => {
                stack.pop();
            }
            Event::Text(t) => {
                if rows.len() >= 1024 {
                    return Err(error("XML node limit exceeded"));
                }
                rows.push((stack.join("/"), t.unescape()?.into_owned()));
            }
            Event::DocType(_) => return Err(error("XML document types are unsupported")),
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(rows)
}
fn text<'a>(rows: &'a [(String, String)], tag: &str) -> Option<&'a str> {
    rows.iter()
        .find(|(p, v)| !v.is_empty() && p.rsplit('/').next() == Some(tag))
        .map(|(_, v)| v.as_str())
}
fn position_seconds(text: &str) -> Option<f64> {
    let parts: Vec<_> = text.split(':').collect();
    if parts.len() != 3 {
        return None;
    }
    let h = parts[0].parse::<u64>().ok()?;
    let m = parts[1].parse::<u64>().ok()?;
    let s = parts[2].parse::<f64>().ok()?;
    (m < 60 && s.is_finite() && (0.0..60.0).contains(&s))
        .then_some(h as f64 * 3600.0 + m as f64 * 60.0 + s)
}
fn xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}
impl Manager {
    pub fn open(path: &Path) -> Result<Self> {
        Self::from_source(&crate::config::Source::file(path))
    }
    pub fn from_source(source: &crate::config::Source<'_>) -> Result<Self> {
        let path = source.path();
        let config: Config = source.decode()?;
        web::Url::parse(&config.description_url)?;
        if !crate::config::valid_unique_labels(std::iter::once(config.name.as_str()), 32, b"-_")
            || !config.udn.starts_with("uuid:")
            || config.udn.len() > 128
            || !(1..=30).contains(&config.timeout_seconds)
        {
            return Err(error("Invalid DLNA configuration"));
        }
        Ok(Self {
            config,
            _lock: crate::config::lock_configuration(path)?,
        })
    }
    fn services(&self) -> Result<Services> {
        let bytes = web::request(
            &self.config.description_url,
            "GET",
            &[],
            &[],
            Duration::from_secs(self.config.timeout_seconds),
        )?
        .body;
        let rows = elements(&bytes)?;
        if text(&rows, "UDN") != Some(&self.config.udn) {
            return Err(error("DLNA device identity mismatch"));
        }
        let mut av = None;
        let mut render = None;
        let mut service = None;
        for (path, value) in &rows {
            if value.is_empty() {
                continue;
            }
            match path.rsplit('/').next() {
                Some("serviceType") => service = Some(value.as_str()),
                Some("controlURL") => {
                    let url = self.resolve(value)?;
                    match service {
                        Some("urn:schemas-upnp-org:service:AVTransport:1") => av = Some(url),
                        Some("urn:schemas-upnp-org:service:RenderingControl:1") => {
                            render = Some(url)
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        }
        Ok(Services {
            av: av.ok_or_else(|| error("Missing AVTransport service"))?,
            render: render.ok_or_else(|| error("Missing RenderingControl service"))?,
        })
    }
    fn resolve(&self, path: &str) -> Result<String> {
        let source = web::Url::parse(&self.config.description_url)?;
        let origin = format!(
            "{}://{}:{}",
            if source.secure { "https" } else { "http" },
            source.host,
            source.port
        );
        let url = if path.starts_with("http") {
            path.to_owned()
        } else if path.starts_with('/') {
            format!("{origin}{path}")
        } else {
            let parent = source.path.rsplit_once('/').map_or("", |(p, _)| p);
            format!("{origin}{parent}/{path}")
        };
        let dest = web::Url::parse(&url)?;
        if dest.host != source.host || dest.port != source.port || dest.secure != source.secure {
            return Err(error("DLNA service endpoint changed origin"));
        }
        Ok(url)
    }
    fn soap(
        &self,
        services: &Services,
        render: bool,
        action: &str,
        params: &[(&str, String)],
    ) -> Result<Vec<(String, String)>> {
        let kind = if render {
            "RenderingControl"
        } else {
            "AVTransport"
        };
        let urn = format!("urn:schemas-upnp-org:service:{kind}:1");
        let args: String = params
            .iter()
            .map(|(k, v)| format!("<{k}>{}</{k}>", xml(v)))
            .collect();
        let body = format!(
            "<?xml version=\"1.0\"?><s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\" s:encodingStyle=\"http://schemas.xmlsoap.org/soap/encoding/\"><s:Body><u:{action} xmlns:u=\"{urn}\"><InstanceID>0</InstanceID>{args}</u:{action}></s:Body></s:Envelope>"
        );
        let response = web::request(
            if render {
                &services.render
            } else {
                &services.av
            },
            "POST",
            &[
                ("Content-Type", "text/xml; charset=utf-8"),
                ("SOAPAction", &format!("\"{urn}#{action}\"")),
            ],
            body.as_bytes(),
            Duration::from_secs(self.config.timeout_seconds),
        )?;
        let rows = elements(&response.body)?;
        if text(&rows, "errorCode").is_some() {
            return Err(error("DLNA SOAP action rejected"));
        }
        let marker = format!("{action}Response");
        if !rows
            .iter()
            .any(|(p, _)| p.rsplit('/').next() == Some(marker.as_str()) && p.contains("/Body/"))
        {
            return Err(error("Missing DLNA action response"));
        }
        Ok(rows)
    }
    fn status_with(&self, s: &Services) -> Result<Value> {
        let t = self.soap(s, false, "GetTransportInfo", &[])?;
        let p = self.soap(s, false, "GetPositionInfo", &[])?;
        let v = self.soap(s, true, "GetVolume", &[("Channel", "Master".into())])?;
        let m = self.soap(s, true, "GetMute", &[("Channel", "Master".into())])?;
        Ok(
            json!({"transport":text(&t,"CurrentTransportState"),"position":text(&p,"RelTime"),"duration":text(&p,"TrackDuration"),"url":text(&p,"TrackURI"),"volume_percent":text(&v,"CurrentVolume").and_then(|x|x.parse::<u8>().ok()),"muted":text(&m,"CurrentMute").and_then(|x|match x{"0"=>Some(false),"1"=>Some(true),_=>None})}),
        )
    }
    fn command(&self, command: Command, args: &Value) -> Result<Value> {
        let s = self.services()?;
        if matches!(command, Command::Play) {
            let m = &args["media"];
            if !crate::playback::valid_play(m) {
                return Err(error("Invalid media"));
            }
            self.soap(
                &s,
                false,
                "SetAVTransportURI",
                &[
                    (
                        "CurrentURI",
                        m["url"]
                            .as_str()
                            .ok_or_else(|| error("Missing URL"))?
                            .into(),
                    ),
                    ("CurrentURIMetaData", String::new()),
                ],
            )?;
            self.soap(&s, false, "Play", &[("Speed", "1".into())])?;
        } else {
            let c = &args["command"];
            if !crate::playback::valid_command(c) {
                return Err(error("Invalid playback command"));
            }
            match c["action"].as_str().unwrap_or_default() {
                "play" => {
                    self.soap(&s, false, "Play", &[("Speed", "1".into())])?;
                }
                "pause" => {
                    self.soap(&s, false, "Pause", &[])?;
                }
                "stop" => {
                    self.soap(&s, false, "Stop", &[])?;
                }
                "seek" => {
                    let n = c["seconds"]
                        .as_f64()
                        .ok_or_else(|| error("Expected seconds"))?
                        as u64;
                    self.soap(
                        &s,
                        false,
                        "Seek",
                        &[
                            ("Unit", "REL_TIME".into()),
                            (
                                "Target",
                                format!("{:02}:{:02}:{:02}", n / 3600, n / 60 % 60, n % 60),
                            ),
                        ],
                    )?;
                }
                "volume" => {
                    self.soap(
                        &s,
                        true,
                        "SetVolume",
                        &[
                            ("Channel", "Master".into()),
                            ("DesiredVolume", c["percent"].to_string()),
                        ],
                    )?;
                }
                "mute" => {
                    self.soap(
                        &s,
                        true,
                        "SetMute",
                        &[
                            ("Channel", "Master".into()),
                            (
                                "DesiredMute",
                                if c["muted"] == true { "1" } else { "0" }.into(),
                            ),
                        ],
                    )?;
                }
                _ => return Err(error("Unsupported playback action")),
            }
        }
        let state = self.status_with(&s)?;
        let c = &args["command"];
        let verified = if matches!(command, Command::Play) {
            state["url"] == args["media"]["url"] && state["transport"] == "PLAYING"
        } else {
            match c["action"].as_str().unwrap_or_default() {
                "play" => state["transport"] == "PLAYING",
                "pause" => state["transport"] == "PAUSED_PLAYBACK",
                "stop" => state["transport"] == "STOPPED",
                "volume" => state["volume_percent"] == c["percent"],
                "mute" => state["muted"] == c["muted"],
                "seek" => state["position"]
                    .as_str()
                    .and_then(position_seconds)
                    .zip(c["seconds"].as_f64())
                    .is_some_and(|(a, b)| (a - b).abs() <= 3.0),
                _ => false,
            }
        };
        Ok(
            json!({"sent":true,"verified":verified,"status":state,"note":if verified{Value::Null}else{json!("Command sent once; requested outcome is not yet confirmed")}}),
        )
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
            crate::device::Family::Dlna,
            0,
            Domain::MediaPlayer,
            &self.config.name,
            "DLNA",
            Capabilities::Playback,
        ));
    }
    fn read_device(&mut self, i: usize) -> Result<Value> {
        if i != 0 {
            return Err(error("Unknown DLNA device"));
        }
        let s = self.services()?;
        self.status_with(&s)
    }
    fn control_device(&mut self, i: usize, c: Command, a: &Value) -> Result<Value> {
        if i != 0 || !matches!(c, Command::Play | Command::Playback) {
            return Err(error("Unsupported DLNA capability"));
        }
        self.command(c, a)
    }
}
