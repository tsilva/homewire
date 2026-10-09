//! Small statically selected Internet services; playback stays in media adapters.
use crate::{
    Result,
    device::{Adapter, Hints, Input, Tool},
    entity::{Entity, Support},
    error, web,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::time::Duration;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Radio {
    #[serde(default = "server")]
    pub server: String,
    #[serde(default = "timeout")]
    pub timeout_seconds: u64,
}
fn server() -> String {
    "https://de1.api.radio-browser.info".into()
}
fn timeout() -> u64 {
    8
}
impl Adapter for Radio {
    const CONFIG_SECTION: &'static str = "services";
    const TOOLS: &'static [Tool] = &[];
    const SUPPORT: Support = Support::NONE;
    const PUBLIC_TOOLS: &'static [Tool] = &[Tool {
        name: "radio_search",
        description: "Find up to eight Radio Browser stations and their resolved playback URLs; does not start playback.",
        input: Input::Object {
            schema: || json!({"type":"object","properties":{"name":{"type":"string","minLength":1,"maxLength":64}},"required":["name"],"additionalProperties":false}),
            valid: |v| {
                v.as_object().is_some_and(|o| o.len() == 1)
                    && v["name"].as_str().is_some_and(|s| {
                        !s.trim().is_empty() && s.len() <= 64 && !s.chars().any(char::is_control)
                    })
            },
            error: "Expected station name",
        },
        hints: Hints::READ,
    }];
    fn from_source(source: &crate::config::Source<'_>) -> Result<Self> {
        let c: Self = source.decode()?;
        let url = web::Url::parse(&c.server)?;
        if !url.secure
            || url.path != "/"
            || c.server.ends_with('/')
            || !(1..=30).contains(&c.timeout_seconds)
        {
            return Err(error("Radio server requires an HTTPS origin"));
        }
        Ok(c)
    }
    fn entities<'a>(&'a self, _: &mut impl FnMut(Entity<'a>)) {}
    fn execute(&mut self, name: &str, args: &Value) -> Result<Value> {
        if name != "radio_search" {
            return Err(error("Unknown radio operation"));
        }
        Self::PUBLIC_TOOLS[0].input.validate(args).map_err(error)?;
        let url = format!(
            "{}/json/stations/search?name={}&hidebroken=true&limit=8&order=clickcount&reverse=true",
            self.server,
            web::encode(args["name"].as_str().ok_or_else(|| error("Missing name"))?)
        );
        let bytes = web::request(
            &url,
            "GET",
            &[("Accept", "application/json")],
            &[],
            Duration::from_secs(self.timeout_seconds),
        )?
        .body;
        let stations: Vec<Value> = serde_json::from_slice(&bytes)?;
        if stations.len() > 8 {
            return Err(error("Station response exceeds limit"));
        }
        Ok(
            json!({"stations":stations.into_iter().filter(|s|s["url_resolved"].as_str().is_some_and(|u|web::Url::parse(u).is_ok())).map(|s|json!({"name":s["name"],"url":s["url_resolved"],"codec":s["codec"],"bitrate":s["bitrate"],"country":s["countrycode"]})).collect::<Vec<_>>()}),
        )
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Speech {
    #[serde(default = "language")]
    pub language: String,
}
fn language() -> String {
    "en".into()
}
impl Adapter for Speech {
    const CONFIG_SECTION: &'static str = "services";
    const TOOLS: &'static [Tool] = &[];
    const SUPPORT: Support = Support::NONE;
    const PUBLIC_TOOLS: &'static [Tool] = &[Tool {
        name: "tts_media",
        description: "Create a short Google Translate speech MP3 URL for media_play. Playback sends this text to Google; availability depends on that public service.",
        input: Input::Object {
            schema: || json!({"type":"object","properties":{"text":{"type":"string","minLength":1,"maxLength":200}},"required":["text"],"additionalProperties":false}),
            valid: |v| {
                v.as_object().is_some_and(|o| o.len() == 1)
                    && v["text"].as_str().is_some_and(|s| {
                        !s.trim().is_empty() && s.len() <= 200 && !s.chars().any(char::is_control)
                    })
            },
            error: "Expected 1–200 bytes of speech text",
        },
        hints: Hints::LIST,
    }];
    fn from_source(source: &crate::config::Source<'_>) -> Result<Self> {
        let c: Self = source.decode()?;
        if c.language.is_empty()
            || c.language.len() > 12
            || !c
                .language
                .bytes()
                .all(|b| b.is_ascii_alphabetic() || b == b'-')
        {
            return Err(error("Invalid speech language"));
        }
        Ok(c)
    }
    fn entities<'a>(&'a self, _: &mut impl FnMut(Entity<'a>)) {}
    fn execute(&mut self, name: &str, args: &Value) -> Result<Value> {
        if name != "tts_media" {
            return Err(error("Unknown speech operation"));
        }
        Self::PUBLIC_TOOLS[0].input.validate(args).map_err(error)?;
        Ok(
            json!({"url":format!("https://translate.google.com/translate_tts?ie=UTF-8&client=tw-ob&tl={}&q={}",web::encode(&self.language),web::encode(args["text"].as_str().ok_or_else(||error("Missing text"))?)),"content_type":"audio/mpeg"}),
        )
    }
}
