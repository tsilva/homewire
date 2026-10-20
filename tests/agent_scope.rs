#![cfg(unix)]
use homewire::{
    api,
    device::{Adapter, Devices},
    shopping,
};
use serde_json::{Value, json};
#[path = "support/mod.rs"]
mod support;

fn scoped(devices: &mut Devices, method: &str, params: Value) -> Value {
    api::dispatch(
        devices,
        json!({"jsonrpc":"2.0","id":1,"method":"homewire/api", "params":{
        "role":"agent","route":"/mcp","tools":["shopping_list"],
        "data":{"jsonrpc":"2.0","id":9,"method":method,"params":params}}}),
        true,
    )
    .expect("reply")
}

#[test]
fn agent_scope_denies_mutations_and_api_role_forgery_before_dispatch() {
    let directory = support::Directory::new().expect("directory");
    let cfg = directory.write("shopping.json", b"{}").expect("config");
    let mut devices = Devices {
        shopping: Some(shopping::Manager::open(&cfg).expect("manager")),
        ..Devices::default()
    };
    let list = scoped(&mut devices, "tools/list", json!({}));
    assert_eq!(
        list["result"]["result"]["tools"]
            .as_array()
            .expect("tools")
            .len(),
        1
    );
    let denied = scoped(
        &mut devices,
        "tools/call",
        json!({"name":"shopping_update","arguments":{"action":"add","name":"must not persist"}}),
    );
    assert_eq!(denied["result"]["error"]["code"], -32602);
    let forged = scoped(
        &mut devices,
        "homewire/api",
        json!({"role":"dashboard","route":"/api/call","data":{"name":"shopping_update"}}),
    );
    assert_eq!(forged["result"]["error"]["code"], -32601);
    let read = scoped(
        &mut devices,
        "tools/call",
        json!({"name":"shopping_list","arguments":{}}),
    );
    assert_eq!(
        read["result"]["result"]["structuredContent"]["items"],
        json!([])
    );
    drop(devices);
}

#[test]
fn agent_cannot_use_dashboard_routes_or_omit_scope() {
    let mut devices = Devices::default();
    for params in [
        json!({"role":"agent","route":"/api/call","data":{}}),
        json!({"role":"agent","route":"/mcp","data":{}}),
    ] {
        let reply =
            api::dispatch(&mut devices, json!({"id":1,"params":params}), true).expect("reply");
        assert!(reply.get("error").is_some());
    }
}

#[test]
fn dehumidifier_scope_exposes_only_narrow_modes_and_respects_read_only_gate() -> homewire::Result<()>
{
    use homewire::{entity, meaco};
    use std::{io::ErrorKind, net::TcpListener};
    let directory = support::Directory::new()?;
    let cfg = directory.config("meaco.json", &json!({"device_id":"synthetic","host":"192.0.2.1","local_key":"0123456789abcdef","protocol_version":"3.3","timeout_seconds":1}))?;
    let listener = TcpListener::bind("127.0.0.1:0")?;
    listener.set_nonblocking(true)?;
    let mut devices = Devices {
        meaco: Some(meaco::Manager::open_with_connect(
            &cfg,
            Some(listener.local_addr()?),
        )?),
        ..Devices::default()
    };
    let mut dispatch = |method: &str, params: Value, allow_control: bool| {
        api::dispatch(&mut devices, json!({"id":1,"method":"homewire/api","params":{"role":"agent","route":"/mcp","tools":["dehumidifier_set"],"data":{"jsonrpc":"2.0","id":2,"method":method,"params":params}}}), allow_control).expect("reply")
    };
    let tools = dispatch("tools/list", json!({}), true);
    let tools = tools["result"]["result"]["tools"]
        .as_array()
        .expect("tools");
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0]["name"], "dehumidifier_set");
    for (name, settings, allow_control) in [
        ("entity_configure", json!({"mode":"purify"}), true),
        ("dehumidifier_set", json!({"power":true}), true),
        ("dehumidifier_set", json!({"sleep":"on"}), true),
        ("dehumidifier_set", json!({"sleep":true}), false),
    ] {
        let reply = dispatch(
            "tools/call",
            json!({"name":name,"arguments":{"entity":"dehumidifier.meaco","settings":settings}}),
            allow_control,
        );
        assert!(
            reply["result"].get("error").is_some() || reply["result"]["result"]["isError"] == true,
            "{reply}"
        );
    }
    let schema = entity::list(&devices);
    assert_eq!(schema["entities"][0]["capabilities"]["purification"], true);
    assert_eq!(schema["entities"][0]["capabilities"]["sleep"], true);
    assert!(
        entity::call(
            &mut devices,
            "dehumidifier_set",
            &json!({"entity":"sensor.meaco_humidity","settings":{"sleep":true}})
        )
        .is_err()
    );
    assert_eq!(
        listener.accept().expect_err("no device I/O").kind(),
        ErrorKind::WouldBlock
    );
    Ok(())
}
