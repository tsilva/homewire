use homewire::{Result, bundle::Bundle, entity, routine};
use serde_json::{Value, json};
#[path = "support/media.rs"]
#[allow(dead_code)]
mod support;

fn config() -> Value {
    json!({"version":1,"adapters":{
        "computer":{"name":"workstation","address":"192.0.2.50:39817","server_public_key":"22".repeat(32),"signing_key":"11".repeat(32),"mac":"02:00:00:00:00:50","wake_address":"192.0.2.255:9","timeout_seconds":1},
        "plugs":{"plugs":[{"name":"computer_outlet","model":"P110","host":"192.0.2.30","device_id":"synthetic","auth_hash":"22".repeat(32)}],"timeout_seconds":5,"settle_seconds":0,"poll_seconds":30}},
        "routines":{"computer":{"plug":"computer_outlet","startup_seconds":5,"ready_seconds":120,"shutdown_policy":"metered_standby","shutdown_verification":{"standby_power_w":1.0,"stable_seconds":30,"timeout_seconds":180,"poll_seconds":2}}}})
}
#[test]
fn metered_plan_and_power_ownership_are_available_without_device_io() -> Result<()> {
    let t = support::Directory::new()?;
    let bundle = Bundle::load(&t.config("home.json", &config())?)?.expect("bundle");
    let mut devices = bundle.open_devices(None)?;
    let plan = routine::call(
        &mut devices,
        "routine_plan",
        &json!({"routine":"computer_off"}),
    )?;
    assert_eq!(plan["result"]["outlet_cutoff_enabled"], true);
    assert_eq!(
        plan["result"]["shutdown_verification"]["standby_power_w"],
        1.0
    );
    for (tool, args) in [
        (
            "switch_set",
            json!({"entity":"switch.workstation","power":false}),
        ),
        (
            "switch_set",
            json!({"entity":"switch.computer_outlet","power":false}),
        ),
        ("wake_on_lan", json!({"device":"computer.workstation"})),
    ] {
        assert!(
            entity::call(&mut devices, tool, &args)
                .expect_err("owned control denied")
                .to_string()
                .contains("routine")
        );
    }
    Ok(())
}
#[test]
fn unsafe_cutoff_settings_and_unmetered_plugs_fail_before_device_io() -> Result<()> {
    let good = config();
    let mut invalid = Vec::new();
    for value in [0.0, 5.01] {
        let mut c = good.clone();
        c["routines"]["computer"]["shutdown_verification"]["standby_power_w"] = json!(value);
        invalid.push(c);
    }
    let mut c = good.clone();
    c["adapters"]["plugs"]["plugs"][0]["model"] = json!("P100");
    invalid.push(c);
    let mut c = good.clone();
    c["routines"]["computer"]
        .as_object_mut()
        .expect("routine")
        .remove("shutdown_verification");
    invalid.push(c);
    let mut c = good.clone();
    c["routines"]["computer"]["shutdown_policy"] = json!("leave_on");
    invalid.push(c);
    let mut c = good.clone();
    c["routines"]["computer"]["shutdown_verification"]["timeout_seconds"] = json!(40);
    invalid.push(c);
    for c in invalid {
        let t = support::Directory::new()?;
        let bundle = Bundle::load(&t.config("home.json", &c)?)?.expect("bundle");
        assert!(bundle.open_devices(None).is_err());
    }
    Ok(())
}
#[test]
fn old_acceptance_is_bound_to_equipment_before_any_verification() -> Result<()> {
    let t = support::Directory::new()?;
    t.config("computer-routine.state.json",&json!({"attempt":chrono::Utc::now().timestamp(),"shutdown_accepted":chrono::Utc::now().timestamp(),"shutdown_identity":"different_equipment"}))?;
    let bundle = Bundle::load(&t.config("home.json", &config())?)?.expect("bundle");
    let mut devices = bundle.open_devices(None)?;
    let e = routine::call(
        &mut devices,
        "routine_run",
        &json!({"routine":"computer_off"}),
    )
    .expect_err("different identity");
    assert!(e.to_string().contains("different equipment"));
    assert!(
        devices.computer_routine.is_some(),
        "routine restored after failure"
    );
    Ok(())
}
