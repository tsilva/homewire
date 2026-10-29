use homewire::{Result, bundle::Bundle, entity, routine};
use serde_json::{Value, json};
#[path = "support/mod.rs"]
#[allow(dead_code)]
mod support;

fn fixture() -> Value {
    let plugs=["sample_load","excluded_load","pc_supply","display_supply"].into_iter().enumerate()
        .map(|(i,name)|json!({"name":name,"model":"P100","host":format!("192.0.2.{}",30+i),"device_id":format!("synthetic{i}"),"auth_hash":"22".repeat(32)})).collect::<Vec<_>>();
    json!({"version":1,"adapters":{
        "plugs":{"plugs":plugs},
        "lights":{"lights":[{"name":"sample_light","model":"L430C","host":"192.0.2.20","device_id":"syntheticlight","auth_hash":"22".repeat(32)}]},
        "meaco":{"device_id":"synthetichumidity","host":"192.0.2.1","local_key":"0123456789abcdef","protocol_version":"3.3"},
        "computer":{"name":"workstation","address":"192.0.2.50:39817","server_public_key":"22".repeat(32),"signing_key":"11".repeat(32),"mac":"02:00:00:00:00:50","wake_address":"192.0.2.255:9"},
        "tcl":{"host":"192.0.2.40","model":"Smart TV","server_cert_sha256":"22".repeat(32),"client_cert":"cert.pem","client_key":"key.pem"}},
        "routines":{"computer":{"plug":"pc_supply","startup_seconds":5,"ready_seconds":120,"shutdown_policy":"leave_on"},
            "tv":{"target":"tcl","plug":"display_supply","startup_seconds":5,"ready_seconds":120},
            "wake_up":{"excluded_entities":["switch.excluded_load"],"steps":[
                {"action":"routine","name":"tv_on"},{"action":"routine","name":"computer_on"},
                {"action":"power_on","entity":"switch.sample_load"},
                {"action":"power_on","entity":"light.sample_light"},
                {"action":"power_on","entity":"dehumidifier.meaco"}]}}})
}
#[test]
fn startup_plan_is_offline_excludes_the_protected_load_and_rejects_options() -> Result<()> {
    let t = support::Directory::new()?;
    t.certs()?;
    let config = fixture();
    let path = t.config("home.json", &config)?;
    let bundle = Bundle::load(&path)?.expect("bundle");
    let mut devices = bundle.open_devices(None)?;
    let list = entity::call(&mut devices, "routine_list", &json!({}))?;
    assert!(
        list["routines"]
            .as_array()
            .expect("routines")
            .iter()
            .any(|r| r["routine"] == "wake_up")
    );
    let reply = entity::call(&mut devices, "routine_plan", &json!({"routine":"wake_up"}))?;
    assert_eq!(
        reply["result"]["steps"],
        config["routines"]["wake_up"]["steps"]
    );
    assert_eq!(
        reply["result"]["excluded_entities"],
        json!(["switch.excluded_load"])
    );
    assert_eq!(reply["result"]["live_state_checked"], false);
    assert_eq!(reply["result"]["preserve_other_settings"], true);
    assert_eq!(reply["result"]["execution"], "concurrent_best_effort");
    for key in ["include_pc", "include_kitchen", "all"] {
        for tool in ["routine_plan", "routine_run"] {
            assert!(
                routine::call(&mut devices, tool, &json!({"routine":"wake_up",key:true})).is_err()
            );
        }
    }
    let denied=homewire::mcp::handle_capabilities(json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"routine_run","arguments":{"routine":"wake_up"}}}),devices.selection(),false,|_,_|panic!("Control must not dispatch")).expect("reply");
    assert_eq!(denied["result"]["isError"], true);
    Ok(())
}
#[test]
fn exclusions_protect_physical_aliases_and_child_routine_outlets() -> Result<()> {
    for id in [
        "switch.sample_load",
        "switch.pc_supply",
        "switch.workstation",
        "switch.display_supply",
        "media_player.tcl",
    ] {
        let t = support::Directory::new()?;
        t.certs()?;
        let mut config = fixture();
        config["routines"]["wake_up"]["excluded_entities"] = json!([id]);
        let path = t.config("home.json", &config)?;
        let bundle = Bundle::load(&path)?.expect("bundle");
        let failure = bundle
            .open_devices(None)
            .err()
            .expect("Excluded device cannot be powered by any route");
        assert!(failure.to_string().contains("excluded physical device"));
    }
    Ok(())
}
#[test]
fn unknown_duplicate_owned_and_non_power_targets_fail_before_io() -> Result<()> {
    for step in [
        json!({"action":"power_on","entity":"switch.missing"}),
        json!({"action":"power_on","entity":"sensor.meaco_humidity"}),
        json!({"action":"power_on","entity":"switch.pc_supply"}),
        json!({"action":"power_on","entity":"media_player.tcl"}),
        json!({"action":"power_on","entity":"switch.sample_load"}),
        json!({"action":"routine","name":"tv_on"}),
        json!({"action":"routine","name":"sleep"}),
        json!({"action":"routine","name":"computer_off"}),
    ] {
        let t = support::Directory::new()?;
        t.certs()?;
        let mut config = fixture();
        config["routines"]["wake_up"]["steps"]
            .as_array_mut()
            .expect("steps")
            .push(step);
        let path = t.config("home.json", &config)?;
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
fn explicit_lists_are_bounded_and_exclusions_are_required() -> Result<()> {
    for exclusions in [
        json!([]),
        json!(["switch.missing"]),
        json!(["switch.excluded_load", "switch.excluded_load"]),
    ] {
        let t = support::Directory::new()?;
        t.certs()?;
        let mut config = fixture();
        config["routines"]["wake_up"]["excluded_entities"] = exclusions;
        let path = t.config("home.json", &config)?;
        assert!(
            Bundle::load(&path)?
                .expect("bundle")
                .open_devices(None)
                .is_err()
        );
    }
    let t = support::Directory::new()?;
    t.certs()?;
    let mut config = fixture();
    config["routines"]["wake_up"]["steps"] = json!(vec![
        json!({"action":"power_on","entity":"switch.sample_load"});
        17
    ]);
    let path = t.config("home.json", &config)?;
    assert!(
        Bundle::load(&path)?
            .expect("bundle")
            .open_devices(None)
            .is_err()
    );
    Ok(())
}

#[test]
fn wake_up_keeps_excluded_printers_and_their_supplies_out_of_startup() -> Result<()> {
    let t = support::Directory::new()?;
    t.certs()?;
    let mut config = fixture();
    config["adapters"]["printers"] = json!({"printers":[
        {"name":"sample_a","host":"192.0.2.10"},
        {"name":"sample_b","host":"192.0.2.11"}
    ]});
    let protected = [
        "printer.sample_a",
        "printer.sample_b",
        "switch.sample_load",
        "switch.excluded_load",
    ];
    config["routines"]["wake_up"]["excluded_entities"] = json!(protected);
    config["routines"]["wake_up"]["steps"]
        .as_array_mut()
        .expect("steps")
        .retain(|step| step["entity"] != "switch.sample_load");
    let path = t.config("home.json", &config)?;
    {
        let mut devices = Bundle::load(&path)?.expect("bundle").open_devices(None)?;
        let plan = routine::call(&mut devices, "routine_plan", &json!({"routine":"wake_up"}))?;
        assert_eq!(plan["result"]["live_state_checked"], false);
        assert_eq!(plan["result"]["excluded_entities"], json!(protected));
        assert_eq!(
            plan["result"]["steps"],
            config["routines"]["wake_up"]["steps"]
        );
        assert!(
            plan["result"]["steps"]
                .as_array()
                .expect("steps")
                .iter()
                .all(|step| !protected.iter().any(|id| step["entity"] == *id))
        );
    }
    for target in protected {
        let mut invalid = config.clone();
        invalid["routines"]["wake_up"]["steps"]
            .as_array_mut()
            .expect("steps")
            .push(json!({"action":"power_on","entity":target}));
        let bad = t.config("invalid.json", &invalid)?;
        let result = Bundle::load(&bad)?.expect("bundle").open_devices(None);
        assert!(
            result.is_err(),
            "Printer or excluded supply must fail before device I/O"
        );
    }
    Ok(())
}
