pub mod awair;
pub mod bulb;
pub mod config;
pub mod device;
pub mod fault;
mod http;
pub mod ipp;
pub mod klap;
pub mod light_schedule;
pub mod mcp;
pub mod meaco;
pub mod naming;
pub mod plug;
pub mod printer;
pub mod schedule;
pub mod solar;
pub mod tuya;

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

pub fn error(message: impl Into<String>) -> Box<dyn std::error::Error + Send + Sync> {
    std::io::Error::other(message.into()).into()
}
