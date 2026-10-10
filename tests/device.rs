#[path = "support/mod.rs"]
mod support;
use homewire::{
    Result, awair, bulb,
    device::{Adapter, Devices, Selection},
    meaco, plug, printer,
};
use serde_json::{Value, json};
use std::{collections::BTreeSet, fs, path::PathBuf};

struct Configs(support::Directory);
impl Configs {
    fn new() -> Result<Self> {
        let root = Self(support::Directory::new()?);
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
            root.0.config(&format!("{name}.json"), &value)?;
        }
        Ok(root)
    }
    fn path(&self, name: &str) -> PathBuf {
        self.0.0.join(format!("{name}.json"))
    }
    fn open<A: Adapter>(&self, name: &str) -> Result<A> {
        A::open(&self.path(name))
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
        ("plug_list", "plugs", 3),
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
        .with_ymd_and_hms(2026, 10, 8, 9, 0, 0)
        .single()
        .expect("configured UTC 09:00");
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

#[test]
fn missing_presets_expose_only_off_and_never_query_for_unconfigured_runs() -> Result<()> {
    let configs = Configs::new()?;
    let path = configs.path("meaco");
    let mut value: Value = serde_json::from_slice(&fs::read(&path)?)?;
    let object = value.as_object_mut().expect("fixture");
    object.remove("presets");
    object.remove("schedule");
    fs::write(&path, value.to_string())?;
    let mut devices = Devices {
        meaco: Some(meaco::Manager::open(&path)?),
        ..Devices::default()
    };
    let list = homewire::routine::call(&mut devices, "routine_list", &json!({}))?;
    assert_eq!(list["routines"].as_array().expect("list").len(), 1);
    assert_eq!(list["routines"][0]["routine"], "meaco_off");
    assert!(
        homewire::routine::call(&mut devices, "routine_run", &json!({"routine":"meaco_day"}))
            .expect_err("missing preset")
            .to_string()
            .contains("Unknown configured routine")
    );
    Ok(())
}

#[test]
fn android_inventory_matches_static_runtime_families() -> Result<()> {
    let rows: Vec<Value> = serde_json::from_str(include_str!("../scripts/android/catalog.json"))?;
    let mut actual = BTreeSet::new();
    for row in rows {
        let name = row["runtime"]
            .as_str()
            .or_else(|| row["name"].as_str())
            .expect("family");
        let section = row["section"].as_str().expect("section");
        assert!(
            actual.insert((name.to_owned(), section.to_owned())),
            "duplicate family"
        );
    }
    // API is a frontend rather than an Adapter; TV has its own composition schema.
    let mut expected: BTreeSet<_> = Devices::CONFIG_FAMILIES
        .iter()
        .filter(|(name, _)| *name != "tv")
        .map(|(name, section)| (name.to_string(), section.to_string()))
        .collect();
    expected.insert(("api".into(), "services".into()));
    assert_eq!(actual, expected);
    Ok(())
}
