//! Standard, statically borrowed device settings. Protocols stay in adapters.
use crate::{Result, error};
use serde_json::{Value, json};

#[derive(Clone, Copy)]
pub enum Kind {
    Boolean,
    Integer { min: i64, max: i64, step: i64 },
    Choice(&'static [&'static str]),
    Text { max: usize },
    Action,
}
#[derive(Clone, Copy)]
pub struct Setting {
    pub name: &'static str,
    pub kind: Kind,
}
impl Setting {
    pub const fn boolean(name: &'static str) -> Self {
        Self {
            name,
            kind: Kind::Boolean,
        }
    }
    pub const fn integer(name: &'static str, min: i64, max: i64, step: i64) -> Self {
        Self {
            name,
            kind: Kind::Integer { min, max, step },
        }
    }
    pub const fn choice(name: &'static str, values: &'static [&'static str]) -> Self {
        Self {
            name,
            kind: Kind::Choice(values),
        }
    }
    pub const fn action(name: &'static str) -> Self {
        Self {
            name,
            kind: Kind::Action,
        }
    }
    fn accepts(self, value: &Value) -> bool {
        match self.kind {
            Kind::Boolean => value.is_boolean(),
            Kind::Integer { min, max, step } => value
                .as_i64()
                .is_some_and(|n| (min..=max).contains(&n) && step > 0 && (n - min) % step == 0),
            Kind::Choice(values) => value.as_str().is_some_and(|s| values.contains(&s)),
            Kind::Text { max } => value.as_str().is_some_and(|s| {
                !s.is_empty() && s.len() <= max && !s.chars().any(char::is_control)
            }),
            Kind::Action => value == &Value::Bool(true),
        }
    }
    fn schema(self) -> Value {
        match self.kind {
            Kind::Boolean => json!({"type":"boolean"}),
            Kind::Integer { min, max, step } => {
                json!({"type":"integer","minimum":min,"maximum":max,"multipleOf":step})
            }
            Kind::Choice(values) => json!({"type":"string","enum":values}),
            Kind::Text { max } => json!({"type":"string","minLength":1,"maxLength":max}),
            Kind::Action => json!({"type":"boolean","const":true,"action":true}),
        }
    }
}
pub fn schema(settings: &[Setting]) -> Value {
    let mut properties = serde_json::Map::new();
    for field in settings {
        properties.insert(field.name.into(), field.schema());
    }
    json!({"type":"object","minProperties":1,"properties":properties,"additionalProperties":false})
}
pub fn validate(settings: &[Setting], value: &Value) -> Result<()> {
    let object = value
        .as_object()
        .filter(|o| !o.is_empty() && o.len() <= 16)
        .ok_or_else(|| error("Specify 1–16 advertised settings"))?;
    for (name, value) in object {
        if !settings.iter().any(|s| s.name == name && s.accepts(value)) {
            return Err(error(format!("Unsupported or invalid setting: {name}")));
        }
    }
    Ok(())
}
