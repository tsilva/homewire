use homewire::{Result, bundle::Bundle, device::Devices, entity, mobile};
use serde_json::{Value, json};
use std::{
    fs, thread,
    time::{Duration, Instant},
};
#[path = "support/mod.rs"]
#[allow(dead_code)]
mod support;

fn configuration(monitored: bool) -> Value {
    let mut config =
        json!({"devices":["host","other"],"latitude":0.0,"longitude":0.0,"max_age_seconds":60});
    if monitored {
        config["battery_monitor"] =
            json!({"devices":["host"],"low_percent":15,"recovery_percent":20,"poll_seconds":60});
    }
    config
}
fn report(devices: &mut Devices, percent: u8) -> Result<()> {
    devices.mobile.as_mut().expect("companion").report(
        "host",
        &json!({"battery_percent":percent,
        "battery_state":"discharging","charger":"none"}),
        chrono::Utc::now().timestamp(),
    )?;
    Ok(())
}
#[test]
fn config_check_needs_no_telemetry_and_monitoring_is_opt_in() -> Result<()> {
    let t = support::Directory::new()?;
    let path = t.config(
        "home.json",
        &json!({"version":1,"services":{"mobile":configuration(false)}}),
    )?;
    let bundle = Bundle::load(&path)?.expect("unified");
    let devices = bundle.open_devices(None)?;
    assert!(devices.next_background().is_none());
    assert!(
        entity::list(&devices)
            .to_string()
            .contains("sensor.host_battery")
    );
    drop(devices);
    drop(bundle);
    for bad in [json!(0), json!(15), json!(101)] {
        let mut value = configuration(true);
        value["battery_monitor"]["recovery_percent"] = bad;
        let path = t.config("mobile.json", &value)?;
        assert!(mobile::Manager::open(&path).is_err());
    }
    let mut value = configuration(true);
    value["battery_monitor"]["devices"] = json!(["unpaired"]);
    let path = t.config("mobile.json", &value)?;
    assert!(mobile::Manager::open(&path).is_err());
    Ok(())
}
#[test]
fn background_battery_detection_routing_and_restart_deduplication() -> Result<()> {
    let t = support::Directory::new()?;
    let path = t.config("mobile.json", &configuration(true))?;
    let alerts_path=t.config("alerts.json",&json!({"companions":["observer"],"channels":[
        {"name":"existing","url":"http://127.0.0.1:1/test","keys":["meaco.meaco","battery.host"]},
        {"name":"other","url":"http://127.0.0.1:1/test","keys":["unrelated.device"]}
    ]}))?;
    let mut devices = Devices::default();
    devices.open_adapter("mobile", &path)?;
    devices.open_adapter("alerts", &alerts_path)?;
    report(&mut devices, 15)?;
    devices.background_tick();
    assert!(
        devices.alerts.as_ref().expect("alerts").list()["alerts"]
            .as_array()
            .expect("cards")
            .is_empty()
    );
    report(&mut devices, 14)?;
    devices.background_tick();
    assert!(devices.next_background().expect("enabled") > Instant::now() + Duration::from_secs(50));
    thread::sleep(Duration::from_millis(3100));
    use homewire::device::Monitored;
    devices.mobile.as_mut().expect("companion").watch()?;
    devices.background_tick(); // Same stored report cannot confirm the warning.
    assert!(
        devices.alerts.as_ref().expect("alerts").list()["alerts"]
            .as_array()
            .expect("cards")
            .is_empty()
    );
    report(&mut devices, 14)?;
    // Drive an explicit check, then reconcile through the same daemon path.
    devices.mobile.as_mut().expect("companion").watch()?;
    devices.background_tick();
    let alerts = devices.alerts.as_mut().expect("alerts");
    let card = alerts.list()["alerts"][0].clone();
    assert_eq!(card["key"], "battery.host");
    assert_eq!(card["active"], true);
    assert_eq!(card["pending_channels"], json!(["existing"]));
    let receipt = alerts.companion_list("observer")["alerts"][0].clone();
    alerts.acknowledge(
        "observer",
        &json!({"key":"battery.host","revision":receipt["revision"]}),
    )?;
    report(&mut devices, 13)?;
    devices.mobile.as_mut().expect("companion").watch()?;
    devices.background_tick();
    assert!(
        devices
            .alerts
            .as_ref()
            .expect("alerts")
            .companion_list("observer")["alerts"]
            .as_array()
            .expect("receipts")
            .is_empty()
    );
    let saved = devices.alerts.as_ref().expect("alerts").list()["alerts"][0].clone();
    drop(devices);
    let mut devices = Devices::default();
    devices.open_adapter("mobile", &path)?;
    devices.open_adapter("alerts", &alerts_path)?;
    devices.background_tick();
    assert_eq!(
        devices.alerts.as_ref().expect("alerts").list()["alerts"][0],
        saved
    );
    report(&mut devices, 19)?;
    devices.mobile.as_mut().expect("companion").watch()?;
    devices.background_tick();
    assert_eq!(
        devices.alerts.as_ref().expect("alerts").list()["alerts"][0],
        saved
    );
    drop(devices);
    let mut telemetry: Value =
        serde_json::from_slice(&fs::read(path.with_extension("state.json"))?)?;
    telemetry["host"]["battery_percent"] = json!(100);
    telemetry["host"]["received_at"] = json!(chrono::Utc::now().timestamp() - 61);
    t.write("mobile.state.json", &serde_json::to_vec(&telemetry)?)?;
    let mut devices = Devices::default();
    devices.open_adapter("mobile", &path)?;
    devices.open_adapter("alerts", &alerts_path)?;
    devices.background_tick();
    assert_eq!(
        devices.alerts.as_ref().expect("alerts").list()["alerts"][0],
        saved
    );
    let status = entity::call(
        &mut devices,
        "entity_status",
        &json!({"entities":["sensor.host_battery"]}),
    )?;
    assert_eq!(status["states"][0]["available"], false);
    Ok(())
}
