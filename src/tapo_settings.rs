//! Shared KLAP settings: read-modify-write preserves unrelated device settings.
use crate::{Result, error, klap, settings::Setting};
use serde_json::{Value, json};

pub(crate) struct Rule {
    pub setting: Setting,
    pub getter: &'static str,
    pub setter: &'static str,
    pub pointer: &'static str,
}
impl Rule {
    pub const fn new(
        setting: Setting,
        getter: &'static str,
        setter: &'static str,
        pointer: &'static str,
    ) -> Self {
        Self {
            setting,
            getter,
            setter,
            pointer,
        }
    }
}
pub(crate) const AUTO_UPDATE: Rule = Rule::new(
    Setting::boolean("auto_update"),
    "get_auto_update_info",
    "set_auto_update_info",
    "/enable",
);
pub(crate) const AUTO_OFF: Rule = Rule::new(
    Setting::boolean("auto_off"),
    "get_auto_off_config",
    "set_auto_off_config",
    "/enable",
);
pub(crate) const AUTO_OFF_MINUTES: Rule = Rule::new(
    Setting::integer("auto_off_minutes", 1, 1440, 1),
    "get_auto_off_config",
    "set_auto_off_config",
    "/delay_min",
);
pub(crate) const LED: Rule = Rule::new(
    Setting::choice("indicator_led", &["always", "never", "night_mode"]),
    "get_led_info",
    "set_led_info",
    "/led_rule",
);
pub(crate) const PROTECTION: Rule = Rule::new(
    Setting::integer("protection_w", 0, 3680, 1),
    "get_protection_power",
    "set_protection_power",
    "/protection_power",
);
pub(crate) const SMOOTH: Rule = Rule::new(
    Setting::boolean("smooth_transitions"),
    "get_on_off_gradually_info",
    "set_on_off_gradually_info",
    "/enable",
);
pub(crate) const FADE_ON: Rule = Rule::new(
    Setting::integer("fade_on_seconds", 0, 60, 1),
    "get_on_off_gradually_info",
    "set_on_off_gradually_info",
    "/on_state/duration",
);
pub(crate) const FADE_OFF: Rule = Rule::new(
    Setting::integer("fade_off_seconds", 0, 60, 1),
    "get_on_off_gradually_info",
    "set_on_off_gradually_info",
    "/off_state/duration",
);

struct Mutation {
    getter: &'static str,
    setter: &'static str,
    params: Value,
    checks: Vec<(String, Value)>,
}
pub(crate) fn configure(
    client: &mut klap::Client,
    rules: &[Rule],
    settings: &Value,
) -> Result<Value> {
    crate::settings::validate(
        &rules.iter().map(|r| r.setting).collect::<Vec<_>>(),
        settings,
    )?;
    if settings.get("smooth_transitions").is_some()
        && ["fade_on_seconds", "fade_off_seconds"]
            .iter()
            .any(|s| settings.get(*s).is_some())
    {
        return Err(error(
            "Set either smooth_transitions or explicit fade durations per request",
        ));
    }
    let mut changes: Vec<Mutation> = Vec::new();
    for (name, value) in settings
        .as_object()
        .ok_or_else(|| error("Expected settings"))?
    {
        let rule = rules
            .iter()
            .find(|r| r.setting.name == name)
            .ok_or_else(|| error("Unsupported setting"))?;
        let index = if let Some(i) = changes.iter().position(|c| c.getter == rule.getter) {
            i
        } else {
            let query = (rule.getter == "get_auto_off_config").then(|| json!({"start_index":0}));
            changes.push(Mutation {
                getter: rule.getter,
                setter: rule.setter,
                params: client.query(rule.getter, query)?,
                checks: Vec::new(),
            });
            changes.len() - 1
        };
        let change = &mut changes[index];
        if name == "protection_w" {
            let max = client.read("get_max_power")?["max_power"]
                .as_u64()
                .ok_or_else(|| error("Missing live protection range"))?;
            if value.as_u64().is_none_or(|n| n > max) {
                return Err(error("Protection threshold exceeds live maximum"));
            }
            let keys: Vec<_> = change
                .params
                .as_object()
                .ok_or_else(|| error("Invalid protection settings"))?
                .keys()
                .filter(|k| k.contains("enabled"))
                .cloned()
                .collect();
            if keys.len() != 1 || !change.params[&keys[0]].is_boolean() {
                return Err(error("Missing protection enable flag"));
            }
            let key = &keys[0];
            let enabled = value != 0;
            change.params[key] = json!(enabled);
            change.checks.push((format!("/{key}"), json!(enabled)));
            if !enabled {
                continue;
            }
        }
        if name == "smooth_transitions" && change.params.get("on_state").is_some() {
            for key in ["on_state", "off_state"] {
                let pointer = format!("/{key}/enable");
                let current = change
                    .params
                    .pointer_mut(&pointer)
                    .ok_or_else(|| error("Missing fade enable flag"))?;
                if !current.is_boolean() {
                    return Err(error("Invalid fade enable flag"));
                }
                *current = value.clone();
                change.checks.push((pointer, value.clone()));
            }
        } else {
            let current = change
                .params
                .pointer_mut(rule.pointer)
                .ok_or_else(|| error(format!("Device does not report setting {name}")))?;
            if current.is_boolean() != value.is_boolean()
                || current.is_string() != value.is_string()
                || current.is_number() != value.is_number()
                || current.is_null()
            {
                return Err(error("Invalid setting read-back type"));
            }
            *current = value.clone();
            change.checks.push((rule.pointer.into(), value.clone()));
            if matches!(name.as_str(), "fade_on_seconds" | "fade_off_seconds") {
                let key = if name == "fade_on_seconds" {
                    "on_state"
                } else {
                    "off_state"
                };
                let max = change.params[key]["max_duration"].as_u64().unwrap_or(60);
                if value.as_u64().is_none_or(|n| n > max) {
                    return Err(error("Fade exceeds live maximum"));
                }
                let enabled = json!(value.as_u64() != Some(0));
                change.params[key]["enable"] = enabled.clone();
                change.checks.push((format!("/{key}/enable"), enabled));
            }
        }
    }
    for change in changes {
        let mut params = change.params;
        if change.setter == "set_on_off_gradually_info" && params.get("on_state").is_some() {
            params = json!({"on_state":{"enable":params["on_state"]["enable"],"duration":params["on_state"]["duration"]},"off_state":{"enable":params["off_state"]["enable"],"duration":params["off_state"]["duration"]}});
        }
        client.query(change.setter, Some(params))?;
        let query = (change.getter == "get_auto_off_config").then(|| json!({"start_index":0}));
        let observed = client.query(change.getter, query)?;
        if change
            .checks
            .iter()
            .any(|(pointer, value)| observed.pointer(pointer) != Some(value))
        {
            return Err(error("Setting verification failed; no retry"));
        }
    }
    Ok(json!({"verified":true}))
}
