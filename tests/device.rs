use homewire::{
    Result, awair, bulb,
    device::{Adapter, Devices, Selection},
    meaco, plug, printer,
};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    fs,
    path::PathBuf,
    sync::atomic::{AtomicUsize, Ordering},
};

struct Configs(PathBuf);
impl Configs {
    fn new() -> Result<Self> {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let nonce = NEXT.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("homewire-catalog-{}-{nonce}", std::process::id()));
        fs::create_dir(&path)?;
        // Construct the cleanup guard only after this fixture owns the directory.
        let root = Self(path);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&root.0, fs::Permissions::from_mode(0o700))?;
        }
        for (name, source) in [
            ("meaco", include_str!("../examples/meaco.json")),
            ("bulbs", include_str!("../examples/lights.json")),
            ("plugs", include_str!("../examples/plugs.json")),
            ("printers", include_str!("../examples/printers.json")),
            ("awair", include_str!("../examples/awair.json")),
        ] {
            let mut value: Value = serde_json::from_str(source)?;
            value
                .as_object_mut()
                .expect("configuration")
                .remove("automation");
            if name == "meaco" {
                value["local_key"] = json!("0123456789abcdef");
                value["host"] = json!("127.0.0.1");
                value["port"] = json!(1);
            }
            for group in ["lights", "plugs", "printers"] {
                if let Some(endpoints) = value.get_mut(group).and_then(Value::as_array_mut) {
                    for endpoint in endpoints {
                        endpoint["connect"] = json!("127.0.0.1:1");
                        if group != "printers" {
                            endpoint["auth_hash"] = json!("00".repeat(32));
                        }
                    }
                }
            }
            let path = root.path(name);
            fs::write(&path, value.to_string())?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
            }
        }
        Ok(root)
    }
    fn path(&self, name: &str) -> PathBuf {
        self.0.join(format!("{name}.json"))
    }
    fn open<A: Adapter>(&self, name: &str) -> Result<A> {
        A::open(&self.path(name))
    }
}
impl Drop for Configs {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn catalog_has_unique_commands_and_resolves_each_adapters_own_metadata() {
    let mut names = BTreeSet::new();
    for tool in Selection::ALL.tools() {
        assert!(
            names.insert(tool.name),
            "duplicate static command: {}",
            tool.name
        );
        assert!(!tool.description.is_empty());
        let schema = tool.input.schema(None);
        assert_eq!(schema["type"], "object");
        assert_eq!(schema["additionalProperties"], false);
        if let Some(required) = schema["required"].as_array() {
            for field in required {
                assert!(
                    schema["properties"]
                        .get(field.as_str().expect("field name"))
                        .is_some()
                );
            }
        }
    }
    assert!(!names.is_empty());
}

#[test]
fn standard_factory_and_catalog_dispatch_keep_validation_before_device_io() -> Result<()> {
    let configs = Configs::new()?;
    let mut devices = Devices {
        meaco: Some(configs.open::<meaco::Manager>("meaco")?),
        bulbs: Some(configs.open::<bulb::Manager>("bulbs")?),
        plugs: Some(configs.open::<plug::Manager>("plugs")?),
        printers: Some(configs.open::<printer::Manager>("printers")?),
        awair: Some(configs.open::<awair::Config>("awair")?),
        ..Devices::default()
    };
    for (tool, key, count) in [
        ("printer_list", "printers", 2),
        ("bulb_list", "bulbs", 3),
        ("plug_list", "plugs", 8),
    ] {
        assert_eq!(
            devices.call(tool, &json!({}))?[key]
                .as_array()
                .expect("devices")
                .len(),
            count
        );
    }
    for (tool, args, expected) in [
        (
            "meaco_apply",
            json!({"operation":"invalid"}),
            "Operation must be day, night or off",
        ),
        (
            "printer_status",
            json!({"printer":"hp","host":"127.0.0.1"}),
            "Expected only a configured printer name",
        ),
        (
            "bulb_apply",
            json!({"bulb":"a","settings":{"brightness_percent":0}}),
            "Invalid bulb settings",
        ),
        (
            "plug_apply",
            json!({"plug":"a","power":"on"}),
            "Expected configured plug name and boolean power for apply",
        ),
        (
            "awair_status",
            json!({"host":"127.0.0.1"}),
            "This tool takes no arguments",
        ),
    ] {
        assert_eq!(
            devices
                .call(tool, &args)
                .expect_err("reject before I/O")
                .to_string(),
            expected
        );
    }
    assert!(
        configs.open::<meaco::Manager>("meaco").is_err(),
        "factory owns the exclusive lock"
    );
    Ok(())
}

#[test]
fn unconfigured_families_have_no_commands_and_cannot_be_dispatched() {
    let mut devices = Devices::default();
    assert_eq!(devices.selection().tools().count(), 0);
    for name in [
        "meaco_status",
        "bulb_list",
        "plug_list",
        "printer_list",
        "awair_status",
        "unknown",
    ] {
        assert!(devices.call(name, &json!({})).is_err());
    }
}

#[test]
fn meaco_adapter_owns_durable_schedule_attempts_before_execution() -> Result<()> {
    use chrono::{TimeZone, Utc};
    let configs = Configs::new()?;
    let now = Utc
        .with_ymd_and_hms(2026, 10, 8, 8, 0, 0)
        .single()
        .expect("London 09:00");
    let mut meaco = configs.open::<meaco::Manager>("meaco")?;
    assert_eq!(meaco.scheduled_operation(now)?, Some(meaco::Operation::Day));
    assert_eq!(meaco.scheduled_operation(now)?, None);
    drop(meaco);
    let mut restarted = configs.open::<meaco::Manager>("meaco")?;
    assert_eq!(
        restarted.scheduled_operation(now)?,
        None,
        "restart cannot replay the attempt"
    );
    Ok(())
}
