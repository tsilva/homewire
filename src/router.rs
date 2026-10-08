//! Archer AXE75 local RSA login, status, verified settings and client presence.
use crate::{
    Result,
    device::{Adapter, Tool},
    entity::{Capabilities, Command, Domain, Entity, Support},
    error,
    settings::Setting,
    web,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    fs::File,
    path::Path,
    time::{Duration, Instant},
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Tracker {
    pub name: String,
    pub mac: String,
    #[serde(default)]
    pub person: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub name: String,
    pub url: String,
    pub password: String,
    #[serde(default)]
    pub trackers: Vec<Tracker>,
    #[serde(default = "timeout")]
    pub timeout_seconds: u64,
    #[serde(default = "absence")]
    pub absence_seconds: u64,
    #[serde(default = "poll")]
    pub poll_seconds: u64,
}
fn poll() -> u64 {
    60
}
fn absence() -> u64 {
    180
}
fn timeout() -> u64 {
    8
}
pub struct Manager {
    config: Config,
    _lock: File,
    absent_since: Vec<Option<i64>>,
    next_poll: Instant,
}
struct Session<'a> {
    config: &'a Config,
    stok: String,
    cookie: String,
}
impl Session<'_> {
    fn login(config: &Config) -> Result<Session<'_>> {
        let referer = format!("{}/webpages/index.html", config.url);
        let headers = [
            ("Content-Type", "application/x-www-form-urlencoded"),
            ("Referer", referer.as_str()),
        ];
        let keys = web::request(
            &format!(
                "{}/cgi-bin/luci/;stok=/login?form=keys&operation=read",
                config.url
            ),
            "POST",
            &headers,
            b"",
            Duration::from_secs(config.timeout_seconds),
        )?;
        let value: Value = serde_json::from_slice(&keys.body)
            .map_err(|_| error("Invalid router login response"))?;
        let n = value["data"]["password"][0]
            .as_str()
            .filter(|s| s.len() == 512)
            .ok_or_else(|| error("Router does not use the supported V1_11 RSA protocol"))?;
        let e = value["data"]["password"][1]
            .as_str()
            .filter(|s| s.len() <= 8)
            .ok_or_else(|| error("Missing router RSA exponent"))?;
        let key = rsa::RsaPublicKey::new(
            rsa::BigUint::parse_bytes(n.as_bytes(), 16)
                .ok_or_else(|| error("Invalid router RSA modulus"))?,
            rsa::BigUint::parse_bytes(e.as_bytes(), 16)
                .ok_or_else(|| error("Invalid router RSA exponent"))?,
        )?;
        let ciphertext = key.encrypt(
            &mut rand_core::OsRng,
            rsa::Pkcs1v15Encrypt,
            config.password.as_bytes(),
        )?;
        let encrypted: String = ciphertext.iter().map(|b| format!("{b:02x}")).collect();
        let response = web::request(
            &format!("{}/cgi-bin/luci/;stok=/login?form=login", config.url),
            "POST",
            &headers,
            format!("operation=login&password={encrypted}").as_bytes(),
            Duration::from_secs(config.timeout_seconds),
        )?;
        let value: Value = serde_json::from_slice(&response.body)
            .map_err(|_| error("Invalid router authentication response"))?;
        if value["success"] != true {
            return Err(error("Router rejected authentication"));
        }
        let stok = value["data"]["stok"]
            .as_str()
            .filter(|s| {
                !s.is_empty() && s.len() <= 128 && s.bytes().all(|b| b.is_ascii_alphanumeric())
            })
            .ok_or_else(|| error("Invalid router session token"))?
            .to_owned();
        Ok(Session {
            config,
            stok,
            cookie: response.cookies,
        })
    }
    fn call(&self, path: &str, operation: &str, mut params: Value) -> Result<Value> {
        params["operation"] = json!(operation);
        let separator = if path.contains('?') { '&' } else { '?' };
        let url = format!(
            "{}/cgi-bin/luci/;stok={}/{path}{separator}operation={operation}",
            self.config.url, self.stok
        );
        let response = web::request(
            &url,
            "POST",
            &[
                ("Content-Type", "application/json"),
                ("Cookie", &self.cookie),
                (
                    "Referer",
                    &format!("{}/webpages/index.html", self.config.url),
                ),
                ("Origin", &self.config.url),
            ],
            &serde_json::to_vec(&params)?,
            Duration::from_secs(self.config.timeout_seconds),
        )?;
        let mut value: Value =
            serde_json::from_slice(&response.body).map_err(|_| error("Invalid router response"))?;
        if value["success"] != true || value.get("data").is_none() {
            return Err(error("Router request rejected"));
        }
        Ok(value["data"].take())
    }
}
impl Drop for Session<'_> {
    fn drop(&mut self) {
        let _ = self.call("admin/system?form=logout", "write", json!({}));
    }
}
impl Manager {
    pub fn open(path: &Path) -> Result<Self> {
        let config: Config = crate::media::load(path)?;
        let url = web::Url::parse(&config.url)?;
        if url.path != "/"
            || config.url.ends_with('/')
            || config.password.is_empty()
            || config.password.len() > 125
            || !(1..=30).contains(&config.timeout_seconds)
            || !(30..=3600).contains(&config.absence_seconds)
            || !(15..=900).contains(&config.poll_seconds)
            || !crate::config::valid_unique_labels(std::iter::once(config.name.as_str()), 32, b"-_")
            || config.trackers.len() > 64
            || config.trackers.iter().enumerate().any(|(i, t)| {
                !crate::config::valid_unique_labels(std::iter::once(t.name.as_str()), 32, b"-_")
                    || config.trackers[..i].iter().any(|p| p.name == t.name)
            })
            || config
                .trackers
                .iter()
                .any(|t| normalize_mac(&t.mac).is_none())
        {
            return Err(error("Invalid router configuration"));
        }
        Ok(Self {
            next_poll: Instant::now(),
            config,
            _lock: crate::config::lock_configuration(path)?,
            absent_since: vec![None; 64],
        })
    }
    fn status(&mut self) -> Result<Value> {
        let result = self.fresh_status();
        self.next_poll = Instant::now() + Duration::from_secs(self.config.poll_seconds);
        if result.is_err() {
            self.absent_since.fill(None);
        }
        result
    }
    fn fresh_status(&mut self) -> Result<Value> {
        let session = Session::login(&self.config)?;
        let firmware = session.call("admin/firmware?form=upgrade", "read", json!({}))?;
        let model = firmware["model"].as_str().unwrap_or_default();
        if !model.contains("AXE75") {
            return Err(error("Router model does not match Archer AXE75"));
        }
        let data = session.call("admin/status?form=all", "read", json!({}))?;
        let mut result = json!({"model":model,"firmware":firmware["firmware_version"],"cpu_percent":percent(&data["cpu_usage"]),"memory_percent":percent(&data["mem_usage"]),"wan_ip":data["wan_ipv4_ipaddr"],"lan_ip":data["lan_ipv4_ipaddr"],"connection_type":data["conn_type"],"dhcp":flag(&data["lan_ipv4_dhcp_enable"])});
        let mut clients = Vec::new();
        for (group, key) in [
            ("wired", "access_devices_wired"),
            ("wifi", "access_devices_wireless_host"),
            ("guest", "access_devices_wireless_guest"),
            ("iot", "access_devices_wireless_iot"),
        ] {
            let devices = data[key].as_array();
            result[format!("{group}_clients")] = devices.map_or(Value::Null, |v| json!(v.len()));
            if let Some(devices) = devices {
                if devices.len() + clients.len() > 128 {
                    return Err(error("Router client list exceeds 128"));
                }
                for d in devices {
                    clients.push(json!({"mac":d["macaddr"],"ip":d["ipaddr"],"name":d["hostname"],"network":group}));
                }
            }
        }
        // Missing lists and request failures never establish absence.
        let presence_known = [
            "access_devices_wired",
            "access_devices_wireless_host",
            "access_devices_wireless_guest",
            "access_devices_wireless_iot",
        ]
        .iter()
        .all(|k| data[*k].is_array());
        let mut nobody = self
            .config
            .trackers
            .iter()
            .any(|t| t.person)
            .then_some(true);
        for (index, tracker) in self.config.trackers.iter().enumerate() {
            let found = clients
                .iter()
                .any(|c| c["mac"].as_str().and_then(normalize_mac) == normalize_mac(&tracker.mac));
            let now = chrono::Utc::now().timestamp();
            let present = if found {
                self.absent_since[index] = None;
                Some(true)
            } else if presence_known {
                let since = self.absent_since[index].get_or_insert(now);
                (now.saturating_sub(*since) >= self.config.absence_seconds as i64).then_some(false)
            } else {
                self.absent_since[index] = None;
                None
            };
            result[PRESENCE[index]] = json!(present);
            if tracker.person {
                nobody = match (nobody, present) {
                    (Some(false), _) | (_, Some(true)) => Some(false),
                    (Some(true), Some(false)) => Some(true),
                    _ => None,
                };
            }
        }
        result["nobody_home"] = json!(nobody);
        for (setting, _, field) in WIFI {
            result[*setting] = json!(flag(&data[*field]));
        }
        result["total_clients"] = json!(clients.len());
        result["wan_ipv6_enabled"] = json!(flag(&data["wan_ipv6_enable"]));
        result["wan_ipv6_address"] = data["wan_ipv6_ipaddr"].clone();
        for (name, path) in [
            ("openvpn", "admin/openvpn?form=config"),
            ("pptp", "admin/pptpd?form=config"),
        ] {
            result[name] = session
                .call(path, "read", json!({}))
                .ok()
                .and_then(|v| flag(&v["enabled"]))
                .map_or(Value::Null, |v| json!(v));
            let counts = session
                .call("admin/vpnconn?form=config", "list", json!({"vpntype":name}))
                .ok()
                .and_then(|v| v.as_array().map(Vec::len));
            result[format!("{name}_clients")] = json!(counts);
        }
        result["clients"] = json!(clients);
        Ok(result)
    }
    fn configure(&self, settings: &Value) -> Result<Value> {
        crate::settings::validate(SETTINGS, settings)?;
        if settings.get("reboot").is_some() && settings.as_object().is_some_and(|o| o.len() != 1) {
            return Err(error("Router reboot must be requested alone"));
        }
        let session = Session::login(&self.config)?;
        let firmware = session.call("admin/firmware?form=upgrade", "read", json!({}))?;
        if !firmware["model"]
            .as_str()
            .is_some_and(|s| s.contains("AXE75"))
        {
            return Err(error("Router identity mismatch"));
        }
        // Prepare every requested change before sending any writes.
        let mut changes = Vec::new();
        for (name, value) in settings
            .as_object()
            .ok_or_else(|| error("Expected settings"))?
        {
            if name == "reboot" {
                session.call("admin/system?form=reboot", "write", json!({}))?;
                return Ok(
                    json!({"requested":true,"verified":false,"note":"Reboot is disruptive; reconnect before reading status"}),
                );
            }
            let (path, key) =
                if let Some((_, form, _)) = WIFI.iter().find(|(setting, _, _)| setting == name) {
                    (format!("admin/wireless?form={form}"), "enable")
                } else {
                    match name.as_str() {
                        "dhcp" => ("admin/dhcps?form=setting".into(), "enable"),
                        "openvpn" => ("admin/openvpn?form=config".into(), "enabled"),
                        "pptp" => ("admin/pptpd?form=config".into(), "enabled"),
                        _ => return Err(error("Unsupported router setting")),
                    }
                };
            let mut params = session.call(&path, "read", json!({}))?;
            if !params.is_object() || params.get(key).is_none() {
                return Err(error("Router does not report the requested setting"));
            }
            params[key] = json!(if value == true { "on" } else { "off" });
            if name == "dhcp" {
                params
                    .as_object_mut()
                    .ok_or_else(|| error("Missing DHCP settings"))?
                    .retain(|k, _| {
                        matches!(
                            k.as_str(),
                            "enable"
                                | "leasetime"
                                | "pri_dns"
                                | "snd_dns"
                                | "gateway"
                                | "ipaddr_start"
                                | "ipaddr_end"
                                | "domain"
                        )
                    });
            }
            changes.push((path, key, params, value == true));
        }
        for (path, key, params, expected) in changes {
            session.call(&path, "write", params)?;
            if flag(&session.call(&path, "read", json!({}))?[key]) != Some(expected) {
                return Err(error("Router setting verification failed; no retry"));
            }
        }
        Ok(json!({"verified":true}))
    }
}
fn percent(value: &Value) -> Option<f64> {
    let v = value
        .as_f64()
        .or_else(|| value.as_str().and_then(|s| s.parse().ok()))?;
    (v.is_finite() && (0.0..=1.0).contains(&v)).then_some(v * 100.0)
}
fn flag(value: &Value) -> Option<bool> {
    match value {
        Value::Bool(v) => Some(*v),
        Value::String(s) if matches!(s.as_str(), "on" | "off") => Some(s == "on"),
        _ => None,
    }
}
fn normalize_mac(mac: &str) -> Option<[u8; 6]> {
    let mut out = [0; 6];
    let mut count = 0;
    for b in mac.bytes().filter(|b| !matches!(b, b':' | b'-')) {
        let digit = match b {
            b'0'..=b'9' => b - b'0',
            b'a'..=b'f' => b - b'a' + 10,
            b'A'..=b'F' => b - b'A' + 10,
            _ => return None,
        };
        if count >= 12 {
            return None;
        }
        out[count / 2] = out[count / 2] * 16 + digit;
        count += 1;
    }
    (count == 12).then_some(out)
}
const WIFI: &[(&str, &str, &str)] = &[
    ("wifi_2g", "wireless_2g", "wireless_2g_enable"),
    ("wifi_5g", "wireless_5g", "wireless_5g_enable"),
    ("wifi_6g", "wireless_6g", "wireless_6g_enable"),
    ("guest_2g", "guest_2g", "guest_2g_enable"),
    ("guest_5g", "guest_5g", "guest_5g_enable"),
    ("guest_6g", "guest_6g", "guest_6g_enable"),
    ("iot_2g", "iot_2g", "iot_2g_enable"),
    ("iot_5g", "iot_5g", "iot_5g_enable"),
    ("iot_6g", "iot_6g", "iot_6g_enable"),
];
const SETTINGS: &[Setting] = &[
    Setting::boolean("wifi_2g"),
    Setting::boolean("wifi_5g"),
    Setting::boolean("wifi_6g"),
    Setting::boolean("guest_2g"),
    Setting::boolean("guest_5g"),
    Setting::boolean("guest_6g"),
    Setting::boolean("iot_2g"),
    Setting::boolean("iot_5g"),
    Setting::boolean("iot_6g"),
    Setting::boolean("dhcp"),
    Setting::boolean("openvpn"),
    Setting::boolean("pptp"),
    Setting::action("reboot"),
];
const PRESENCE: &[&str] = &[
    "presence_0",
    "presence_1",
    "presence_2",
    "presence_3",
    "presence_4",
    "presence_5",
    "presence_6",
    "presence_7",
    "presence_8",
    "presence_9",
    "presence_10",
    "presence_11",
    "presence_12",
    "presence_13",
    "presence_14",
    "presence_15",
    "presence_16",
    "presence_17",
    "presence_18",
    "presence_19",
    "presence_20",
    "presence_21",
    "presence_22",
    "presence_23",
    "presence_24",
    "presence_25",
    "presence_26",
    "presence_27",
    "presence_28",
    "presence_29",
    "presence_30",
    "presence_31",
    "presence_32",
    "presence_33",
    "presence_34",
    "presence_35",
    "presence_36",
    "presence_37",
    "presence_38",
    "presence_39",
    "presence_40",
    "presence_41",
    "presence_42",
    "presence_43",
    "presence_44",
    "presence_45",
    "presence_46",
    "presence_47",
    "presence_48",
    "presence_49",
    "presence_50",
    "presence_51",
    "presence_52",
    "presence_53",
    "presence_54",
    "presence_55",
    "presence_56",
    "presence_57",
    "presence_58",
    "presence_59",
    "presence_60",
    "presence_61",
    "presence_62",
    "presence_63",
];
impl Adapter for Manager {
    fn next_background(&self) -> Option<Instant> {
        Some(self.next_poll)
    }
    fn background_tick(&mut self) -> Result<()> {
        self.status().map(|_| ())
    }
    const TOOLS: &'static [Tool] = &[];
    const SUPPORT: Support = Support::READ.union(Support::CONFIGURE);
    fn open(path: &Path) -> Result<Self> {
        Self::open(path)
    }
    fn execute(&mut self, _: &str, _: &Value) -> Result<Value> {
        Err(error("Use standard entity tools for router"))
    }
    fn entities<'a>(&'a self, visit: &mut impl FnMut(Entity<'a>)) {
        let base = Entity::new(
            crate::device::Family::Router,
            0,
            Domain::Router,
            &self.config.name,
            "Archer AXE75",
            Capabilities::Read,
        );
        visit(base.configurable(SETTINGS));
        for (suffix, field, unit) in [
            ("_cpu", "cpu_percent", "%"),
            ("_memory", "memory_percent", "%"),
            ("_wan_ip", "wan_ip", ""),
            ("_lan_ip", "lan_ip", ""),
            ("_wired_clients", "wired_clients", "clients"),
            ("_wifi_clients", "wifi_clients", "clients"),
            ("_guest_clients", "guest_clients", "clients"),
            ("_iot_clients", "iot_clients", "clients"),
            ("_nobody_home", "nobody_home", ""),
        ] {
            visit(base.sensor(suffix, field, unit));
        }
        for (index, tracker) in self.config.trackers.iter().enumerate() {
            let mut e = Entity::new(
                crate::device::Family::Router,
                0,
                Domain::DeviceTracker,
                &tracker.name,
                "router presence",
                Capabilities::Read,
            );
            e.field = Some(PRESENCE[index]);
            e.device_name = Some(&self.config.name);
            visit(e);
        }
    }
    fn read_device(&mut self, index: usize) -> Result<Value> {
        if index != 0 {
            return Err(error("Unknown router"));
        }
        self.status()
    }
    fn control_device(&mut self, index: usize, command: Command, args: &Value) -> Result<Value> {
        if index != 0 || !matches!(command, Command::Configure) {
            return Err(error("Unsupported router command"));
        }
        self.configure(&args["settings"])
    }
}
