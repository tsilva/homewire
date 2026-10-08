//! Durable desired alerts and per-channel delivery acknowledgements.
//! Detection and delivery are independent: detector state is reconciled after
//! restarts, and a failed send never counts as a delivered notification.
use crate::{
    Result, config,
    device::{Adapter, Hints, Input, Tool},
    entity::{Entity, Support},
    error, web,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::File,
    path::{Path, PathBuf},
    time::Duration,
};

const LIMIT: usize = 64;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Channel {
    pub name: String,
    pub url: String,
    #[serde(default)]
    pub push_token: Option<String>,
    #[serde(default)]
    pub registration_info: Value,
    #[serde(default)]
    pub authorization: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub companions: Vec<String>,
    #[serde(default)]
    pub channels: Vec<Channel>,
    #[serde(default = "timeout")]
    pub timeout_seconds: u64,
}
fn timeout() -> u64 {
    8
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Alert {
    pub active: bool,
    pub title: String,
    pub message: String,
    pub updated_at: i64,
    #[serde(default)]
    pub delivered: BTreeMap<String, String>,
    #[serde(default)]
    pub companion_receipts: BTreeMap<String, String>,
}
#[derive(Default, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Memory {
    alerts: BTreeMap<String, Alert>,
}
pub struct Manager {
    config: Config,
    memory: Memory,
    path: PathBuf,
    _lock: File,
    next_attempt: i64,
    retry_at: BTreeMap<(String, String), i64>,
}
impl Manager {
    pub fn open(path: &Path) -> Result<Self> {
        let config: Config = crate::media::load(path)?;
        if config.channels.len() > 4
            || config.companions.len() > 8
            || !config::valid_unique_labels(config.companions.iter().map(String::as_str), 32, b"-_")
            || !(1..=30).contains(&config.timeout_seconds)
            || !config::valid_unique_labels(
                config.channels.iter().map(|c| c.name.as_str()),
                32,
                b"-_",
            )
        {
            return Err(error("Configure at most four named notification channels"));
        }
        for channel in &config.channels {
            if !web::Url::parse(&channel.url)?.secure
                && !channel.url.starts_with("http://127.0.0.1:")
            {
                return Err(error("Notification credentials require HTTPS"));
            }
            if channel
                .push_token
                .as_ref()
                .is_some_and(|t| t.is_empty() || t.len() > 1024)
                || channel
                    .authorization
                    .as_ref()
                    .is_some_and(|t| t.len() > 1024 || t.chars().any(char::is_control))
                || serde_json::to_vec(&channel.registration_info)?.len() > 2048
            {
                return Err(error("Invalid notification registration"));
            }
        }
        let state = path.with_extension("state.json");
        let memory: Memory = match config::read_bounded_limit(&state, "Alert ledger", 65536) {
            Ok(b) => serde_json::from_slice(&b).map_err(|_| error("Invalid alert ledger"))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Memory::default(),
            Err(e) => return Err(e.into()),
        };
        if memory.alerts.len() > LIMIT
            || memory.alerts.iter().any(|(key, a)| {
                !valid_key(key)
                    || a.title.len() > 128
                    || a.message.len() > 1024
                    || a.delivered.len() > 4
                    || a.companion_receipts.len() > 8
                    || a.companion_receipts
                        .iter()
                        .any(|(k, v)| !valid_key(k) || v.len() != 64)
            })
        {
            return Err(error("Alert ledger exceeds configured bounds"));
        }
        Ok(Self {
            config,
            memory,
            path: state,
            _lock: config::lock_configuration(path)?,
            next_attempt: 0,
            retry_at: BTreeMap::new(),
        })
    }
    pub fn record(
        &mut self,
        key: &str,
        active: bool,
        title: &str,
        message: &str,
        now: i64,
    ) -> Result<()> {
        if !valid_key(key) || title.len() > 128 || message.len() > 1024 {
            return Err(error("Invalid bounded alert"));
        }
        if self
            .memory
            .alerts
            .get(key)
            .is_some_and(|a| a.active == active && a.title == title && a.message == message)
        {
            return Ok(());
        }
        let mut next = self.memory.clone();
        if !next.alerts.contains_key(key) && next.alerts.len() >= LIMIT {
            let oldest = next
                .alerts
                .iter()
                .filter(|(_, a)| {
                    !a.active
                        && self
                            .config
                            .channels
                            .iter()
                            .all(|c| a.delivered.get(&c.name) == Some(&fingerprint(c, a)))
                        && self
                            .config
                            .companions
                            .iter()
                            .all(|name| a.companion_receipts.get(name) == Some(&revision(a)))
                })
                .min_by_key(|(_, a)| a.updated_at)
                .map(|(k, _)| k.clone())
                .ok_or_else(|| error("Alert ledger full; pending alerts are retained"))?;
            next.alerts.remove(&oldest);
        }
        let delivered = next
            .alerts
            .get(key)
            .map(|a| a.delivered.clone())
            .unwrap_or_default();
        next.alerts.insert(
            key.into(),
            Alert {
                active,
                title: title.into(),
                message: message.into(),
                updated_at: now,
                delivered,
                companion_receipts: BTreeMap::new(),
            },
        );
        self.persist(&next)?;
        self.memory = next;
        self.next_attempt = now;
        self.retry_at
            .retain(|(k, _), _| k != key && self.memory.alerts.contains_key(k));
        Ok(())
    }
    pub fn reconcile(
        &mut self,
        scope: &str,
        name: &str,
        faults: &[String],
        now: i64,
    ) -> Result<()> {
        let key = format!("{scope}.{name}");
        if faults.is_empty() && !self.memory.alerts.contains_key(&key) {
            return Ok(());
        }
        self.record(
            &key,
            !faults.is_empty(),
            &format!("{name}: device alert"),
            &faults.join(", "),
            now,
        )
    }
    fn persist(&self, memory: &Memory) -> Result<()> {
        config::save_private_bounded(&self.path, memory, 65536)
    }
    pub fn next_delay(&self, now: i64) -> Option<u64> {
        self.memory
            .alerts
            .iter()
            .flat_map(|(key, a)| {
                self.config.channels.iter().filter_map(move |c| {
                    if a.delivered.get(&c.name) == Some(&fingerprint(c, a)) {
                        None
                    } else {
                        Some(
                            self.retry_at
                                .get(&(key.clone(), c.name.clone()))
                                .copied()
                                .unwrap_or(now)
                                .max(self.next_attempt)
                                .saturating_sub(now)
                                .max(0) as u64,
                        )
                    }
                })
            })
            .min()
    }
    pub fn deliver(&mut self, now: i64) -> Result<usize> {
        if now < self.next_attempt {
            return Ok(0);
        }
        self.next_attempt = now + 30;
        let mut sent = 0;
        // Send at most one item per tick. This bounds device-control latency,
        // even when a channel is unavailable or a queue fills after downtime.
        for (key, alert) in &self.memory.alerts {
            for channel in &self.config.channels {
                let expected = fingerprint(channel, alert);
                let route = (key.clone(), channel.name.clone());
                if self.retry_at.get(&route).is_some_and(|at| now < *at)
                    || alert.delivered.get(&channel.name) == Some(&expected)
                {
                    continue;
                }
                self.next_attempt = now + 1;
                if let Err(e) = send(
                    channel,
                    key,
                    alert,
                    Duration::from_secs(self.config.timeout_seconds),
                ) {
                    self.retry_at.insert(route, now + 30);
                    return Err(e);
                }
                self.retry_at.remove(&route);
                let mut next = self.memory.clone();
                let saved = next
                    .alerts
                    .get_mut(key)
                    .ok_or_else(|| error("Missing alert after delivery"))?;
                saved
                    .delivered
                    .retain(|name, _| self.config.channels.iter().any(|c| c.name == *name));
                saved.delivered.insert(channel.name.clone(), expected);
                self.persist(&next)?;
                self.memory = next;
                sent += 1;
                self.next_attempt = now + 1;
                return Ok(sent);
            }
        }
        Ok(sent)
    }
    pub fn companion_list(&self, name: &str) -> Value {
        json!({"alerts":self.memory.alerts.iter().filter(|(_,a)|a.companion_receipts.get(name)!=Some(&revision(a))).map(|(key,a)|json!({"key":key,"active":a.active,"title":a.title,"message":a.message,"revision":revision(a)})).collect::<Vec<_>>()})
    }
    pub fn acknowledge(&mut self, name: &str, args: &Value) -> Result<Value> {
        if !valid_key(name) || args.as_object().is_none_or(|o| o.len() != 2) {
            return Err(error("Invalid companion acknowledgement"));
        }
        let key = args["key"]
            .as_str()
            .ok_or_else(|| error("Expected alert key"))?;
        let mut next = self.memory.clone();
        let a = next
            .alerts
            .get_mut(key)
            .ok_or_else(|| error("Unknown alert"))?;
        let rev = revision(a);
        if args["revision"] != rev {
            return Err(error("Alert changed; fetch current revision"));
        }
        if a.companion_receipts.len() >= 8 && !a.companion_receipts.contains_key(name) {
            return Err(error("Too many companion receipts"));
        }
        a.companion_receipts.insert(name.into(), rev);
        self.persist(&next)?;
        self.memory = next;
        Ok(json!({"acknowledged":true}))
    }
    pub fn list(&self) -> Value {
        let alerts:Vec<_>=self.memory.alerts.iter().map(|(key,a)|json!({"key":key,"active":a.active,"title":a.title,"message":a.message,"updated_at":a.updated_at,"pending_channels":self.config.channels.iter().filter(|c|a.delivered.get(&c.name)!=Some(&fingerprint(c,a))).map(|c|&c.name).collect::<Vec<_>>()})).collect();
        json!({"alerts":alerts,"delivery_channels":self.config.channels.len()})
    }
}
fn valid_key(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 96
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}
fn fingerprint(channel: &Channel, alert: &Alert) -> String {
    let digest = Sha256::digest(format!(
        "{}\0{}\0{}\0{}\0{}\0{}",
        channel.url,
        channel.push_token.as_deref().unwrap_or_default(),
        channel.authorization.as_deref().unwrap_or_default(),
        alert.active,
        alert.title,
        alert.message
    ));
    let mut hash = Sha256::new();
    hash.update(digest);
    hash.update(channel.registration_info.to_string());
    hash.finalize().iter().map(|b| format!("{b:02x}")).collect()
}
fn send(channel: &Channel, key: &str, alert: &Alert, timeout: Duration) -> Result<()> {
    let mut payload = json!({"title":alert.title,"message":if alert.active {alert.message.as_str()} else {"clear_notification"},"data":{"tag":key,"alert_once":true}});
    if let Some(token) = &channel.push_token {
        payload["push_token"] = json!(token);
        payload["registration_info"] = channel.registration_info.clone();
    } else {
        payload["key"] = json!(key);
        payload["active"] = json!(alert.active);
        payload["updated_at"] = json!(alert.updated_at);
    }
    let mut headers = vec![("Content-Type", "application/json")];
    if let Some(authorization) = channel.authorization.as_deref() {
        headers.push(("Authorization", authorization));
    }
    // Never include server response text in errors; it may echo credentials.
    web::request(
        &channel.url,
        "POST",
        &headers,
        &serde_json::to_vec(&payload)?,
        timeout,
    )
    .map_err(|_| {
        error(format!(
            "Notification delivery failed for {}; pending retry retained",
            channel.name
        ))
    })?;
    Ok(())
}
impl Adapter for Manager {
    const TOOLS: &'static [Tool] = &[];
    const PUBLIC_TOOLS: &'static [Tool] = &[Tool {
        name: "alert_list",
        description: "Read active and recovered local alert cards and pending delivery channels; no secrets or sends.",
        input: Input::Empty,
        hints: Hints::READ,
    }];
    const SUPPORT: Support = Support::NONE;
    fn open(path: &Path) -> Result<Self> {
        Self::open(path)
    }
    fn entities<'a>(&'a self, _: &mut impl FnMut(Entity<'a>)) {}
    fn execute(&mut self, name: &str, _: &Value) -> Result<Value> {
        if name == "alert_list" {
            Ok(self.list())
        } else {
            Err(error("Unknown alert operation"))
        }
    }
}

fn revision(a: &Alert) -> String {
    Sha256::digest(format!(
        "{}\0{}\0{}\0{}",
        a.active, a.title, a.message, a.updated_at
    ))
    .iter()
    .map(|b| format!("{b:02x}"))
    .collect()
}
