use homewire::{Result, bundle::Bundle, entity, routine};
use serde_json::{Value, json};
#[path = "support/mod.rs"]
#[allow(dead_code)]
mod support;
fn fixture() -> Value {
    let plugs=(0..4).map(|i|json!({"name":format!("outlet_{i}"),"model":"P100","host":format!("192.0.2.{}",30+i),"device_id":format!("SYNTHETICDEVICE{i}"),"auth_hash":"00".repeat(32)})).collect::<Vec<_>>();
    json!({"version":1,"adapters":{"plugs":{"plugs":plugs},"printers":{"printers":[{"name":"office","host":"192.0.2.10"}]},
        "computer":{"name":"workstation","address":"192.0.2.50:39817","server_public_key":"22".repeat(32),"signing_key":"11".repeat(32),"mac":"02:00:00:00:00:01","wake_address":"192.0.2.255:9"}},
        "routines":{"computer":{"plug":"outlet_3","startup_seconds":2,"ready_seconds":2,"shutdown_policy":"leave_on"},
        "leave_house":{"steps":[{"action":"printer","entity":"printer.office","outlet":"switch.outlet_0"},{"action":"outlet","entity":"switch.outlet_1"}],"optional_pc":[{"action":"routine","name":"computer_off"}],"optional_kitchen":[{"action":"outlet","entity":"switch.outlet_2"}]},
        "sleep":{"steps":[{"action":"printer","entity":"printer.office","outlet":"switch.outlet_0"},{"action":"outlet","entity":"switch.outlet_1"},{"action":"routine","name":"computer_off"},{"action":"outlet","entity":"switch.outlet_2"}]}}})
}
#[test]
fn plans_are_offline_explicit_and_optional_groups_require_booleans() -> Result<()> {
    let t = support::Directory::new()?;
    let path = t.config("home.json", &fixture())?;
    let bundle = Bundle::load(&path)?.expect("bundle");
    let mut devices = bundle.open_devices(None)?;
    let list = entity::call(&mut devices, "routine_list", &json!({}))?;
    assert!(list.to_string().contains("leave_house") && list.to_string().contains("sleep"));
    for (pc, kitchen, count) in [
        (false, false, 2),
        (true, false, 3),
        (false, true, 3),
        (true, true, 4),
    ] {
        let plan = entity::call(
            &mut devices,
            "routine_plan",
            &json!({"routine":"leave_house","include_pc":pc,"include_kitchen":kitchen}),
        )?;
        assert_eq!(
            plan["result"]["steps"].as_array().expect("steps").len(),
            count
        );
        assert_eq!(plan["result"]["live_state_checked"], false);
        assert_eq!(plan["result"]["execution"], "concurrent_best_effort");
    }
    let plan = routine::call(
        &mut devices,
        "routine_plan",
        &json!({"routine":"leave_house"}),
    )?;
    assert_eq!(plan["result"]["steps"].as_array().expect("steps").len(), 2);
    let plan = routine::call(&mut devices, "routine_plan", &json!({"routine":"sleep"}))?;
    assert_eq!(plan["result"]["steps"].as_array().expect("steps").len(), 4);
    for args in [
        json!({"routine":"sleep","include_pc":false}),
        json!({"routine":"leave_house","include_pc":"true"}),
        json!({"routine":"leave_house","all":true}),
        json!({"routine":"computer_off","include_kitchen":true}),
    ] {
        for tool in ["routine_run", "routine_plan"] {
            assert!(routine::call(&mut devices, tool, &args).is_err());
        }
    }
    Ok(())
}
#[test]
fn invalid_targets_duplicate_physical_devices_and_owned_outlets_fail_before_io() -> Result<()> {
    for step in [
        json!({"action":"outlet","entity":"switch.outlet_3"}),
        json!({"action":"outlet","entity":"sensor.office_queued_jobs"}),
        json!({"action":"printer","entity":"sensor.office_queued_jobs","outlet":"switch.outlet_0"}),
        json!({"action":"outlet","entity":"switch.missing"}),
        json!({"action":"outlet","entity":"switch.outlet_0"}),
    ] {
        let t = support::Directory::new()?;
        let mut value = fixture();
        value["routines"]["leave_house"]["steps"]
            .as_array_mut()
            .expect("steps")
            .push(step);
        let path = t.config("home.json", &value)?;
        assert!(
            Bundle::load(&path)?
                .expect("bundle")
                .open_devices(None)
                .is_err()
        );
    }
    Ok(())
}

#[test]
fn plug_only_lists_are_discoverable_and_household_runs_require_control_permission() -> Result<()> {
    let t = support::Directory::new()?;
    let mut value = fixture();
    value["adapters"]
        .as_object_mut()
        .expect("adapters")
        .retain(|k, _| k == "plugs");
    value["routines"] =
        json!({"leave_house":{"steps":[{"action":"outlet","entity":"switch.outlet_1"}]}});
    let path = t.config("home.json", &value)?;
    let bundle = Bundle::load(&path)?.expect("bundle");
    let mut devices = bundle.open_devices(None)?;
    assert_eq!(
        entity::call(&mut devices, "routine_list", &json!({}))?["routines"][0]["routine"],
        "leave_house"
    );
    for flag in ["include_pc", "include_kitchen"] {
        let args = json!({"routine":"leave_house",flag:true});
        for tool in ["routine_plan", "routine_run"] {
            assert!(
                routine::call(&mut devices, tool, &args)
                    .expect_err("Unconfigured optional group must fail before I/O")
                    .to_string()
                    .contains("not configured")
            );
        }
    }
    for name in ["leave_house", "sleep"] {
        let reply=homewire::mcp::handle_capabilities(json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"routine_run","arguments":{"routine":name}}}),devices.selection(),false,|_,_|panic!("Control must be blocked before dispatch")).expect("reply");
        assert_eq!(reply["result"]["isError"], true);
    }
    Ok(())
}
