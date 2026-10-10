#[path = "support/mod.rs"]
mod support;
#[path = "support/tapo.rs"]
mod tapo;
use homewire::{
    Result,
    bulb::{self, Change, Config, Device, Manager, Status},
    fault,
    klap::{Client, hash},
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    net::{SocketAddr, TcpListener},
    path::PathBuf,
    thread,
    time::Duration,
};
use tapo::{read_request, respond};

const ID: &str = "syntheticL430C1";
fn info() -> Value {
    json!({"model":"L430C","device_id":ID,"device_on":true,"brightness":5,"hue":0,"saturation":0,"color_temp":2700,"color_temp_range":[2500,6500],"overheated":false})
}
fn status() -> Status {
    Status::from_info("bulb_1", ID, info()).expect("status")
}
fn change(v: Value) -> Change {
    serde_json::from_value(v).expect("change")
}
struct Fake {
    state: Status,
    sets: usize,
    reads: usize,
    reject: bool,
    ignore: bool,
    missing_after: bool,
}
impl Default for Fake {
    fn default() -> Self {
        Self {
            state: status(),
            sets: 0,
            reads: 0,
            reject: false,
            ignore: false,
            missing_after: false,
        }
    }
}
impl Device for Fake {
    fn read(&mut self) -> Result<Status> {
        self.reads += 1;
        if self.missing_after && self.sets > 0 {
            return Err(homewire::error("Unavailable"));
        }
        Ok(self.state.clone())
    }
    fn set(&mut self, c: &Change) -> Result<()> {
        self.sets += 1;
        if self.reject {
            return Err(homewire::error("Rejected"));
        }
        if !self.ignore {
            if let Some(v) = c.power {
                self.state.power = v;
            }
            if let Some(v) = c.brightness_percent {
                self.state.brightness_percent = v;
            }
            if let Some(v) = c.hue {
                self.state.hue = v;
                self.state.color_temperature_k = 0;
            }
            if let Some(v) = c.saturation_percent {
                self.state.saturation_percent = v;
            }
            if let Some(v) = c.color_temperature_k {
                self.state.color_temperature_k = v;
            }
        }
        Ok(())
    }
    fn settle(&mut self) -> Result<()> {
        Ok(())
    }
}
#[test]
fn absolute_controls_verify_and_repeated_settings_send_no_controls() -> Result<()> {
    let mut d = Fake::default();
    for settings in [
        json!({"power":false}),
        json!({"power":true,"brightness_percent":12}),
        json!({"hue":210,"saturation_percent":75}),
        json!({"color_temperature_k":3000}),
    ] {
        let c = change(settings);
        let n = d.sets;
        let result = bulb::apply(&mut d, &c)?;
        assert!(result.applied && c.matches(&result.status));
        assert_eq!(d.sets, n + 1);
        assert!(!bulb::apply(&mut d, &c)?.applied);
        assert_eq!(d.sets, n + 1);
    }
    Ok(())
}
#[test]
fn brightness_and_colour_never_implicitly_turn_on_an_off_bulb() -> Result<()> {
    let mut d = Fake::default();
    d.state.power = false;
    let result = bulb::apply(
        &mut d,
        &change(json!({"brightness_percent":25,"hue":120,"saturation_percent":0})),
    )?;
    assert!(!result.status.power);
    assert_eq!(result.status.color_temperature_k, 0);
    Ok(())
}
#[test]
fn rejected_unavailable_or_unverified_controls_are_never_retried() {
    for (reject, ignore, missing) in [
        (true, false, false),
        (false, true, false),
        (false, false, true),
    ] {
        let mut d = Fake {
            reject,
            ignore,
            missing_after: missing,
            ..Fake::default()
        };
        assert!(bulb::apply(&mut d, &change(json!({"power":false}))).is_err());
        assert_eq!(d.sets, 1);
    }
}
#[test]
fn invalid_settings_and_temperature_ranges_fail_before_controls() {
    for v in [
        json!({}),
        json!({"brightness_percent":0}),
        json!({"brightness_percent":101}),
        json!({"hue":361,"saturation_percent":50}),
        json!({"hue":20}),
        json!({"saturation_percent":20}),
        json!({"hue":20,"saturation_percent":101}),
        json!({"color_temperature_k":2499}),
        json!({"color_temperature_k":9001}),
        json!({"color_temperature_k":2700,"hue":20,"saturation_percent":50}),
    ] {
        let mut d = Fake::default();
        assert!(bulb::apply(&mut d, &change(v)).is_err());
        assert_eq!(d.sets, 0);
        assert_eq!(d.reads, 0);
    }
    assert!(serde_json::from_value::<Change>(json!({"toggle":true})).is_err());
    let mut d = Fake::default();
    d.state.temperature_range_k = [2700, 6000];
    assert!(bulb::apply(&mut d, &change(json!({"color_temperature_k":2500}))).is_err());
    assert_eq!(d.sets, 0);
}
#[test]
fn overheated_or_unknown_fault_status_allows_only_power_off() -> Result<()> {
    for hot in [Some(true), None] {
        let mut d = Fake::default();
        d.state.overheated = hot;
        assert!(bulb::apply(&mut d, &change(json!({"brightness_percent":10}))).is_err());
        assert_eq!(d.sets, 0);
        assert!(bulb::apply(&mut d, &change(json!({"power":false})))?.applied);
        assert_eq!(d.sets, 1);
    }
    Ok(())
}
#[test]
fn unknown_faults_remain_unknown_and_wrong_devices_are_rejected() -> Result<()> {
    let mut v = info();
    v.as_object_mut().expect("object").remove("overheated");
    assert_eq!(Status::from_info("bulb_1", ID, v)?.overheated, None);
    for (field, bad) in [
        ("model", json!("P110")),
        ("device_id", json!("anotherBulb")),
        ("brightness", json!(0)),
        ("brightness", json!(101)),
        ("hue", json!(361)),
        ("color_temp", json!(7000)),
        ("color_temp_range", json!([6500, 2500])),
        ("overheated", json!("false")),
    ] {
        let mut v = info();
        v[field] = bad;
        assert!(Status::from_info("bulb_1", ID, v).is_err());
    }
    let mut v = info();
    v.as_object_mut().expect("object").remove("device_on");
    assert!(Status::from_info("bulb_1", ID, v).is_err());
    Ok(())
}

struct PrivateConfig {
    _directory: support::Directory,
    path: PathBuf,
}
impl PrivateConfig {
    fn new(connect: Option<SocketAddr>) -> Result<Self> {
        let directory = support::Directory::new()?;
        let path = directory.0.join("bulbs.json");
        support::Directory::write_private(&path,json!({"bulbs":[{"name":"bulb_1","host":"192.0.2.20","device_id":ID,"auth_hash":"00".repeat(32),"connect":connect}],"settle_seconds":0}).to_string().as_bytes())?;
        Ok(Self {
            _directory: directory,
            path,
        })
    }
}
#[test]
fn config_keeps_secrets_private_and_enforces_bounds() -> Result<()> {
    let file = PrivateConfig::new(None)?;
    let original = fs::read(&file.path)?;
    let cfg = Config::load(&file.path)?;
    assert_eq!(cfg.bulbs[0].port, 80);
    for patch in [
        json!({"timeout_seconds":0}),
        json!({"settle_seconds":8}),
        json!({"poll_seconds":301}),
        json!({"extra":"secret"}),
    ] {
        let mut value: Value = serde_json::from_slice(&original)?;
        value
            .as_object_mut()
            .expect("object")
            .extend(patch.as_object().expect("object").clone());
        fs::write(&file.path, value.to_string())?;
        assert!(Config::load(&file.path).is_err());
    }
    for (field, v) in [
        ("name", json!("bad\r\nname")),
        ("auth_hash", json!("credential must stay private")),
        ("device_id", json!("")),
        ("port", json!(0)),
        ("connect", json!("127.0.0.1:0")),
    ] {
        let mut value: Value = serde_json::from_slice(&original)?;
        value["bulbs"][0][field] = v;
        fs::write(&file.path, value.to_string())?;
        let e = Config::load(&file.path).err().expect("error").to_string();
        assert!(!e.contains("credential must stay private"));
    }
    let mut duplicate: Value = serde_json::from_slice(&original)?;
    let duplicated = duplicate["bulbs"][0].clone();
    duplicate["bulbs"]
        .as_array_mut()
        .expect("array")
        .push(duplicated);
    fs::write(&file.path, duplicate.to_string())?;
    assert!(Config::load(&file.path).is_err());
    fs::write(&file.path, vec![b' '; 8193])?;
    assert!(Config::load(&file.path).is_err());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::write(&file.path, original)?;
        fs::set_permissions(&file.path, fs::Permissions::from_mode(0o644))?;
        assert!(Config::load(&file.path).is_err());
    }
    Ok(())
}
#[test]
fn overheating_debounces_persists_and_unknown_readings_never_clear_faults() -> Result<()> {
    let file = PrivateConfig::new(None)?;
    let state = file._directory.0.join("bulbs.state.json");
    let identity = "192.0.2.20:80:syntheticL430C1";
    let mut m = fault::Monitor::default();
    let hot = vec!["overheated".into()];
    assert!(
        m.observe_faults("bulb_1", identity, Some(&hot), 0)
            .is_none()
    );
    assert_eq!(
        m.observe_faults("bulb_1", identity, Some(&hot), 3)
            .expect("fault")
            .event,
        "fault"
    );
    m.save(&state)?;
    let mut restored = fault::Monitor::load(
        &state,
        &BTreeMap::from([("bulb_1".into(), identity.into())]),
    )?;
    assert!(
        restored
            .observe_faults("bulb_1", identity, Some(&hot), 5)
            .is_none()
    );
    assert!(
        restored
            .observe_faults("bulb_1", identity, None, 100)
            .is_none()
    );
    assert_eq!(restored.active_faults()["bulb_1"], hot);
    assert!(
        restored
            .observe_faults("bulb_1", identity, Some(&[]), 101)
            .is_none()
    );
    assert!(
        restored
            .observe_faults("bulb_1", identity, None, 125)
            .is_none()
    );
    assert!(
        restored
            .observe_faults("bulb_1", identity, Some(&[]), 130)
            .is_none()
    );
    assert!(
        restored
            .observe_faults("bulb_1", identity, Some(&[]), 159)
            .is_none()
    );
    assert_eq!(
        restored
            .observe_faults("bulb_1", identity, Some(&[]), 160)
            .expect("recovery")
            .event,
        "recovered"
    );
    assert!(
        fault::Monitor::load(
            &state,
            &BTreeMap::from([("bulb_1".into(), "changed-device".into())])
        )?
        .active_faults()
        .is_empty()
    );
    let manager = Manager::open(&file.path)?;
    assert_eq!(manager.monitor.active_faults()["bulb_1"], hot);
    assert!(Manager::open(&file.path).is_err());
    assert!(!manager.list().to_string().contains("auth_hash"));
    Ok(())
}

fn mock(
    methods: Vec<&'static str>,
) -> Result<(SocketAddr, thread::JoinHandle<Result<Vec<Value>>>)> {
    mock_state(methods, info())
}
fn mock_state(
    methods: Vec<&'static str>,
    initial: Value,
) -> Result<(SocketAddr, thread::JoinHandle<Result<Vec<Value>>>)> {
    mock_sessions(vec![(methods, None)], initial)
}
fn mock_sessions(
    sessions: Vec<(Vec<&'static str>, Option<usize>)>,
    initial: Value,
) -> Result<(SocketAddr, thread::JoinHandle<Result<Vec<Value>>>)> {
    tapo::mock_sessions(sessions, initial, None)
}
#[test]
fn real_protocol_controls_use_one_absolute_write_then_verify() -> Result<()> {
    let (address, worker) = mock(vec![
        "get_device_info",
        "set_device_info",
        "get_device_info",
    ])?;
    let file = PrivateConfig::new(Some(address))?;
    let manager = Manager::open(&file.path)?;
    let result = manager.apply(
        "bulb_1",
        &change(json!({"hue":120,"saturation_percent":75,"brightness_percent":10})),
    )?;
    assert!(result.applied);
    assert_eq!(result.status.hue, 120);
    assert_eq!(result.status.color_temperature_k, 0);
    let requests = worker.join().expect("worker")?;
    assert_eq!(
        requests[1]["params"],
        json!({"hue":120,"saturation":75,"brightness":10,"color_temp":0})
    );
    Ok(())
}
#[test]
fn wire_status_and_idempotent_apply_work_without_controls() -> Result<()> {
    let (address, worker) = mock(vec!["get_device_info"])?;
    let file = PrivateConfig::new(Some(address))?;
    let manager = Manager::open(&file.path)?;
    assert!(
        !manager
            .apply("bulb_1", &change(json!({"brightness_percent":5})))?
            .applied
    );
    assert!(manager.status("unknown").is_err());
    worker.join().expect("worker")?;
    Ok(())
}

#[test]
fn bulb_only_cli_and_mcp_need_no_other_device_configuration() -> Result<()> {
    use std::process::{Command, Stdio};
    for serve in [false, true] {
        let (address, worker) = mock(vec!["get_device_info"])?;
        let file = PrivateConfig::new(Some(address))?;
        let mut child = Command::new(env!("CARGO_BIN_EXE_homewire"))
            .args(["--bulbs", file.path.to_str().expect("path")])
            .args(if serve {
                vec!["serve", "--legacy-tools"]
            } else {
                vec!["bulb-status", "bulb_1"]
            })
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        if serve {
            let mut stdin = child.stdin.take().expect("stdin");
            for request in [
                json!({"jsonrpc":"2.0","id":1,"method":"initialize"}),
                json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
                json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
                json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"bulb_status","arguments":{"bulb":"bulb_1"}}}),
            ] {
                writeln!(stdin, "{request}")?;
            }
        }
        let output = child.wait_with_output()?;
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.stderr.is_empty());
        let values: Vec<Value> = String::from_utf8(output.stdout)?
            .lines()
            .map(serde_json::from_str)
            .collect::<std::result::Result<_, _>>()?;
        if serve {
            assert_eq!(values.len(), 3);
            assert_eq!(
                values[1]["result"]["tools"]
                    .as_array()
                    .expect("tools")
                    .len(),
                3
            );
            assert_eq!(
                values[2]["result"]["structuredContent"]["brightness_percent"],
                5
            );
        } else {
            assert_eq!(values.len(), 1);
            assert_eq!(values[0]["brightness_percent"], 5);
        }
        worker.join().expect("worker")?;
    }
    Ok(())
}
#[test]
fn wrong_handshake_hash_or_invalid_cookie_stops_authentication() -> Result<()> {
    for bad_cookie in [false, true] {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let address = listener.local_addr()?;
        let worker = thread::spawn(move || -> Result<()> {
            let (mut socket, _) = listener.accept()?;
            let (_, local, _) = read_request(&mut socket)?;
            let remote = [7; 16];
            let mut body = remote.to_vec();
            body.extend_from_slice(&hash(&[
                &local,
                &remote,
                if bad_cookie { &[0; 32] } else { &[1; 32] },
            ]));
            if bad_cookie {
                socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nSet-Cookie: TP_SESSIONID=unsafe value\r\n\r\n",body.len()).as_bytes())?;
                socket.write_all(&body)?;
            } else {
                respond(&mut socket, &body, true)?;
            }
            Ok(())
        });
        assert!(
            Client::connect(
                address,
                "192.0.2.20:80".into(),
                &[0; 32],
                Duration::from_secs(3)
            )
            .is_err()
        );
        worker.join().expect("worker")?;
    }
    Ok(())
}

fn strip_info() -> Value {
    let mut value = info();
    value["model"] = json!("L900");
    value["color_temp"] = json!(2500);
    value["color_temp_range"] = json!([2500, 9000]);
    value["lighting_effect"] = json!({"enable":0,"name":"station"});
    value["music_rhythm_enable"] = json!(false);
    value
}
fn strip_config(connect: Option<SocketAddr>) -> Result<PrivateConfig> {
    let file = PrivateConfig::new(connect)?;
    let mut value: Value = serde_json::from_slice(&fs::read(&file.path)?)?;
    value["bulbs"][0]["model"] = json!("L900");
    let lights = value
        .as_object_mut()
        .expect("config")
        .remove("bulbs")
        .expect("lights");
    value["lights"] = lights;
    fs::write(&file.path, value.to_string())?;
    Ok(file)
}
fn strip_fake() -> Fake {
    Fake {
        state: Status::from_model_info("led_strip", ID, bulb::Model::L900, strip_info())
            .expect("strip"),
        ..Fake::default()
    }
}

#[test]
fn l900_range_and_model_are_distinct_from_l430c() -> Result<()> {
    let strip = strip_fake();
    assert_eq!(strip.state.temperature_range_k, [2500, 9000]);
    assert_eq!(strip.state.effect_active, Some(false));
    assert!(Status::from_info("bulb", ID, strip_info()).is_err());
    assert!(Status::from_model_info("strip", ID, bulb::Model::L900, info()).is_err());
    let mut v = info();
    v["color_temp_range"] = json!([2500, 9000]);
    assert!(Status::from_info("bulb", ID, v).is_err());
    let mut d = Fake::default();
    assert!(bulb::apply(&mut d, &change(json!({"color_temperature_k":7000}))).is_err());
    assert_eq!(d.sets, 0);
    Ok(())
}
#[test]
fn l900_absolute_controls_support_full_temperature_range_and_verify() -> Result<()> {
    let mut d = strip_fake();
    for v in [
        json!({"color_temperature_k":9000}),
        json!({"hue":210,"saturation_percent":75,"brightness_percent":10}),
        json!({"power":false}),
        json!({"power":true,"color_temperature_k":2500}),
    ] {
        let c = change(v);
        let result = bulb::apply(&mut d, &c)?;
        assert!(result.applied && c.matches(&result.status));
        let count = d.sets;
        assert!(!bulb::apply(&mut d, &c)?.applied);
        assert_eq!(d.sets, count);
    }
    Ok(())
}
#[test]
fn l900_effects_and_music_mode_prevent_unverified_fixed_settings() -> Result<()> {
    for effects in [Some(true), None] {
        let mut d = strip_fake();
        d.state.effect_active = effects;
        for c in [
            json!({"brightness_percent":10}),
            json!({"hue":40,"saturation_percent":50}),
            json!({"color_temperature_k":7000}),
        ] {
            assert!(bulb::apply(&mut d, &change(c)).is_err());
            assert_eq!(d.sets, 0);
        }
        assert!(bulb::apply(&mut d, &change(json!({"power":false})))?.applied);
    }
    let mut v = strip_info();
    v["music_rhythm_enable"] = json!(true);
    assert_eq!(
        Status::from_model_info("strip", ID, bulb::Model::L900, v)?.effect_active,
        Some(true)
    );
    let mut v = strip_info();
    v["lighting_effect"]["enable"] = json!(1);
    assert_eq!(
        Status::from_model_info("strip", ID, bulb::Model::L900, v)?.effect_active,
        Some(true)
    );
    let mut v = strip_info();
    v.as_object_mut().expect("object").remove("lighting_effect");
    assert_eq!(
        Status::from_model_info("strip", ID, bulb::Model::L900, v)?.effect_active,
        None
    );
    for bad in [json!(2), json!("off"), Value::Null] {
        let mut v = strip_info();
        v["lighting_effect"]["enable"] = bad;
        assert!(Status::from_model_info("strip", ID, bulb::Model::L900, v).is_err());
    }
    Ok(())
}
#[test]
fn l900_config_requires_explicit_model_and_keeps_legacy_config_usable() -> Result<()> {
    let file = strip_config(None)?;
    let original = fs::read(&file.path)?;
    let config = Config::load(&file.path)?;
    assert_eq!(config.bulbs[0].model, bulb::Model::L900);
    let old = PrivateConfig::new(None)?;
    assert_eq!(Config::load(&old.path)?.bulbs[0].model, bulb::Model::L430C);
    let mut v: Value = serde_json::from_slice(&original)?;
    v["lights"][0]["model"] = json!("L920");
    fs::write(&file.path, v.to_string())?;
    assert!(Config::load(&file.path).is_err());
    let mut v: Value = serde_json::from_slice(&original)?;
    v["bulbs"] = v["lights"].clone();
    fs::write(&file.path, v.to_string())?;
    assert!(Config::load(&file.path).is_err());
    Ok(())
}
#[test]
fn l900_wire_control_verifies_white_above_the_bulb_range() -> Result<()> {
    let (address, worker) = mock_state(
        vec!["get_device_info", "set_device_info", "get_device_info"],
        strip_info(),
    )?;
    let file = strip_config(Some(address))?;
    let manager = Manager::open(&file.path)?;
    let result = manager.apply("bulb_1", &change(json!({"color_temperature_k":7000})))?;
    assert_eq!(result.status.model, bulb::Model::L900);
    assert_eq!(result.status.color_temperature_k, 7000);
    assert_eq!(
        worker.join().expect("worker")?[1]["params"],
        json!({"color_temp":7000})
    );
    Ok(())
}
#[test]
fn l900_fault_monitor_retains_reported_overheating_and_deduplicates() -> Result<()> {
    let file = strip_config(None)?;
    let mut m = Manager::open(&file.path)?;
    let identity = "192.0.2.20:80:syntheticL430C1:L900";
    let hot = vec!["overheated".into()];
    assert!(
        m.monitor
            .observe_faults("bulb_1", identity, Some(&hot), 0)
            .is_none()
    );
    assert_eq!(
        m.monitor
            .observe_faults("bulb_1", identity, Some(&hot), 3)
            .expect("fault")
            .faults,
        hot
    );
    m.monitor
        .save(&file._directory.0.join("bulbs.state.json"))?;
    drop(m);
    let mut m = Manager::open(&file.path)?;
    assert_eq!(m.monitor.active_faults()["bulb_1"], hot);
    assert!(
        m.monitor
            .observe_faults("bulb_1", identity, None, 30)
            .is_none()
    );
    assert!(
        m.monitor
            .observe_faults("bulb_1", identity, Some(&hot), 100)
            .is_none()
    );
    drop(m);
    // Changing expected model invalidates the saved interpretation for this endpoint.
    let mut v: Value = serde_json::from_slice(&fs::read(&file.path)?)?;
    v["lights"][0]["model"] = json!("L430C");
    fs::write(&file.path, v.to_string())?;
    assert!(
        Manager::open(&file.path)?
            .monitor
            .active_faults()
            .is_empty()
    );
    Ok(())
}
#[test]
fn l900_cli_and_light_mcp_use_shared_adapter_without_duplicate_tools() -> Result<()> {
    use std::process::{Command, Stdio};
    for serve in [false, true] {
        let (address, worker) = mock_state(vec!["get_device_info"], strip_info())?;
        let file = strip_config(Some(address))?;
        let mut child = Command::new(env!("CARGO_BIN_EXE_homewire"))
            .args(["--lights", file.path.to_str().expect("path")])
            .args(if serve {
                vec!["serve", "--legacy-tools"]
            } else {
                vec!["light-status", "bulb_1"]
            })
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        if serve {
            let mut stdin = child.stdin.take().expect("stdin");
            for request in [
                json!({"jsonrpc":"2.0","id":1,"method":"initialize"}),
                json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
                json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"light_status","arguments":{"light":"bulb_1"}}}),
            ] {
                writeln!(stdin, "{request}")?;
            }
        }
        let output = child.wait_with_output()?;
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.stderr.is_empty());
        let values: Vec<Value> = String::from_utf8(output.stdout)?
            .lines()
            .map(serde_json::from_str)
            .collect::<std::result::Result<_, _>>()?;
        let status = if serve {
            let list = values[1]["result"]["tools"].as_array().expect("tools");
            assert_eq!(list.len(), 3);
            assert!(
                list.iter()
                    .all(|t| t["name"].as_str().expect("name").starts_with("light_"))
            );
            values[2]["result"]["structuredContent"].clone()
        } else {
            values[0].clone()
        };
        assert_eq!(status["light"], "bulb_1");
        assert_eq!(status["model"], "L900");
        assert!(status.get("bulb").is_none());
        worker.join().expect("worker")?;
    }
    Ok(())
}

#[test]
fn security_rejected_verification_refreshes_only_read_without_repeating_control() -> Result<()> {
    let (address, worker) = mock_sessions(
        vec![
            (
                vec!["get_device_info", "set_device_info", "get_device_info"],
                Some(2),
            ),
            (vec!["get_device_info"], None),
        ],
        info(),
    )?;
    let file = PrivateConfig::new(Some(address))?;
    let manager = Manager::open(&file.path)?;
    let result = manager.apply("bulb_1", &change(json!({"brightness_percent":6})))?;
    assert!(result.applied);
    assert_eq!(result.status.brightness_percent, 6);
    let requests = worker.join().expect("worker")?;
    assert_eq!(requests.len(), 4);
    assert_eq!(
        requests
            .iter()
            .filter(|r| r["method"] == "set_device_info")
            .count(),
        1
    );
    Ok(())
}
#[test]
fn security_rejected_control_is_never_replayed() -> Result<()> {
    let (address, worker) = mock_sessions(
        vec![(vec!["get_device_info", "set_device_info"], Some(1))],
        info(),
    )?;
    let file = PrivateConfig::new(Some(address))?;
    let manager = Manager::open(&file.path)?;
    let e = manager
        .apply("bulb_1", &change(json!({"brightness_percent":6})))
        .err()
        .expect("rejected");
    assert!(e.to_string().contains("Light control failed"));
    assert!(e.to_string().contains("403"));
    assert_eq!(worker.join().expect("worker")?.len(), 2);
    Ok(())
}
#[test]
fn security_rejected_reads_refresh_at_most_once() -> Result<()> {
    let (address, worker) = mock_sessions(
        vec![
            (vec!["get_device_info"], Some(0)),
            (vec!["get_device_info"], Some(0)),
        ],
        info(),
    )?;
    let file = PrivateConfig::new(Some(address))?;
    let manager = Manager::open(&file.path)?;
    assert!(manager.status("bulb_1").is_err());
    assert_eq!(worker.join().expect("worker")?.len(), 2);
    Ok(())
}
