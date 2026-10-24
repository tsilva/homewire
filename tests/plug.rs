#[path = "support/mod.rs"]
mod support;
#[path = "support/tapo.rs"]
mod tapo;
use homewire::{
    Result, fault,
    klap::Client,
    plug::{self, Config, Device, Energy, Manager, Model, Status},
};
use serde_json::{Value, json};
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    net::SocketAddr,
    path::PathBuf,
    process::{Command, Stdio},
    time::{Duration, Instant},
};
const ID: &str = "syntheticPlug1";
fn info(model: Model) -> Value {
    json!({"model":if model==Model::P100{"P100"}else{"P110"},"device_id":ID,"device_on":false,"power_protection_status":"normal","auto_off_status":"off","auto_off_remain_time":0})
}
fn state(model: Model) -> Status {
    Status::from_info("plug1", ID, model, info(model)).expect("fixture")
}
fn energy() -> (Value, Value) {
    (
        json!({"current_power":45259,"today_energy":49,"today_energy_mwh":48738,"month_energy":255,"month_energy_mwh":255437}),
        json!({"power_mw":45259,"voltage_mv":235440,"current_ma":193,"energy_wh":2317,"energy_mwh":2317816}),
    )
}
struct Fake {
    state: Status,
    sets: usize,
    reject: bool,
    ignore: bool,
    missing: bool,
    hot_after: bool,
}
impl Fake {
    fn new(model: Model) -> Self {
        Self {
            state: state(model),
            sets: 0,
            reject: false,
            ignore: false,
            missing: false,
            hot_after: false,
        }
    }
}
impl Device for Fake {
    fn read(&mut self) -> Result<Status> {
        if self.sets > 0 && self.missing {
            return Err(homewire::error("unavailable"));
        }
        let mut status = self.state.clone();
        if self.sets > 0 && self.hot_after {
            status.overloaded = Some(true);
        }
        Ok(status)
    }
    fn set_power(&mut self, power: bool) -> Result<()> {
        self.sets += 1;
        if self.reject {
            return Err(homewire::error("rejected"));
        }
        if !self.ignore {
            self.state.power = power;
        }
        Ok(())
    }
    fn settle(&mut self) -> Result<()> {
        Ok(())
    }
}
struct Private {
    _directory: support::Directory,
    path: PathBuf,
}
impl Private {
    fn new(model: Model, connect: Option<SocketAddr>) -> Result<Self> {
        let directory = support::Directory::new()?;
        let path = directory.0.join("plugs.json");
        let config = json!({"plugs":[{"name":"plug1","model":model,"host":"192.0.2.20","device_id":ID,"auth_hash":"00".repeat(32),"connect":connect}],"settle_seconds":0});
        Self::write(&path, &config)?;
        Ok(Self {
            _directory: directory,
            path,
        })
    }
    fn write(path: &std::path::Path, value: &Value) -> Result<()> {
        support::Directory::write_private(path, value.to_string().as_bytes())
    }
}
#[test]
fn plug_state_requires_identity_and_preserves_unknown_overload_and_timer() -> Result<()> {
    for model in [Model::P100, Model::P110] {
        let status = state(model);
        assert!(!status.power);
        assert_eq!(
            status.overloaded,
            if model == Model::P110 {
                Some(false)
            } else {
                None
            }
        );
        assert_eq!(status.auto_off_active, Some(false));
        assert!(status.energy.is_none());
    }
    let mut value = info(Model::P110);
    value["power_protection_status"] = json!("overloaded");
    assert_eq!(
        Status::from_info("plug1", ID, Model::P110, value.clone())?.overloaded,
        Some(true)
    );
    for unknown in [Value::Null, json!("unknown"), json!("future_status")] {
        value["power_protection_status"] = unknown;
        assert_eq!(
            Status::from_info("plug1", ID, Model::P110, value.clone())?.overloaded,
            None
        );
    }
    value["auto_off_status"] = json!("on");
    value["auto_off_remain_time"] = json!(120);
    let s = Status::from_info("plug1", ID, Model::P110, value.clone())?;
    assert_eq!(s.auto_off_active, Some(true));
    assert_eq!(s.auto_off_remaining_seconds, Some(120));
    value["device_on"] = Value::Null;
    assert!(Status::from_info("plug1", ID, Model::P110, value).is_err());
    assert!(Status::from_info("plug1", "wrong", Model::P110, info(Model::P110)).is_err());
    assert!(Status::from_info("plug1", ID, Model::P100, info(Model::P110)).is_err());
    Ok(())
}
#[test]
fn energy_preserves_units_and_prefers_firmware_milliwatt_hour_precision() -> Result<()> {
    let (usage, meter) = energy();
    let e = Energy::from_readings(usage, meter)?;
    assert_eq!(e.power_w, Some(45.259));
    assert_eq!(e.current_a, Some(0.193));
    assert_eq!(e.voltage_v, Some(235.44));
    assert_eq!(e.today_kwh, Some(0.048738));
    assert_eq!(e.month_kwh, Some(0.255437));
    assert_eq!(e.total_kwh, Some(2.317816));
    let old = Energy::from_readings(
        json!({"current_power":1200,"today_energy":40,"month_energy":300}),
        json!({}),
    )?;
    assert_eq!(old.power_w, Some(1.2));
    assert_eq!(old.today_kwh, Some(0.04));
    assert_eq!(old.month_kwh, Some(0.3));
    assert_eq!(old.voltage_v, None);
    assert_eq!(old.total_kwh, None);
    let zero = Energy::from_readings(json!({"current_power":0}), json!({}))?;
    assert_eq!(zero.power_w, Some(0.));
    assert_eq!(zero.current_a, None);
    Ok(())
}
#[test]
fn invalid_energy_never_becomes_zero_or_a_derived_fault() {
    for value in [json!(-1), json!("45259"), json!(1.2), json!(true)] {
        assert!(Energy::from_readings(json!({"current_power":value}), json!({})).is_err());
    }
    assert!(Energy::from_readings(json!({}), json!({})).is_err());
    assert!(Energy::from_readings(Value::Null, json!({})).is_err());
    assert!(Energy::from_readings(json!({"current_power":0}), json!({"voltage_mv":-1})).is_err());
}
#[test]
fn absolute_power_is_verified_and_identical_requests_send_no_commands() -> Result<()> {
    for model in [Model::P100, Model::P110] {
        let mut d = Fake::new(model);
        assert!(!plug::apply(&mut d, false)?.applied);
        assert_eq!(d.sets, 0);
        assert!(plug::apply(&mut d, true)?.applied);
        assert!(d.state.power);
        assert!(!plug::apply(&mut d, true)?.applied);
        assert_eq!(d.sets, 1);
        assert!(plug::apply(&mut d, false)?.applied);
        assert_eq!(d.sets, 2);
    }
    Ok(())
}
#[test]
fn p110_hot_or_unknown_overload_blocks_power_on_and_still_allows_off() -> Result<()> {
    for hot in [Some(true), None] {
        let mut d = Fake::new(Model::P110);
        d.state.overloaded = hot;
        assert!(plug::apply(&mut d, true).is_err());
        assert_eq!(d.sets, 0);
        d.state.power = true;
        assert!(plug::apply(&mut d, false)?.applied);
        assert_eq!(d.sets, 1);
    }
    Ok(())
}
#[test]
fn rejected_ignored_unavailable_and_new_fault_controls_are_never_replayed() {
    for mode in 0..4 {
        let mut d = Fake::new(Model::P110);
        d.reject = mode == 0;
        d.ignore = mode == 1;
        d.missing = mode == 2;
        d.hot_after = mode == 3;
        assert!(plug::apply(&mut d, true).is_err());
        assert_eq!(d.sets, 1);
    }
}
#[test]
fn private_config_has_bounds_and_exclusive_ownership() -> Result<()> {
    let file = Private::new(Model::P110, None)?;
    let base: Value = serde_json::from_slice(&fs::read(&file.path)?)?;
    for (key, value) in [
        ("name", json!("a\nheader")),
        ("device_id", json!("")),
        ("auth_hash", json!("private credential must not leak")),
        ("port", json!(0)),
        ("model", json!("P115")),
    ] {
        let mut config = base.clone();
        config["plugs"][0][key] = value;
        Private::write(&file.path, &config)?;
        let e = Config::load(&file.path).err().expect("invalid").to_string();
        assert!(!e.contains("private credential"));
    }
    for count in [0, 2, 9] {
        let mut config = base.clone();
        config["plugs"] = json!(vec![base["plugs"][0].clone(); count]);
        Private::write(&file.path, &config)?;
        assert!(Config::load(&file.path).is_err());
    }
    for (key, value) in [
        ("timeout_seconds", json!(0)),
        ("poll_seconds", json!(301)),
        ("settle_seconds", json!(8)),
    ] {
        let mut config = base.clone();
        config[key] = value;
        Private::write(&file.path, &config)?;
        assert!(Config::load(&file.path).is_err());
    }
    Private::write(&file.path, &base)?;
    let manager = Manager::open(&file.path)?;
    assert!(Manager::open(&file.path).is_err());
    assert!(!manager.list().to_string().contains("auth_hash"));
    assert!(manager.status("missing").is_err());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&file.path, fs::Permissions::from_mode(0o644))?;
        assert!(Config::load(&file.path).is_err());
    }
    Ok(())
}
#[test]
fn reported_overload_is_debounced_retained_and_recovered_without_guessing() -> Result<()> {
    let file = Private::new(Model::P110, None)?;
    let mut manager = Manager::open(&file.path)?;
    let identity = format!("192.0.2.20:80:{ID}:P110");
    let hot = vec!["overloaded".to_owned()];
    assert!(
        manager
            .monitor
            .observe_faults("plug1", &identity, Some(&hot), 0)
            .is_none()
    );
    assert!(
        manager
            .monitor
            .observe_faults("plug1", &identity, Some(&hot), 2)
            .is_none()
    );
    assert_eq!(
        manager
            .monitor
            .observe_faults("plug1", &identity, Some(&hot), 3)
            .expect("event")
            .event,
        "fault"
    );
    manager
        .monitor
        .save(&file.path.with_extension("state.json"))?;
    drop(manager);
    let mut manager = Manager::open(&file.path)?;
    assert_eq!(manager.monitor.active_faults()["plug1"], hot);
    assert!(
        manager
            .monitor
            .observe_faults("plug1", &identity, None, 20)
            .is_none()
    );
    assert_eq!(manager.monitor.active_faults()["plug1"], hot);
    assert!(
        manager
            .monitor
            .observe_faults("plug1", &identity, Some(&[]), 21)
            .is_none()
    );
    assert!(
        manager
            .monitor
            .observe_faults("plug1", &identity, Some(&[]), 50)
            .is_none()
    );
    assert_eq!(
        manager
            .monitor
            .observe_faults("plug1", &identity, Some(&[]), 51)
            .expect("recovery")
            .event,
        "recovered"
    );
    assert!(
        fault::Monitor::load(
            &file.path.with_extension("state.json"),
            &std::collections::BTreeMap::from([("plug1".into(), "changed identity".into())])
        )?
        .active_faults()
        .is_empty()
    );
    Ok(())
}
#[test]
fn wire_p100_controls_use_one_absolute_write_and_verify() -> Result<()> {
    let (address, worker) = tapo::mock_sessions(
        vec![(
            vec!["get_device_info", "set_device_info", "get_device_info"],
            None,
        )],
        info(Model::P100),
        None,
    )?;
    let file = Private::new(Model::P100, Some(address))?;
    let manager = Manager::open(&file.path)?;
    assert!(manager.apply("plug1", true)?.applied);
    let requests = worker.join().expect("worker")?;
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[1]["params"], json!({"device_on":true}));
    Ok(())
}

#[test]
fn standard_switch_control_preserves_one_write_and_fresh_verification() -> Result<()> {
    let (address, worker) = tapo::mock_sessions(
        vec![(
            vec!["get_device_info", "set_device_info", "get_device_info"],
            None,
        )],
        info(Model::P100),
        None,
    )?;
    let file = Private::new(Model::P100, Some(address))?;
    let mut devices = homewire::device::Devices {
        plugs: Some(Manager::open(&file.path)?),
        ..Default::default()
    };
    let result = homewire::entity::call(
        &mut devices,
        "switch_set",
        &json!({"entity":"switch.plug1","power":true}),
    )?;
    assert_eq!(result["result"]["applied"], true);
    assert_eq!(result["result"]["status"]["power"], true);
    let requests = worker.join().expect("worker")?;
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[1]["params"], json!({"device_on":true}));
    Ok(())
}

#[test]
fn p110_switch_and_sensor_entities_share_one_authenticated_snapshot() -> Result<()> {
    let (address, worker) = tapo::mock_sessions(
        vec![(
            vec!["get_device_info", "get_energy_usage", "get_emeter_data"],
            None,
        )],
        info(Model::P110),
        Some(energy()),
    )?;
    let file = Private::new(Model::P110, Some(address))?;
    let mut devices = homewire::device::Devices {
        plugs: Some(Manager::open(&file.path)?),
        ..Default::default()
    };
    let result = homewire::entity::call(
        &mut devices,
        "entity_status",
        &json!({"entities":["sensor.plug1_power","switch.plug1","sensor.plug1_voltage","sensor.plug1_total_energy"]}),
    )?;
    let states = result["states"].as_array().expect("states");
    assert!(states.iter().all(|s| s["available"] == true));
    assert_eq!(states[0]["state"], 45.259);
    assert_eq!(states[1]["state"]["power"], false);
    assert_eq!(states[2]["state"], 235.44);
    assert_eq!(states[3]["state"], 2.317816);
    assert_eq!(worker.join().expect("worker")?.len(), 3);
    Ok(())
}
#[test]
fn wire_p110_queries_only_supported_read_methods_and_preserves_measurements() -> Result<()> {
    let (address, worker) = tapo::mock_sessions(
        vec![(
            vec!["get_device_info", "get_energy_usage", "get_emeter_data"],
            None,
        )],
        info(Model::P110),
        Some(energy()),
    )?;
    let file = Private::new(Model::P110, Some(address))?;
    let manager = Manager::open(&file.path)?;
    let status = manager.status("plug1")?;
    assert_eq!(status.overloaded, Some(false));
    assert_eq!(status.energy.expect("meter").power_w, Some(45.259));
    assert_eq!(worker.join().expect("worker")?.len(), 3);
    Ok(())
}
#[test]
fn wire_rejected_calendar_statistics_preserve_fresh_live_meter_readings() -> Result<()> {
    let (address, worker) = tapo::mock_sessions_with_energy_replies(
        vec![(
            vec!["get_device_info", "get_energy_usage", "get_emeter_data"],
            None,
        )],
        info(Model::P110),
        Some((
            json!({"error_code":-1003}),
            json!({"error_code":0,"result":energy().1}),
        )),
    )?;
    let file = Private::new(Model::P110, Some(address))?;
    let status = Manager::open(&file.path)?.status("plug1")?;
    assert_eq!(status.overloaded, Some(false));
    let energy = status.energy.expect("fresh meter");
    assert_eq!(energy.power_w, Some(45.259));
    assert_eq!(energy.voltage_v, Some(235.44));
    assert_eq!(energy.current_a, Some(0.193));
    assert_eq!(energy.total_kwh, Some(2.317816));
    assert_eq!(energy.today_kwh, None);
    assert_eq!(energy.month_kwh, None);
    let requests = worker.join().expect("worker")?;
    assert_eq!(requests.len(), 3, "no command retries or clock writes");
    Ok(())
}
#[test]
fn wire_calendar_fallback_never_swallows_other_errors_or_invents_measurements() -> Result<()> {
    for (usage, meter, methods, expected) in [
        (
            json!({"error_code":-1002}),
            json!({"error_code":0,"result":energy().1}),
            vec!["get_device_info", "get_energy_usage"],
            "get_energy_usage request (code -1002)",
        ),
        (
            json!({"error_code":-1003}),
            json!({"error_code":-1003}),
            vec!["get_device_info", "get_energy_usage", "get_emeter_data"],
            "get_emeter_data request (code -1003)",
        ),
        (
            json!({"error_code":-1003}),
            json!({"error_code":0,"result":{}}),
            vec!["get_device_info", "get_energy_usage", "get_emeter_data"],
            "Missing plug energy measurements",
        ),
        (
            json!({"error_code":-1003}),
            json!({"error_code":0,"result":{"power_mw":-1}}),
            vec!["get_device_info", "get_energy_usage", "get_emeter_data"],
            "Invalid plug energy measurement",
        ),
    ] {
        let count = methods.len();
        let (address, worker) = tapo::mock_sessions_with_energy_replies(
            vec![(methods, None)],
            info(Model::P110),
            Some((usage, meter)),
        )?;
        let file = Private::new(Model::P110, Some(address))?;
        let error = Manager::open(&file.path)?
            .status("plug1")
            .expect_err("invalid energy remains unavailable");
        assert!(error.to_string().contains(expected), "{error}");
        assert_eq!(worker.join().expect("worker")?.len(), count);
    }
    Ok(())
}
#[test]
fn wire_p110_relay_status_verifies_identity_without_querying_energy() -> Result<()> {
    for mismatch in [false, true] {
        let mut value = info(Model::P110);
        if mismatch {
            value["device_id"] = json!("wrong");
        }
        let (address, worker) =
            tapo::mock_sessions(vec![(vec!["get_device_info"], None)], value, None)?;
        let file = Private::new(Model::P110, Some(address))?;
        let manager = Manager::open(&file.path)?;
        let result = manager.relay_status("plug1");
        if mismatch {
            assert!(result.is_err());
        } else {
            let status = result?;
            assert!(!status.power);
            assert_eq!(status.overloaded, Some(false));
            assert!(status.energy.is_none());
        }
        assert_eq!(worker.join().expect("worker")?.len(), 1);
    }
    Ok(())
}
#[test]
fn wire_identity_mismatch_and_reported_overload_stop_before_control() -> Result<()> {
    for mismatch in [true, false] {
        let mut value = info(Model::P110);
        if mismatch {
            value["device_id"] = json!("wrong");
        } else {
            value["power_protection_status"] = json!("overloaded");
        }
        let (address, worker) =
            tapo::mock_sessions(vec![(vec!["get_device_info"], None)], value, None)?;
        let file = Private::new(Model::P110, Some(address))?;
        let manager = Manager::open(&file.path)?;
        assert!(manager.apply("plug1", true).is_err());
        assert_eq!(worker.join().expect("worker")?.len(), 1);
    }
    Ok(())
}
#[test]
fn wire_verification_security_refresh_repeats_only_read() -> Result<()> {
    let (address, worker) = tapo::mock_sessions(
        vec![
            (
                vec!["get_device_info", "set_device_info", "get_device_info"],
                Some(2),
            ),
            (vec!["get_device_info"], None),
        ],
        info(Model::P100),
        None,
    )?;
    let file = Private::new(Model::P100, Some(address))?;
    let manager = Manager::open(&file.path)?;
    assert!(manager.apply("plug1", true)?.applied);
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
fn read_refresh_api_cannot_replay_mutating_methods() -> Result<()> {
    let (address, worker) = tapo::mock_sessions(vec![(vec![], None)], info(Model::P100), None)?;
    let mut client = Client::connect(
        address,
        "192.0.2.20:80".into(),
        &[0; 32],
        Duration::from_secs(3),
    )?;
    assert!(client.read("set_device_info").is_err());
    assert!(worker.join().expect("worker")?.is_empty());
    Ok(())
}
#[test]
fn plug_only_cli_and_mcp_do_not_require_other_device_config() -> Result<()> {
    let file = Private::new(Model::P100, None)?;
    let output = Command::new(env!("CARGO_BIN_EXE_homewire"))
        .args(["--plugs", file.path.to_str().expect("path"), "plugs"])
        .output()?;
    assert!(output.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout)?["plugs"][0]["model"],
        "P100"
    );
    let (address, worker) = tapo::mock_sessions(
        vec![(vec!["get_device_info"], None)],
        info(Model::P100),
        None,
    )?;
    let file = Private::new(Model::P100, Some(address))?;
    let mut server = Command::new(env!("CARGO_BIN_EXE_homewire"))
        .args([
            "--plugs",
            file.path.to_str().expect("path"),
            "serve",
            "--legacy-tools",
            "--allow-control",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut stdin = server.stdin.take().expect("stdin");
    let mut stdout = BufReader::new(server.stdout.take().expect("stdout"));
    for (id, name, args) in [
        (1, "plug_list", json!({})),
        (2, "plug_apply", json!({"plug":"plug1","power":false})),
    ] {
        writeln!(
            stdin,
            "{}",
            json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{"name":name,"arguments":args}})
        )?;
        stdin.flush()?;
        let mut line = String::new();
        stdout.read_line(&mut line)?;
        let result: Value = serde_json::from_str(&line)?;
        assert_eq!(result["result"]["isError"], false);
        if id == 2 {
            assert_eq!(result["result"]["structuredContent"]["applied"], false);
        }
    }
    drop(stdin);
    assert!(server.wait()?.success());
    assert_eq!(worker.join().expect("worker")?.len(), 1);
    Ok(())
}
#[test]
fn watcher_does_not_block_mcp_handshake_before_initialized() -> Result<()> {
    let file = Private::new(Model::P100, Some("127.0.0.1:1".parse()?))?;
    let mut server = Command::new(env!("CARGO_BIN_EXE_homewire"))
        .args([
            "--plugs",
            file.path.to_str().expect("path"),
            "serve",
            "--legacy-tools",
            "--watch-plugs",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()?;
    let start = Instant::now();
    let mut stdin = server.stdin.take().expect("stdin");
    writeln!(
        stdin,
        "{}",
        json!({"jsonrpc":"2.0","id":1,"method":"initialize"})
    )?;
    drop(stdin);
    let out = server.wait_with_output()?;
    assert!(out.status.success());
    assert!(start.elapsed() < Duration::from_secs(3));
    assert_eq!(serde_json::from_slice::<Value>(&out.stdout)?["id"], 1);
    Ok(())
}

#[test]
fn stdio_modes_preserve_errors_notifications_and_final_unterminated_message() -> Result<()> {
    for background in [false, true] {
        let file = Private::new(Model::P100, Some("127.0.0.1:1".parse()?))?;
        let mut command = Command::new(env!("CARGO_BIN_EXE_homewire"));
        command.args([
            "--plugs",
            file.path.to_str().expect("path"),
            "serve",
            "--legacy-tools",
        ]);
        if background {
            command.arg("--watch-plugs");
        }
        let mut server = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let mut stdin = server.stdin.take().expect("stdin");
        stdin.write_all(b"invalid-json\n{\"jsonrpc\":\"2.0\",\"method\":\"notifications/cancelled\"}\n{\"jsonrpc\":\"2.0\",\"id\":7,\"method\":\"ping\"}")?;
        drop(stdin);
        let output = server.wait_with_output()?;
        assert!(output.status.success());
        assert!(output.stderr.is_empty());
        let responses: Vec<Value> = String::from_utf8(output.stdout)?
            .lines()
            .map(serde_json::from_str)
            .collect::<std::result::Result<_, _>>()?;
        assert_eq!(responses.len(), 2, "notifications produce no output");
        assert_eq!(responses[0]["error"]["code"], -32700);
        assert_eq!(responses[1]["id"], 7);
        assert_eq!(responses[1]["result"], json!({}));
    }
    Ok(())
}

#[test]
fn stdio_modes_reject_oversized_and_non_utf8_input() -> Result<()> {
    for background in [false, true] {
        for input in [
            vec![b'x'; homewire::mcp::MAX_MESSAGE + 1],
            vec![0xff, b'\n'],
        ] {
            let file = Private::new(Model::P100, Some("127.0.0.1:1".parse()?))?;
            let mut command = Command::new(env!("CARGO_BIN_EXE_homewire"));
            command.args([
                "--plugs",
                file.path.to_str().expect("path"),
                "serve",
                "--legacy-tools",
            ]);
            if background {
                command.arg("--watch-plugs");
            }
            let mut server = command
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()?;
            server.stdin.take().expect("stdin").write_all(&input)?;
            let output = server.wait_with_output()?;
            assert!(!output.status.success());
            assert!(
                output.stdout.is_empty(),
                "transport errors never contaminate stdout"
            );
            assert!(!output.stderr.is_empty());
        }
    }
    Ok(())
}

#[test]
fn transient_handshake_rejection_retries_only_authentication_and_only_once() -> Result<()> {
    use homewire::klap::{Session, hash};
    for fail_again in [false, true] {
        let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        let address = listener.local_addr()?;
        let worker = std::thread::spawn(move || -> Result<usize> {
            let mut request_count = 0;
            let remote = [7; 16];
            let auth = [0; 32];
            for attempt in 0..2 {
                let (mut socket, _) = listener.accept()?;
                let (path, local, _) = tapo::read_request(&mut socket)?;
                assert_eq!(path, "/app/handshake1");
                request_count += 1;
                let local: [u8; 16] = local.try_into().expect("seed");
                let mut body = remote.to_vec();
                body.extend_from_slice(&hash(&[&local, &remote, &auth]));
                tapo::respond(&mut socket, &body, true)?;
                drop(socket);
                let (mut socket, _) = listener.accept()?;
                let (path, proof, _) = tapo::read_request(&mut socket)?;
                assert_eq!(path, "/app/handshake2");
                assert_eq!(proof, hash(&[&remote, &local, &auth]));
                request_count += 1;
                if attempt == 0 || fail_again {
                    socket.write_all(b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\n\r\n")?;
                    continue;
                }
                tapo::respond(&mut socket, &[], false)?;
                drop(socket);
                let (mut socket, _) = listener.accept()?;
                let (_, body, _) = tapo::read_request(&mut socket)?;
                request_count += 1;
                let mut codec = Session::new(&local, &remote, &auth)?;
                codec.encrypt(b"")?;
                let request: Value = serde_json::from_slice(&codec.decrypt(&body)?)?;
                assert_eq!(request["method"], "get_device_info");
                let mut codec = Session::new(&local, &remote, &auth)?;
                let body = codec
                    .encrypt(&serde_json::to_vec(
                        &json!({"error_code":0,"result":info(Model::P100)}),
                    )?)?
                    .0;
                tapo::respond(&mut socket, &body, false)?;
            }
            Ok(request_count)
        });
        let client = Client::connect(
            address,
            "192.0.2.20:80".into(),
            &[0; 32],
            Duration::from_secs(3),
        );
        if fail_again {
            assert!(client.is_err());
        } else {
            assert_eq!(client?.read_info()?["model"], "P100");
        }
        assert_eq!(
            worker.join().expect("worker")?,
            if fail_again { 4 } else { 5 }
        );
    }
    Ok(())
}
