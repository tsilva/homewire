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
