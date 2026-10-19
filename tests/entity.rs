use homewire::{
    Result, awair, bulb,
    device::{Adapter, Devices, Selection},
    entity, mcp, tv,
};
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Write},
    net::TcpListener,
    process::{Command, Stdio},
    thread,
};
#[path = "support/mod.rs"]
mod support;

#[test]
fn meaco_discovery_uses_dehumidifier_and_legacy_id_shares_one_fresh_read() -> Result<()> {
    use homewire::{meaco, tuya::encrypt};
    use std::{io::Read, time::Duration};
    let temp = support::Directory::new()?;
    let config = temp.config(
        "meaco.json",
        &json!({
            "device_id":"synthetic", "host":"192.0.2.1", "local_key":"0123456789abcdef",
            "protocol_version":"3.3", "timeout_seconds":2,
        }),
    )?;
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let mut devices = Devices {
        meaco: Some(meaco::Manager::open_with_connect(
            &config,
            Some(listener.local_addr()?),
        )?),
        ..Devices::default()
    };
    let discovery = entity::list(&devices);
    let base = &discovery["entities"][0];
    assert_eq!(base["entity"], "dehumidifier.meaco");
    assert_eq!(base["domain"], "dehumidifier");
    assert!(!discovery.to_string().contains("\"humidifier.meaco\""));
    let worker = thread::spawn(move || -> Result<TcpListener> {
        let (mut socket, _) = listener.accept()?;
        socket.set_read_timeout(Some(Duration::from_secs(2)))?;
        let mut header = [0; 16];
        socket.read_exact(&mut header)?;
        assert_eq!(u32::from_be_bytes(header[8..12].try_into()?), 10);
        let mut query = vec![0; u32::from_be_bytes(header[12..].try_into()?) as usize];
        socket.read_exact(&mut query)?;
        let payload = encrypt(
            &serde_json::to_vec(&json!({"dps":{"1":false,"2":55,"4":"manual","16":61,"19":0}}))?,
            b"0123456789abcdef",
        );
        let mut frame = Vec::new();
        for value in [0x55aa_u32, 1, 10, payload.len() as u32 + 12, 0] {
            frame.extend_from_slice(&value.to_be_bytes());
        }
        frame.extend_from_slice(&payload);
        frame.extend_from_slice(&crc32fast::hash(&frame).to_be_bytes());
        frame.extend_from_slice(&0xaa55_u32.to_be_bytes());
        socket.write_all(&frame)?;
        Ok(listener)
    });
    let reply = entity::call(
        &mut devices,
        "entity_status",
        &json!({"entities":["dehumidifier.meaco","humidifier.meaco","sensor.meaco_humidity","sensor.meaco_tank_full","sensor.meaco_target"]}),
    )?;
    let states = reply["states"].as_array().expect("states");
    assert!(states.iter().all(|s| s["available"] == true));
    assert_eq!(states[0]["state"], states[1]["state"]);
    assert_eq!(states[0]["state"]["power"], false);
    assert_eq!(states[2]["state"], 61);
    assert_eq!(states[3]["state"], false);
    assert_eq!(states[4]["state"], 55);
    assert!(
        states
            .iter()
            .all(|s| s["observed_at"] == states[0]["observed_at"])
    );
    let listener = worker.join().expect("mock")?;
    listener.set_nonblocking(true)?;
    assert_eq!(
        listener.accept().expect_err("one read").kind(),
        std::io::ErrorKind::WouldBlock
    );
    Ok(())
}

fn lights(temp: &support::Directory) -> Result<bulb::Manager> {
    let mut value: Value = serde_json::from_str(include_str!("../examples/lights.json"))?;
    value.as_object_mut().expect("config").remove("automation");
    for endpoint in value["lights"].as_array_mut().expect("lights") {
        endpoint["auth_hash"] = json!("00".repeat(32));
        endpoint["connect"] = json!("127.0.0.1:1");
    }
    bulb::Manager::open(&temp.config("lights.json", &value)?)
}
#[test]
fn discovery_groups_capabilities_without_network_queries_or_credentials() -> Result<()> {
    let temp = support::Directory::new()?;
    let devices = Devices {
        bulbs: Some(lights(&temp)?),
        awair: Some(awair::Config {
            host: "192.0.2.12".parse()?,
            port: 80,
            connect: None,
            timeout_seconds: 1,
            max_age_seconds: 120,
        }),
        ..Devices::default()
    };
    devices.validate()?;
    let list = entity::list(&devices);
    let entities = list["entities"].as_array().expect("entities");
    let strip = entities
        .iter()
        .find(|e| e["entity"] == "light.led_strip")
        .expect("strip");
    assert_eq!(strip["capabilities"]["color_temperature_k"]["max"], 9000);
    assert_eq!(
        strip["capabilities"]["fixed_settings_require_effects_off"],
        true
    );
    let sensors: Vec<_> = entities
        .iter()
        .filter(|e| e["domain"] == "sensor")
        .collect();
    assert_eq!(sensors.len(), 6);
    assert!(sensors.iter().all(|e| e["device"] == "awair.awair"));
    for forbidden in ["auth_hash", "host", "local_key", "connect"] {
        assert!(!list.to_string().contains(forbidden));
    }
    Ok(())
}
#[test]
fn sensor_batch_fetches_once_per_physical_device_and_refreshes_next_request() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let mut devices = Devices {
        awair: Some(awair::Config {
            host: "192.0.2.12".parse()?,
            port: 80,
            connect: Some(listener.local_addr()?),
            timeout_seconds: 1,
            max_age_seconds: 120,
        }),
        ..Devices::default()
    };
    let worker = thread::spawn(move || -> Result<()> {
        for co2 in [620, 710] {
            let (mut socket, _) = listener.accept()?;
            socket.set_read_timeout(Some(std::time::Duration::from_secs(2)))?;
            let mut reader = BufReader::new(socket.try_clone()?);
            let mut first = String::new();
            reader.read_line(&mut first)?;
            assert_eq!(first, "GET /air-data/latest HTTP/1.1\r\n");
            loop {
                let mut line = String::new();
                reader.read_line(&mut line)?;
                if line == "\r\n" {
                    break;
                }
                assert!(!line.is_empty());
            }
            let body=json!({"timestamp":chrono::Utc::now().to_rfc3339(),"score":91,"temp":24.75,"humid":51.2,"co2":co2,"voc":180.5,"pm25":3.25}).to_string();
            write!(
                socket,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )?;
        }
        Ok(())
    });
    for co2 in [620, 710] {
        let result = entity::call(
            &mut devices,
            "entity_status",
            &json!({"entities":["sensor.awair_co2","sensor.awair_temperature","sensor.awair_humidity"]}),
        )?;
        let states = result["states"].as_array().expect("states");
        assert!(states.iter().all(|s| s["available"] == true));
        assert_eq!(states[0]["state"].as_f64(), Some(f64::from(co2)));
        assert_eq!(states[1]["state"], 24.75);
        assert_eq!(states[2]["state"], 51.2);
        assert_eq!(states[0]["observed_at"], states[2]["observed_at"]);
        assert!(states[0]["measured_at"].is_string());
    }
    worker.join().expect("mock")?;
    Ok(())
}
#[test]
fn complete_target_validation_and_capability_checks_precede_device_io() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    listener.set_nonblocking(true)?;
    let mut devices = Devices {
        awair: Some(awair::Config {
            host: "192.0.2.12".parse()?,
            port: 80,
            connect: Some(listener.local_addr()?),
            timeout_seconds: 1,
            max_age_seconds: 120,
        }),
        ..Devices::default()
    };
    for args in [
        json!({"entities":[]}),
        json!({"entities":["sensor.awair_co2","sensor.awair_co2"]}),
        json!({"entities":["sensor.awair_co2","sensor.missing"]}),
        json!({"entities":vec!["sensor.awair_co2";33]}),
        json!({"entities":["sensor.awair_co2"],"host":"127.0.0.1"}),
    ] {
        assert!(entity::call(&mut devices, "entity_status", &args).is_err());
    }
    assert!(
        entity::call(
            &mut devices,
            "switch_set",
            &json!({"entity":"sensor.awair_co2","power":false})
        )
        .is_err()
    );
    assert_eq!(
        listener.accept().expect_err("no I/O").kind(),
        std::io::ErrorKind::WouldBlock
    );
    Ok(())
}
#[test]
fn unavailable_sensor_batch_returns_explicit_unknown_without_stale_values() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let address = listener.local_addr()?;
    drop(listener);
    let mut devices = Devices {
        awair: Some(awair::Config {
            host: "192.0.2.12".parse()?,
            port: 80,
            connect: Some(address),
            timeout_seconds: 1,
            max_age_seconds: 120,
        }),
        ..Devices::default()
    };
    let result = entity::call(
        &mut devices,
        "entity_status",
        &json!({"entities":["sensor.awair_co2","sensor.awair_temperature"]}),
    )?;
    assert!(
        result["states"]
            .as_array()
            .expect("states")
            .iter()
            .all(|s| s["available"] == false && s["state"].is_null() && s["error"].is_string())
    );
    Ok(())
}
#[test]
fn tv_routine_owns_only_its_power_controls_in_standard_and_legacy_interfaces() -> Result<()> {
    let temp = support::Directory::new()?;
    temp.certs()?;
    let mut samsung: Value = serde_json::from_str(include_str!("../examples/samsung.json"))?;
    samsung["token"] = json!("00".repeat(16));
    temp.config("samsung.json", &samsung)?;
    let mut digi: Value = serde_json::from_str(include_str!("../examples/digi.json"))?;
    digi["client_cert"] = json!("cert.pem");
    digi["client_key"] = json!("key.pem");
    digi["server_cert_sha256"] = json!("00".repeat(32));
    temp.config("digi.json", &digi)?;
    let mut plugs: Value = serde_json::from_str(include_str!("../examples/plugs.json"))?;
    for plug in plugs["plugs"].as_array_mut().expect("plugs") {
        plug["auth_hash"] = json!("00".repeat(32));
    }
    temp.config("plugs.json", &plugs)?;
    let path = temp.config(
        "tv.json",
        &serde_json::from_str(include_str!("../examples/tv.json"))?,
    )?;
    let mut devices = Devices {
        tv: Some(tv::Manager::open(&path)?),
        ..Devices::default()
    };
    devices.validate()?;
    for (tool, args) in [
        ("switch_set", json!({"entity":"switch.tv","power":false})),
        ("wake_on_lan", json!({"device":"samsung.samsung"})),
        (
            "media_power",
            json!({"entity":"media_player.samsung","power":false}),
        ),
        (
            "media_power",
            json!({"entity":"media_player.digi","power":true}),
        ),
    ] {
        assert!(
            entity::call(&mut devices, tool, &args)
                .expect_err("owned power")
                .to_string()
                .contains("owned by the TV routine")
        );
    }
    for (tool, args) in [
        ("plug_apply", json!({"plug":"tv","power":false})),
        ("samsung_apply", json!({"power":false})),
        ("digi_apply", json!({"power":true})),
    ] {
        assert!(
            devices
                .call(tool, &args)
                .expect_err("legacy guard")
                .to_string()
                .contains("owned by the TV routine")
        );
    }
    let list = entity::list(&devices);
    let entities = list["entities"].as_array().expect("entities");
    let wakes = entity::call(&mut devices, "wake_list", &json!({}))?;
    assert_eq!(wakes["devices"][0]["device"], "samsung.samsung");
    assert_eq!(wakes["devices"][0]["maintenance_required"], true);
    let alias = temp.config(
        "wol.json",
        &json!({"devices":[{"name":"alias","mac":samsung["mac"],"wake_address":"127.0.0.1:9"}]}),
    )?;
    devices.wol = Some(homewire::wol::Manager::open(&alias)?);
    assert!(
        devices
            .validate()
            .expect_err("alias bypass")
            .to_string()
            .contains("Duplicate wake identity")
    );
    assert!(entity::call(&mut devices, "wake_on_lan", &json!({"device":"wol.alias"})).is_err());
    devices.wol = None;
    assert!(
        entities
            .iter()
            .filter(|e| !e["power_owner"].is_null())
            .all(|e| e["capabilities"]["power"] == false)
    );
    assert!(entities.iter().any(|e| e["domain"] == "switch"
        && e["entity"] != "switch.tv"
        && e["capabilities"]["power"] == true));
    assert!(
        entity::call(
            &mut devices,
            "remote_key",
            &json!({"entity":"media_player.samsung","key":"channel_up"})
        )
        .is_err()
    );
    let plan = entity::call(&mut devices, "routine_plan", &json!({"routine":"tv_off"}))?;
    assert_eq!(plan["result"]["live_state_checked"], false);
    assert_eq!(plan["result"]["physical_samsung_off_verified"], false);
    devices.maintenance = true;
    let list = entity::list(&devices);
    assert!(
        list["entities"]
            .as_array()
            .expect("entities")
            .iter()
            .filter(|e| !e["power_owner"].is_null())
            .all(|e| e["capabilities"]["power"] == true)
    );
    Ok(())
}
#[test]
fn standard_mcp_grants_only_configured_categories_and_gates_all_writes() {
    let selection = Selection::ALL;
    let request = json!({"jsonrpc":"2.0","id":1,"method":"tools/list"});
    for control in [false, true] {
        let list =
            mcp::handle_capabilities(request.clone(), selection, control, |_, _| panic!("no I/O"))
                .expect("reply");
        let tools = list["result"]["tools"].as_array().expect("tools");
        let expected: Vec<_> = entity::tools(selection)
            .filter(|t| control || t.hints.read_only)
            .map(|t| t.name)
            .collect();
        assert_eq!(tools.len(), expected.len());
        assert!(
            expected
                .iter()
                .all(|name| tools.iter().any(|t| t["name"] == *name))
        );
        assert!(!tools.iter().any(|t| t["name"] == "samsung_apply"));
    }
    for (name, args) in [
        (
            "light_set",
            json!({"entity":"light.test","settings":{"power":false}}),
        ),
        ("switch_set", json!({"entity":"switch.test","power":false})),
        (
            "media_power",
            json!({"entity":"media_player.digi","power":false}),
        ),
        (
            "remote_key",
            json!({"entity":"media_player.digi","key":"home"}),
        ),
        (
            "media_launch",
            json!({"entity":"media_player.digi","url":"https://www.netflix.com"}),
        ),
        (
            "media_text",
            json!({"entity":"media_player.digi","text":"movie"}),
        ),
        ("routine_run", json!({"routine":"tv_off"})),
    ] {
        let result=mcp::handle_capabilities(json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":name,"arguments":args}}),selection,false,|_,_|panic!("write bypass")).expect("reply");
        assert_eq!(result["result"]["isError"], true);
    }
}
#[test]
fn default_stdio_advertises_entities_and_rejects_legacy_names() -> Result<()> {
    let temp = support::Directory::new()?;
    let path = temp.config("awair.json", &json!({"host":"192.0.2.12"}))?;
    let mut child = Command::new(env!("CARGO_BIN_EXE_homewire"))
        .args(["--awair", path.to_str().expect("path"), "serve"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut input = child.stdin.take().expect("stdin");
    for request in [
        json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}),
        json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"entity_list"}}),
        json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"awair_status"}}),
    ] {
        writeln!(input, "{request}")?;
    }
    drop(input);
    let output = child.wait_with_output()?;
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    let replies: Vec<Value> = String::from_utf8(output.stdout)?
        .lines()
        .map(serde_json::from_str)
        .collect::<std::result::Result<_, _>>()?;
    assert_eq!(
        replies[0]["result"]["tools"]
            .as_array()
            .expect("tools")
            .len(),
        2
    );
    assert_eq!(
        replies[1]["result"]["structuredContent"]["entities"]
            .as_array()
            .expect("entities")
            .len(),
        6
    );
    assert_eq!(replies[2]["error"]["code"], -32602);
    Ok(())
}
