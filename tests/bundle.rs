use homewire::{
    Result,
    bundle::Bundle,
    config::{Config, Store},
    entity,
};
use serde_json::{Value, json};
use std::{fs, process::Command};
#[path = "support/mod.rs"]
mod support;

fn example(name: &str) -> Result<Value> {
    Ok(serde_json::from_slice(&fs::read(format!(
        "examples/{name}.json"
    ))?)?)
}
fn fixture() -> Result<Value> {
    let mut value = json!({"version":1,"adapters":{},"services":{},"routines":{},"automations":{}});
    for name in [
        "meaco", "lights", "plugs", "printers", "awair", "samsung", "digi", "brother", "cast",
        "dlna",
    ] {
        value["adapters"][name] = example(name)?;
    }
    value["adapters"]["meaco"]["local_key"] = json!("0123456789abcdef");
    value["adapters"]["samsung"]["token"] = json!("00".repeat(16));
    for (family, list) in [("lights", "lights"), ("plugs", "plugs")] {
        for device in value["adapters"][family][list]
            .as_array_mut()
            .expect("devices")
        {
            device["auth_hash"] = json!("00".repeat(32));
        }
    }
    for family in ["digi", "cast"] {
        value["adapters"][family]["server_cert_sha256"] = json!("00".repeat(32));
    }
    value["adapters"]["digi"]["client_cert"] = json!("cert.pem");
    value["adapters"]["digi"]["client_key"] = json!("key.pem");
    let presets = value["adapters"]["meaco"]
        .as_object_mut()
        .expect("fixture")
        .remove("presets")
        .expect("fixture");
    let schedule = value["adapters"]["meaco"]
        .as_object_mut()
        .expect("fixture")
        .remove("schedule")
        .expect("fixture");
    value["routines"]["meaco"] = presets;
    value["automations"]["meaco"] = schedule;
    value["automations"]["lights"] = json!({"light":"led_strip","timezone":"Etc/UTC","latitude":0.0,"longitude":0.0,"curve":[[360,20,3000],[1080,80,5000]],"outside_curve":[20,2700]});
    let mut tv = example("tv")?;
    for family in ["samsung", "digi", "plugs"] {
        tv.as_object_mut().expect("fixture").remove(family);
    }
    value["routines"]["tv"] = tv;
    for name in [
        "mobile", "history", "alerts", "shopping", "weather", "radio", "speech",
    ] {
        value["services"][name] = example(name)?;
    }
    value["services"]["api"] = json!({"listen":"127.0.0.1:0","dashboard_token":"a".repeat(64)});
    Ok(value)
}
#[test]
fn all_static_adapters_open_without_child_files_and_keep_tv_ownership() -> Result<()> {
    let t = support::Directory::new()?;
    t.certs()?;
    let path = t.config("home.json", &fixture()?)?;
    let bundle = Bundle::load(&path)?.expect("unified");
    let mut devices = bundle.open_devices(None)?;
    assert!(bundle.api_settings()?.is_some());
    let _guard = bundle.into_lock();
    assert!(devices.meaco.as_ref().expect("fixture").has_schedule());
    assert!(
        devices
            .bulbs
            .as_ref()
            .expect("lights")
            .config
            .automation
            .is_some()
    );
    assert!(devices.samsung.is_none() && devices.digi.is_none() && devices.plugs.is_none());
    assert!(devices.next_background().is_none());
    devices.enable_monitoring("plugs")?;
    let deadline = devices
        .next_background()
        .expect("TV-owned plug monitor deadline");
    assert!(deadline > std::time::Instant::now());
    devices.background_tick(); // Not due: must not contact synthetic endpoints.
    assert_eq!(devices.next_background(), Some(deadline));
    let list = entity::list(&devices);
    assert!(!list["entities"].as_array().expect("fixture").is_empty());
    for forbidden in ["local_key", "auth_hash", "dashboard_token", "password"] {
        assert!(!list.to_string().contains(forbidden));
    }
    for (tool, args) in [
        (
            "media_power",
            json!({"entity":"media_player.samsung","power":true}),
        ),
        (
            "media_power",
            json!({"entity":"media_player.digi","power":true}),
        ),
        ("switch_set", json!({"entity":"switch.tv","power":false})),
    ] {
        assert!(
            entity::call(&mut devices, tool, &args)
                .expect_err("TV ownership must reject")
                .to_string()
                .contains("TV")
        );
    }
    assert!(!t.0.join("meaco.json").exists());
    assert!(t.0.join("meaco.json.lock").exists());
    assert!(Bundle::load(&path).is_err());
    Ok(())
}
#[test]
fn migration_reads_existing_state_and_excludes_legacy_owner() -> Result<()> {
    let t = support::Directory::new()?;
    let config = json!({"device_id":"fixture","host":"192.0.2.1","local_key":"0123456789abcdef","protocol_version":"3.3"});
    let old = t.config("meaco.json", &config)?;
    let typed = Config::load(&old)?;
    let store = Store::open(&old, &typed.device_id)?;
    let mut memory = store.read()?;
    memory.base_mode = Some("purify".into());
    memory
        .schedule_attempts
        .insert("night".into(), "2026-10-07".into());
    store.save(&memory)?;
    drop(store);
    let bytes = fs::read(old.with_extension("state.json"))?;
    let path = t.config(
        "home.json",
        &json!({"version":1,"adapters":{"meaco":config}}),
    )?;
    let b = Bundle::load(&path)?.expect("fixture");
    let devices = b.open_devices(None)?;
    assert!(Store::open(&old, &typed.device_id).is_err());
    assert_eq!(bytes, fs::read(old.with_extension("state.json"))?);
    drop(devices);
    assert_eq!(
        Store::open(&old, &typed.device_id)?
            .read()?
            .schedule_attempts["night"],
        "2026-10-07"
    );
    Ok(())
}
#[test]
fn invalid_root_or_duplicate_settings_fail_before_io() -> Result<()> {
    let t = support::Directory::new()?;
    for v in [
        json!({"version":2}),
        json!({"version":1,"extra":{}}),
        json!({"version":1,"adapters":{"unsupported":{}}}),
        json!({"version":1,"adapters":{"router":{}}}),
        json!({"version":1,"services":{"meaco":{}}}),
        json!({"version":1,"adapters":{"api":{}}}),
        json!({"version":1,"automations":{"meaco":{}}}),
        json!({"version":1,"automations":{"unknown":{}}}),
        json!({"version":1,"adapters":{"meaco":{"schedule":{}}},"automations":{"meaco":{}}}),
    ] {
        let path = t.config("home.json", &v)?;
        assert!(Bundle::load(&path).is_err());
    }
    let path = t.write("home.json", &vec![b' '; 65537])?;
    assert!(Bundle::load(&path).is_err());
    Ok(())
}
#[test]
fn cli_unified_validation_and_discovery_need_no_network_and_reject_mixed_inputs() -> Result<()> {
    let t = support::Directory::new()?;
    t.certs()?;
    let path = t.config("home.json", &fixture()?)?;
    let binary = env!("CARGO_BIN_EXE_homewire");
    let output = Command::new(binary)
        .args(["--config"])
        .arg(&path)
        .arg("config-check")
        .output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout)?["device_io"],
        false
    );
    let output = Command::new(binary)
        .arg("--config")
        .arg(&path)
        .args(["lights"])
        .output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let output = Command::new(binary)
        .arg("--config")
        .arg(&path)
        .args(["--awair", "missing.json", "config-check"])
        .output()?;
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("cannot be combined"));
    Ok(())
}
