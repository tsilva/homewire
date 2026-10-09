pub mod alerts;
pub mod awair;
pub mod bulb;
pub mod config;
pub mod device;
pub mod entity;
pub mod fault;
pub mod history;
mod http;
#[cfg(unix)]
pub mod ipc;
pub mod ipp;
pub mod klap;
pub mod light_schedule;
pub mod mcp;
pub mod meaco;
pub mod naming;
pub mod plug;
pub mod printer;
pub mod router;
pub mod routine;
pub mod schedule;
pub mod settings;
pub mod shopping;
pub mod solar;
pub mod tuya;
pub mod weather;

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

pub fn error(message: impl Into<String>) -> Box<dyn std::error::Error + Send + Sync> {
    std::io::Error::other(message.into()).into()
}

pub mod androidtv;
mod media;
mod protobuf;
pub mod samsung;
mod tapo_settings;
mod transport;
mod web;
mod websocket;

pub use routine::tv;

#[cfg(unix)]
pub mod api;
pub mod mobile;

pub mod cast;
pub mod dlna;
mod playback;

pub mod brother;
pub mod convenience;

pub mod setup;

pub mod bundle;
