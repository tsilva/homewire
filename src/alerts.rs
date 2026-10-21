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
    path::{Path, PathBuf},
    time::Duration,
};

const LIMIT: usize = 64;
#[derive(Default, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Delivery {
    #[default]
    Webhook,
    Telegram,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Channel {
    pub name: String,
    pub url: String,
    #[serde(default)]
    pub delivery: Delivery,
    #[serde(default)]
    pub chat_id: Option<String>,
    /// Empty means all alert keys; otherwise exact bounded keys only.
    #[serde(default)]
    pub keys: Vec<String>,
    #[serde(default)]
    pub push_token: Option<String>,
    #[serde(default)]
    pub registration_info: Value,
    #[serde(default)]
    pub authorization: Option<String>,
}
impl Channel {
    fn accepts(&self, key: &str) -> bool {
        self.keys.is_empty() || self.keys.iter().any(|k| k == key)
    }
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
    _lock: crate::config::FileLock,
    next_attempt: i64,
    retry_at: BTreeMap<(String, String), i64>,
}
impl Manager {
    pub fn open(path: &Path) -> Result<Self> {
        Self::from_source(&crate::config::Source::file(path))
    }
    pub fn from_source(source: &crate::config::Source<'_>) -> Result<Self> {
        let path = source.path();
        let config: Config = source.decode()?;
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
            if channel.keys.len() > LIMIT
                || channel.keys.iter().any(|key| !valid_key(key))
                || channel
                    .keys
                    .iter()
                    .enumerate()
                    .any(|(i, key)| channel.keys[..i].contains(key))
            {
                return Err(error("Invalid notification key filter"));
            }
            if channel.delivery == Delivery::Telegram {
                let token = channel
                    .url
                    .strip_prefix("https://api.telegram.org/bot")
                    .and_then(|s| s.strip_suffix("/sendMessage"));
                let valid_token =
                    token
                        .and_then(|s| s.split_once(':'))
                        .is_some_and(|(id, secret)| {
                            !id.is_empty()
                                && id.bytes().all(|b| b.is_ascii_digit())
                                && !secret.is_empty()
                                && secret
                                    .bytes()
                                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
                        });
                if !valid_token
                    || !channel.chat_id.as_deref().is_some_and(valid_chat_id)
                    || channel.push_token.is_some()
                    || channel.authorization.is_some()
                    || !channel.registration_info.is_null()
                {
                    return Err(error("Invalid Telegram notification configuration"));
                }
            } else if channel.chat_id.is_some() {
                return Err(error("chat_id requires Telegram delivery"));
            }
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
                .filter(|(key, a)| {
                    !a.active
                        && self.config.channels.iter().all(|c| {
                            !c.accepts(key) || a.delivered.get(&c.name) == Some(&fingerprint(c, a))
                        })
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
                    if !c.accepts(key) || a.delivered.get(&c.name) == Some(&fingerprint(c, a)) {
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
                if !channel.accepts(key)
                    || self.retry_at.get(&route).is_some_and(|at| now < *at)
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
        let alerts:Vec<_>=self.memory.alerts.iter().map(|(key,a)|json!({"key":key,"active":a.active,"title":a.title,"message":a.message,"updated_at":a.updated_at,"pending_channels":self.config.channels.iter().filter(|c|c.accepts(key) && a.delivered.get(&c.name)!=Some(&fingerprint(c,a))).map(|c|&c.name).collect::<Vec<_>>()})).collect();
        json!({"alerts":alerts,"delivery_channels":self.config.channels.len()})
    }
}
fn valid_key(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 96
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}
fn valid_chat_id(s: &str) -> bool {
    let digits = s.strip_prefix('-').unwrap_or(s);
    (!digits.is_empty() && digits.len() <= 20 && digits.bytes().all(|b| b.is_ascii_digit()))
        || s.strip_prefix('@').is_some_and(|name| {
            !name.is_empty()
                && name.len() <= 32
                && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
        })
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
    // Preserve webhook acknowledgement fingerprints across this upgrade.
    if channel.delivery == Delivery::Telegram {
        hash.update(b"telegram\0");
        hash.update(channel.chat_id.as_deref().unwrap_or_default());
    }
    hash.finalize().iter().map(|b| format!("{b:02x}")).collect()
}
fn send(channel: &Channel, key: &str, alert: &Alert, timeout: Duration) -> Result<()> {
    if channel.delivery == Delivery::Telegram {
        return send_telegram(channel, alert, timeout);
    }
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
fn telegram_payload(channel: &Channel, alert: &Alert) -> Value {
    let text = if !alert.active {
        format!(
            "Resolved: {}\nThe reported device fault has cleared.",
            alert.title
        )
    } else if alert.message == "tank_full" {
        "Dehumidifier tank full\nPlease empty the water tank.".into()
    } else {
        format!("{}\n{}", alert.title, alert.message)
    };
    json!({"chat_id":channel.chat_id,"text":text})
}
fn telegram_accepted(body: &[u8]) -> bool {
    serde_json::from_slice::<Value>(body)
        .ok()
        .is_some_and(|response| {
            response["ok"] == true && response["result"]["message_id"].as_i64().is_some()
        })
}
fn send_telegram(channel: &Channel, alert: &Alert, timeout: Duration) -> Result<()> {
    let failed = || {
        error(format!(
            "Notification delivery failed for {}; pending retry retained",
            channel.name
        ))
    };
    let response = web::request(
        &channel.url,
        "POST",
        &[("Content-Type", "application/json")],
        &serde_json::to_vec(&telegram_payload(channel, alert))?,
        timeout,
    )
    .map_err(|_| failed())?;
    if !telegram_accepted(&response.body) {
        return Err(failed());
    }
    Ok(())
}
impl Adapter for Manager {
    const CONFIG_SECTION: &'static str = "services";
    const TOOLS: &'static [Tool] = &[];
    const PUBLIC_TOOLS: &'static [Tool] = &[Tool {
        name: "alert_list",
        description: "Read active and recovered local alert cards and pending delivery channels; no secrets or sends.",
        input: Input::Empty,
        hints: Hints::READ,
    }];
    const SUPPORT: Support = Support::NONE;
    fn from_source(source: &crate::config::Source<'_>) -> Result<Self> {
        Self::from_source(source)
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

#[cfg(test)]
mod telegram_tests {
    use super::*;
    use std::io::{BufRead, BufReader, Read, Write};

    fn channel(url: &str) -> Channel {
        serde_json::from_value(json!({"name":"telegram","url":url,"delivery":"telegram","chat_id":"-100123456789","keys":["meaco.meaco"]})).expect("channel")
    }
    fn alert(active: bool) -> Alert {
        Alert {
            active,
            title: "Dehumidifier".into(),
            message: if active { "tank_full" } else { "" }.into(),
            updated_at: 100,
            delivered: BTreeMap::new(),
            companion_receipts: BTreeMap::new(),
        }
    }
    #[test]
    fn payload_is_plain_text_and_recovery_is_explicit() {
        let c = channel("https://api.telegram.org/bot123:synthetic/sendMessage");
        assert_eq!(
            telegram_payload(&c, &alert(true))["text"],
            "Dehumidifier tank full\nPlease empty the water tank."
        );
        assert_eq!(
            telegram_payload(&c, &alert(true))["chat_id"],
            "-100123456789"
        );
        assert!(
            telegram_payload(&c, &alert(false))["text"]
                .as_str()
                .expect("text")
                .starts_with("Resolved:")
        );
        assert!(!c.accepts("schedule.meaco"));
        assert!(c.accepts("meaco.meaco"));
        assert!(valid_chat_id("123"));
        assert!(valid_chat_id("@example_alerts"));
        assert!(!valid_chat_id("-"));
        assert!(!valid_chat_id("@bad/name"));
    }
    #[test]
    fn telegram_requires_api_success_and_message_receipt() {
        assert!(telegram_accepted(
            br#"{"ok":true,"result":{"message_id":42}}"#
        ));
        for body in [
            br#"{"ok":false,"description":"SECRET"}"#.as_slice(),
            br#"{"ok":true}"#,
            b"invalid",
        ] {
            assert!(!telegram_accepted(body));
        }
    }
    #[test]
    fn failed_send_retries_and_success_survives_restart() -> Result<()> {
        let root = std::env::temp_dir().join(format!("homewire-telegram-{}", getrandom_id()?));
        std::fs::create_dir(&root)?;
        let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        let url = format!("http://{}/sendMessage", listener.local_addr()?);
        let worker = std::thread::spawn(move || -> Result<()> {
            for response in [
                r#"{"ok":false,"description":"SECRET"}"#,
                r#"{"ok":true,"result":{"message_id":1}}"#,
                r#"{"ok":true,"result":{"message_id":2}}"#,
            ] {
                let (mut socket, _) = listener.accept()?;
                socket.set_read_timeout(Some(Duration::from_secs(3)))?;
                let mut reader = BufReader::new(&mut socket);
                let mut line = String::new();
                let mut length = 0;
                loop {
                    line.clear();
                    reader.read_line(&mut line)?;
                    if line == "\r\n" {
                        break;
                    }
                    if let Some(n) = line.strip_prefix("Content-Length: ") {
                        length = n.trim().parse::<usize>()?;
                    }
                }
                let mut body = vec![0; length];
                reader.read_exact(&mut body)?;
                assert_eq!(
                    serde_json::from_slice::<Value>(&body)?["chat_id"],
                    "-100123456789"
                );
                drop(reader);
                write!(
                    socket,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    response.len(),
                    response
                )?;
            }
            Ok(())
        });
        let path = root.join("alerts.json");
        // The local mock is permitted only as a webhook in saved config;
        // production validation pins Telegram delivery to api.telegram.org.
        config::save_private(
            &path,
            &json!({"channels":[{"name":"telegram","url":url,"keys":["meaco.meaco"]}]}),
        )?;
        let mut manager = Manager::open(&path)?;
        manager.config.channels[0] = channel(&url);
        manager.record("other", true, "Other", "Not routed", 100)?;
        assert_eq!(manager.next_delay(100), None);
        manager.reconcile("meaco", "meaco", &["tank_full".into()], 100)?;
        let e = manager.deliver(100).expect_err("API failure");
        assert!(!e.to_string().contains("SECRET"));
        assert_eq!(manager.next_delay(101), Some(29));
        assert_eq!(manager.deliver(130)?, 1);
        drop(manager);
        let mut manager = Manager::open(&path)?;
        manager.config.channels[0] = channel(&url);
        assert_eq!(manager.next_delay(131), None);
        manager.reconcile("meaco", "meaco", &["tank_full".into()], 131)?;
        assert_eq!(manager.deliver(131)?, 0);
        manager.reconcile("meaco", "meaco", &[], 132)?;
        assert_eq!(manager.deliver(132)?, 1);
        worker.join().expect("server")?;
        drop(manager);
        std::fs::remove_dir_all(root)?;
        Ok(())
    }
    fn getrandom_id() -> Result<u64> {
        let mut bytes = [0u8; 8];
        getrandom::getrandom(&mut bytes)?;
        Ok(u64::from_ne_bytes(bytes))
    }
}
