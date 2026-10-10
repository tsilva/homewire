use homewire::{
    Result, alerts, api,
    device::{Adapter, Devices},
    entity, history, mobile,
    settings::Setting,
    shopping,
};
use serde_json::json;
mod support {
    include!("support/mod.rs");
}
use std::{
    io::{BufRead, BufReader, Read, Write},
    net::TcpListener,
    thread,
};
fn endpoint(status: u16) -> Result<(String, thread::JoinHandle<Result<()>>)> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let url = format!("http://{}/notify", listener.local_addr()?);
    let worker = thread::spawn(move || -> Result<()> {
        let (mut s, _) = listener.accept()?;
        s.set_read_timeout(Some(std::time::Duration::from_secs(3)))?;
        let mut r = BufReader::new(&mut s);
        let mut line = String::new();
        let mut len = 0;
        loop {
            line.clear();
            r.read_line(&mut line)?;
            if line == "\r\n" {
                break;
            }
            if let Some(v) = line.strip_prefix("Content-Length: ") {
                len = v.trim().parse::<usize>()?;
            }
        }
        let mut bytes = vec![0; len];
        r.read_exact(&mut bytes)?;
        drop(r);
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&bytes)?["key"],
            "test"
        );
        write!(
            s,
            "HTTP/1.1 {status} Response\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{{}}"
        )?;
        Ok(())
    });
    Ok((url, worker))
}
#[test]
fn failed_channel_does_not_starve_other_channels_and_receipts_survive_restart() -> Result<()> {
    let t = support::Directory::new()?;
    let _ = t.certs()?;
    let (bad, w1) = endpoint(500)?;
    let (good, w2) = endpoint(200)?;
    let path = t.config(
        "alerts.json",
        &json!({"channels":[{"name":"bad","url":bad},{"name":"good","url":good}]}),
    )?;
    let mut a = alerts::Manager::open(&path)?;
    a.record("test", true, "Device problem", "tank full", 100)?;
    assert!(a.deliver(100).is_err());
    assert_eq!(a.deliver(101)?, 1);
    w1.join().expect("server")?;
    w2.join().expect("server")?;
    assert_eq!(a.list()["alerts"][0]["pending_channels"], json!(["bad"]));
    let card = a.companion_list("phone")["alerts"][0].clone();
    assert!(
        a.acknowledge("phone", &json!({"key":"test","revision":"wrong"}))
            .is_err()
    );
    a.acknowledge("phone", &json!({"key":"test","revision":card["revision"]}))?;
    drop(a);
    let mut a = alerts::Manager::open(&path)?;
    assert_eq!(a.companion_list("phone")["alerts"], json!([]));
    assert_eq!(a.list()["alerts"][0]["pending_channels"], json!(["bad"]));
    a.record("test", false, "Device problem", "", 110)?;
    assert_eq!(a.companion_list("phone")["alerts"][0]["active"], false);
    assert_ne!(
        a.companion_list("phone")["alerts"][0]["revision"],
        card["revision"]
    );
    Ok(())
}
#[test]
fn history_wrap_restart_torn_record_and_bounded_queries() -> Result<()> {
    let t = support::Directory::new()?;
    let path = t.config(
        "history.json",
        &json!({"entities":["sensor.energy"],"max_points":96}),
    )?;
    let mut h = history::Manager::open(&path)?;
    for n in 1..=110 {
        h.record("sensor.energy", 10000 + n, n as f64)?;
    }
    let q = h.query(&json!({"entity":"sensor.energy"}))?;
    assert_eq!(q["points"].as_array().expect("points").len(), 96);
    assert_eq!(q["points"][0]["value"], 15.0);
    assert_eq!(q["points"][95]["value"], 110.0);
    assert_eq!(
        std::fs::metadata(path.with_extension("history").join("sensor.energy.bin"))?.len(),
        96 * 32
    );
    assert!(
        h.query(&json!({"entity":"sensor.energy","limit":100000}))
            .is_err()
    );
    assert!(
        h.query(&json!({"entity":"sensor.energy","start":20,"end":10}))
            .is_err()
    );
    drop(h);
    let file = path.with_extension("history").join("sensor.energy.bin");
    let mut bytes = std::fs::read(&file)?;
    bytes[24] ^= 1;
    std::fs::write(&file, bytes)?;
    let mut h = history::Manager::open(&path)?;
    h.record("sensor.energy", 10111, 111.0)?;
    let q = h.query(&json!({"entity":"sensor.energy","aggregation":"hour"}))?;
    assert_eq!(q["points"][0]["max"], 111.0);
    Ok(())
}
#[test]
fn companion_requires_paired_api_scope_and_preserves_uncertain_location() -> Result<()> {
    let t = support::Directory::new()?;
    let path = t.config(
        "mobile.json",
        &json!({"devices":["phone"],"latitude":0.0,"longitude":0.0}),
    )?;
    let mut d = Devices {
        mobile: Some(mobile::Manager::open(&path)?),
        ..Devices::default()
    };
    let now = chrono::Utc::now().timestamp();
    let data = json!({"battery_percent":60,"battery_state":"charging","charger":"usb","location":{"latitude":0.0,"longitude":0.0,"accuracy_m":500,"measured_at":now}});
    assert!(entity::call(&mut d, "mobile_report", &data).is_err());
    let request =
        json!({"id":1,"params":{"role":"mobile:phone","route":"/api/mobile/report","data":data}});
    assert!(
        api::dispatch(&mut d, request, false)
            .expect("reply")
            .get("error")
            .is_none()
    );
    let s = entity::call(
        &mut d,
        "entity_status",
        &json!({"entities":["device_tracker.phone","sensor.phone_battery"]}),
    )?;
    assert_eq!(s["states"][0]["state"]["home"], serde_json::Value::Null);
    assert_eq!(s["states"][1]["state"], 60);
    let denied=api::dispatch(&mut d,json!({"id":1,"params":{"role":"mobile:phone","route":"/api/call","data":{"name":"entity_list","arguments":{}}}}),true).expect("reply");
    assert!(denied.get("error").is_some());
    assert!(
        d.mobile
            .as_mut()
            .expect("mobile")
            .report("wrong", &json!({"battery_percent":1}), now)
            .is_err()
    );
    Ok(())
}
#[test]
fn typed_settings_and_paginated_discovery_reject_unadvertised_fields() -> Result<()> {
    assert!(
        homewire::settings::validate(
            &[Setting::integer("humidity", 35, 70, 5)],
            &json!({"humidity":36})
        )
        .is_err()
    );
    assert!(
        homewire::settings::validate(&[Setting::boolean("enabled")], &json!({"rpc":"reboot"}))
            .is_err()
    );
    let t = support::Directory::new()?;
    let path = t.config(
        "mobile.json",
        &json!({"devices":["a","b"],"latitude":0.0,"longitude":0.0}),
    )?;
    let mut d = Devices {
        mobile: Some(mobile::Manager::open(&path)?),
        ..Devices::default()
    };
    let p = entity::call(&mut d, "entity_list", &json!({"limit":3}))?;
    assert_eq!(p["total"], 10);
    assert_eq!(p["next_offset"], 3);
    let p = entity::call(&mut d, "entity_list", &json!({"domain":"device_tracker"}))?;
    assert_eq!(p["total"], 2);
    assert!(entity::call(&mut d, "entity_list", &json!({"limit":100})).is_err());
    Ok(())
}
#[test]
fn shopping_ids_completion_and_state_are_durable_and_gated() -> Result<()> {
    let t = support::Directory::new()?;
    let path = t.config("shopping.json", &json!({}))?;
    let mut d = Devices {
        shopping: Some(shopping::Manager::open(&path)?),
        ..Devices::default()
    };
    entity::call(
        &mut d,
        "shopping_update",
        &json!({"action":"add","name":"Coffee"}),
    )?;
    entity::call(
        &mut d,
        "shopping_update",
        &json!({"action":"add","name":"coffee"}),
    )?;
    let list = entity::call(&mut d, "shopping_list", &json!({}))?;
    assert_eq!(list["items"].as_array().expect("items").len(), 1);
    let denied=api::dispatch(&mut d,json!({"id":1,"params":{"role":"dashboard","route":"/api/call","data":{"name":"shopping_update","arguments":{"action":"check","id":1,"complete":true}}}}),false).expect("reply");
    assert!(denied.get("error").is_some());
    entity::call(
        &mut d,
        "shopping_update",
        &json!({"action":"check","id":1,"complete":true}),
    )?;
    drop(d);
    let mut s = shopping::Manager::open(&path)?;
    assert_eq!(
        s.execute("shopping_list", &json!({}))?["items"][0]["complete"],
        true
    );
    Ok(())
}
