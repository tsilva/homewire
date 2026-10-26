use homewire::{
    Result,
    bundle::Bundle,
    entity,
    group::{Group, Groups},
    mcp,
};
use serde_json::{Value, json};
#[path = "support/mod.rs"]
#[allow(dead_code)]
mod support;

fn fixture() -> Value {
    json!({"version":1,"adapters":{"lights":{"settle_seconds":0,"lights":[
        {"name":"sample_bulb","host":"192.0.2.11","device_id":"SYNTHETICBULB","auth_hash":"00".repeat(32)},
        {"name":"sample_strip","host":"192.0.2.12","model":"L900","device_id":"SYNTHETICSTRIP","auth_hash":"00".repeat(32)}]}},
        "groups":{"sample_lights":{"members":["light.sample_bulb","light.sample_strip"]}},
        "routines":{"sleep":{"steps":[{"action":"group","group":"group.sample_lights"}]}}})
}

#[test]
fn group_and_shutdown_plans_are_offline_and_control_gate_is_preserved() -> Result<()> {
    let temp = support::Directory::new()?;
    let path = temp.config("home.json", &fixture())?;
    let bundle = Bundle::load(&path)?.expect("synthetic fixture");
    let mut devices = bundle.open_devices(None)?;
    let list = entity::call(&mut devices, "group_list", &json!({}))?;
    assert_eq!(
        list["groups"][0]["members"]
            .as_array()
            .expect("synthetic fixture")
            .len(),
        2
    );
    let plan = entity::call(
        &mut devices,
        "group_plan",
        &json!({"group":"group.sample_lights","power":false}),
    )?;
    assert_eq!(plan["live_state_checked"], false);
    assert_eq!(plan["atomic"], false);
    let plan = entity::call(&mut devices, "routine_plan", &json!({"routine":"sleep"}))?;
    assert_eq!(plan["result"]["steps"][0]["group"], "group.sample_lights");
    let denied = mcp::handle_entities(
        json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"group_set","arguments":{"group":"group.sample_lights","power":true}}}),
        &mut devices,
        false,
    ).expect("MCP reply");
    assert!(denied["error"].is_object() || denied["result"]["isError"] == true);
    assert!(
        entity::call(
            &mut devices,
            "group_set",
            &json!({"group":"group.sample_lights","power":true,"brightness":50})
        )
        .is_err()
    );
    Ok(())
}

#[test]
fn duplicate_unknown_nested_and_non_light_members_fail_before_io() -> Result<()> {
    for members in [
        json!([]),
        json!(["light.sample_bulb", "light.sample_bulb"]),
        json!(["light.sample_bulb", "light.unknown"]),
        json!(["group.sample_lights"]),
        json!(["sensor.sample_bulb_overheated"]),
    ] {
        let temp = support::Directory::new()?;
        let mut value = fixture();
        value["groups"]["sample_lights"]["members"] = members;
        let path = temp.config("home.json", &value)?;
        let bundle = Bundle::load(&path)?.expect("synthetic fixture");
        assert!(bundle.open_devices(None).is_err());
    }
    let temp = support::Directory::new()?;
    let mut value = fixture();
    value["routines"]["sleep"]["steps"]
        .as_array_mut()
        .expect("synthetic fixture")
        .push(json!({"action":"light","entity":"light.sample_strip"}));
    let path = temp.config("home.json", &value)?;
    let bundle = Bundle::load(&path)?.expect("synthetic fixture");
    assert!(bundle.open_devices(None).is_err());
    Ok(())
}

#[test]
fn all_members_resolve_before_even_one_network_connection() -> Result<()> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    listener.set_nonblocking(true)?;
    let temp = support::Directory::new()?;
    let mut value = fixture();
    value["adapters"]["lights"]["lights"][0]["connect"] = json!(listener.local_addr()?.to_string());
    let path = temp.config("home.json", &value)?;
    let bundle = Bundle::load(&path)?.expect("synthetic fixture");
    let mut devices = bundle.open_devices(None)?;
    devices.groups = Groups(std::collections::BTreeMap::from([(
        "invalid".into(),
        Group {
            members: vec!["light.sample_bulb".into(), "light.unknown".into()],
        },
    )]));
    assert!(
        entity::call(
            &mut devices,
            "group_set",
            &json!({"group":"group.invalid","power":false})
        )
        .is_err()
    );
    assert_eq!(
        listener.accept().expect_err("no network request").kind(),
        std::io::ErrorKind::WouldBlock
    );
    Ok(())
}
