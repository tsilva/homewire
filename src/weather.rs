//! MET Norway forecast access. Small projections, no retained forecast cache.
use crate::{
    Result,
    device::{Adapter, Hints, Input, Tool},
    entity::{Capabilities, Domain, Entity, Support},
    error, web,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::time::Duration;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub latitude: f64,
    pub longitude: f64,
    #[serde(default = "timeout")]
    pub timeout_seconds: u64,
}
fn timeout() -> u64 {
    8
}
impl Config {
    fn status(&self) -> Result<Value> {
        let url = format!(
            "https://api.met.no/weatherapi/locationforecast/2.0/compact?lat={:.4}&lon={:.4}",
            self.latitude, self.longitude
        );
        let response = web::request(
            &url,
            "GET",
            &[("Accept", "application/json")],
            &[],
            Duration::from_secs(self.timeout_seconds),
        )?;
        let value: Value = serde_json::from_slice(&response.body)
            .map_err(|_| error("Invalid weather response"))?;
        let series = value["properties"]["timeseries"]
            .as_array()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| error("Missing weather forecast"))?;
        let current = &series[0];
        let time = current["time"]
            .as_str()
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .ok_or_else(|| error("Missing weather timestamp"))?;
        let age = chrono::Utc::now().timestamp() - time.timestamp();
        if !(-3600..=7200).contains(&age) {
            return Err(error("Weather forecast is stale"));
        }
        let data = &current["data"]["instant"]["details"];
        if !data["air_temperature"].is_number() {
            return Err(error("Missing weather temperature"));
        }
        let forecast:Vec<_>=series.iter().take(12).map(|s|json!({"time":s["time"],"temperature_c":s["data"]["instant"]["details"]["air_temperature"],"symbol":s["data"]["next_1_hours"]["summary"]["symbol_code"],"precipitation_mm":s["data"]["next_1_hours"]["details"]["precipitation_amount"]})).collect();
        Ok(
            json!({"time":current["time"],"temperature_c":data["air_temperature"],"humidity_percent":data["relative_humidity"],"pressure_hpa":data["air_pressure_at_sea_level"],"wind_m_s":data["wind_speed"],"wind_direction_degrees":data["wind_from_direction"],"forecast":forecast,"source":"MET Norway"}),
        )
    }
}
impl Adapter for Config {
    const CONFIG_SECTION: &'static str = "services";
    const TOOLS: &'static [Tool] = &[];
    const SUPPORT: Support = Support::READ;
    const PUBLIC_TOOLS: &'static [Tool] = &[Tool {
        name: "weather_status",
        description: "Read MET Norway weather and twelve forecast steps for the configured home coordinates.",
        input: Input::Empty,
        hints: Hints::READ,
    }];
    fn from_source(source: &crate::config::Source<'_>) -> Result<Self> {
        let c: Self = source.decode()?;
        if !c.latitude.is_finite()
            || !(-90. ..=90.).contains(&c.latitude)
            || !c.longitude.is_finite()
            || !(-180. ..=180.).contains(&c.longitude)
            || !(1..=30).contains(&c.timeout_seconds)
        {
            return Err(error("Invalid weather coordinates/timeout"));
        }
        Ok(c)
    }
    fn entities<'a>(&'a self, visit: &mut impl FnMut(Entity<'a>)) {
        visit(Entity::new(
            crate::device::Family::Weather,
            0,
            Domain::Weather,
            "home",
            "MET Norway",
            Capabilities::Read,
        ));
    }
    fn read_device(&mut self, index: usize) -> Result<Value> {
        if index != 0 {
            return Err(error("Unknown weather entity"));
        }
        self.status()
    }
    fn execute(&mut self, name: &str, _: &Value) -> Result<Value> {
        if name == "weather_status" {
            self.status()
        } else {
            Err(error("Unknown weather operation"))
        }
    }
}
