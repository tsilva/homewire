use homewire::{
    Result,
    bundle::Bundle,
    device::{Adapter, Devices},
    entity, mcp, wol,
};
use serde_json::{Value, json};
use std::{net::UdpSocket, time::Duration};
#[path = "support/mod.rs"]
#[allow(dead_code)]
mod support;

fn request(name: &str, args: Value) -> Value {
    json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":name,"arguments":args}})
}

#[test]
fn known_targets_are_discoverable_and_mcp_sends_once_only_with_permission() -> Result<()> {
    let t = support::Directory::new()?;
    let socket = UdpSocket::bind("127.0.0.1:0")?;
    socket.set_read_timeout(Some(Duration::from_millis(100)))?;
    let c = json!({"version":1,"adapters":{"wol":{"devices":[{"name":"pc","mac":"02:00:00:00:00:01","wake_address":socket.local_addr()?}]}}});
    let bundle = Bundle::load(&t.config("home.json", &c)?)?.expect("bundle");
    let mut devices = bundle.open_devices(None)?;
    let _lock = bundle.into_lock();
    let list =
        mcp::handle_entities(request("wake_list", json!({})), &mut devices, false).expect("reply");
    let targets = &list["result"]["structuredContent"]["devices"];
    assert_eq!(targets[0]["device"], "wol.pc");
    assert_eq!(targets[0]["wake_verifiable"], false);
    assert!(!targets.to_string().contains("mac"));
    assert!(!targets.to_string().contains("127.0.0.1"));
    for allow in [false, true] {
        let listed = mcp::handle_entities(
            json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
            &mut devices,
            allow,
        )
        .expect("reply");
        let tools = listed["result"]["tools"].as_array().expect("tools");
        assert!(tools.iter().any(|v| v["name"] == "wake_list"));
        assert_eq!(tools.iter().any(|v| v["name"] == "wake_on_lan"), allow);
    }
    let denied = mcp::handle_entities(
        request("wake_on_lan", json!({"device":"wol.pc"})),
        &mut devices,
        false,
    )
    .expect("reply");
    assert_eq!(denied["result"]["isError"], true);
    for args in [
        json!({"device":"wol.unknown"}),
        json!({"device":"wol.pc","mac":"02:00:00:00:00:02"}),
        json!({"device":"wol.pc","wake_address":"127.0.0.1:9"}),
        json!({"device":"wol.pc","power":false}),
        json!({"device":12}),
    ] {
        let reply =
            mcp::handle_entities(request("wake_on_lan", args), &mut devices, true).expect("reply");
        assert!(reply.get("error").is_some() || reply["result"]["isError"] == true);
    }
    let mut packet = [0; 103];
    assert!(socket.recv(&mut packet).is_err());
    let reply = mcp::handle_entities(
        request("wake_on_lan", json!({"device":"wol.pc"})),
        &mut devices,
        true,
    )
    .expect("reply");
    let result = &reply["result"]["structuredContent"]["result"];
    assert_eq!(result["sent"], true);
    assert_eq!(result["verified"], false);
    assert_eq!(socket.recv(&mut packet)?, 102);
    assert_eq!(packet[..6], [255; 6]);
    for block in packet[6..102].as_chunks::<6>().0 {
        assert_eq!(*block, [2, 0, 0, 0, 0, 1]);
    }
    assert!(socket.recv(&mut packet).is_err(), "no replay");
    Ok(())
}

#[test]
fn unknown_configurations_and_duplicate_identities_are_rejected_without_sending() -> Result<()> {
    let t = support::Directory::new()?;
    let good = json!({"name":"pc","mac":"02:00:00:00:00:01","wake_address":"127.0.0.1:9"});
    let mut invalid = vec![
        json!({"devices":[]}),
        json!({"devices":[good.clone(),good.clone()]}),
        json!({"devices":[good.clone()],"extra":true}),
    ];
    for (field, value) in [
        ("name", json!("a".repeat(25))),
        ("mac", json!("00:00:00:00:00:00")),
        ("mac", json!("ff:ff:ff:ff:ff:ff")),
        ("mac", json!("invalid")),
        ("wake_address", json!("127.0.0.1:0")),
        ("wake_address", json!("0.0.0.0:9")),
        ("wake_address", json!("224.0.0.1:9")),
    ] {
        let mut d = good.clone();
        d[field] = value;
        invalid.push(json!({"devices":[d]}));
    }
    let mut alias = good.clone();
    alias["name"] = json!("alias");
    alias["mac"] = json!("020000000001");
    invalid.push(json!({"devices":[good.clone(),alias]}));
    invalid.push(json!({"devices":(0..9).map(|i|json!({"name":format!("pc{i}"),"mac":format!("02:00:00:00:00:{:02x}",i+1),"wake_address":"127.0.0.1:9"})).collect::<Vec<_>>()}));
    for c in invalid {
        assert!(wol::Manager::open(&t.config("invalid.json", &c)?).is_err());
    }
    let path = t.config("wol.json", &json!({"devices":[good]}))?;
    let mut devices = Devices {
        wol: Some(wol::Manager::open(&path)?),
        ..Devices::default()
    };
    assert!(
        wol::Manager::open(&path).is_err(),
        "configuration ownership"
    );
    assert!(
        entity::call(
            &mut Devices::default(),
            "wake_on_lan",
            &json!({"device":"wol.pc"})
        )
        .is_err()
    );
    assert_eq!(
        entity::call(&mut devices, "wake_list", &json!({}))?["devices"]
            .as_array()
            .expect("list")
            .len(),
        1
    );
    Ok(())
}
