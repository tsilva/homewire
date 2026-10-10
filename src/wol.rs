//! Bounded configured wake targets and a shared, one-shot magic-packet sender.
use crate::{
    Result,
    config::{Source, lock_configuration, valid_unique_labels},
    device::{Adapter, Devices, Family, Hints, Input, Selection, Tool},
    entity::{Device, Entity, Support},
    error,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    fs::File,
    net::{SocketAddr, UdpSocket},
};

pub(crate) fn mac_bytes(text: &str) -> Result<[u8; 6]> {
    crate::media::hex::<6>(&text.replace(':', ""))
}

/// Sends exactly once. UDP delivery is not evidence of a successful wake.
pub(crate) fn send(mac: [u8; 6], destination: SocketAddr) -> Result<()> {
    let mut packet = [0xff; 102];
    for block in packet[6..].chunks_exact_mut(6) {
        block.copy_from_slice(&mac);
    }
    let bind = if destination.is_ipv4() {
        "0.0.0.0:0"
    } else {
        "[::]:0"
    };
    let socket = UdpSocket::bind(bind)?;
    socket.set_broadcast(true)?;
    socket.set_write_timeout(Some(std::time::Duration::from_secs(2)))?;
    if socket.send_to(&packet, destination)? != packet.len() {
        return Err(error("Incomplete Wake-on-LAN packet send"));
    }
    Ok(())
}

#[derive(Clone, Copy)]
pub struct Target<'a> {
    pub device: Device,
    pub name: &'a str,
    pub(crate) mac: [u8; 6],
    pub verified: bool,
}
impl Target<'_> {
    fn id(self) -> String {
        format!("{}.{}", self.device.family.as_str(), self.name)
    }
    fn matches(self, id: &str) -> bool {
        id.strip_prefix(self.device.family.as_str())
            .and_then(|s| s.strip_prefix('.'))
            == Some(self.name)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    devices: Vec<Endpoint>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Endpoint {
    name: String,
    mac: String,
    wake_address: SocketAddr,
}
struct EndpointConfig {
    name: String,
    mac: [u8; 6],
    wake_address: SocketAddr,
}
pub struct Manager {
    devices: Vec<EndpointConfig>,
    _lock: File,
}
impl Adapter for Manager {
    const TOOLS: &'static [Tool] = &[];
    const SUPPORT: Support = Support::WAKE;
    fn from_source(source: &Source<'_>) -> Result<Self> {
        let c: Config = source.decode()?;
        if c.devices.is_empty()
            || !valid_unique_labels(c.devices.iter().map(|d| d.name.as_str()), 24, b"_-")
        {
            return Err(error("Expected 1–8 uniquely named Wake-on-LAN devices"));
        }
        let mut devices = Vec::with_capacity(c.devices.len());
        for d in c.devices {
            let mac = mac_bytes(&d.mac)?;
            if mac == [0; 6]
                || mac[0] & 1 != 0
                || d.wake_address.port() == 0
                || d.wake_address.ip().is_unspecified()
                || d.wake_address.ip().is_multicast()
                || devices
                    .iter()
                    .any(|other: &EndpointConfig| other.mac == mac)
            {
                return Err(error("Invalid or duplicate Wake-on-LAN endpoint"));
            }
            devices.push(EndpointConfig {
                name: d.name,
                mac,
                wake_address: d.wake_address,
            });
        }
        Ok(Self {
            devices,
            _lock: lock_configuration(source.path())?,
        })
    }
    fn entities<'a>(&'a self, _: &mut impl FnMut(Entity<'a>)) {}
    fn wake_targets<'a>(&'a self, visit: &mut impl FnMut(Target<'a>)) {
        for (index, d) in self.devices.iter().enumerate() {
            visit(Target {
                device: Device {
                    family: Family::Wol,
                    index,
                },
                name: &d.name,
                mac: d.mac,
                verified: false,
            });
        }
    }
    fn wake_device(&mut self, index: usize) -> Result<Value> {
        let d = self
            .devices
            .get(index)
            .ok_or_else(|| error("Unknown Wake-on-LAN device"))?;
        send(d.mac, d.wake_address)?;
        Ok(json!({"sent":true,"verified":false,"evidence":"magic_packet_sent_once"}))
    }
    fn execute(&mut self, _: &str, _: &Value) -> Result<Value> {
        Err(error("Unknown Wake-on-LAN operation"))
    }
}

const LIST: Tool = Tool {
    name: "wake_list",
    description: "List configured Wake-on-LAN targets and their verification/ownership policy. No network I/O or private addresses.",
    input: Input::Empty,
    hints: Hints::LIST,
};
const WAKE: Tool = Tool {
    name: "wake_on_lan",
    description: "Wake one device returned by wake_list. Generic targets send one magic packet and report verified:false; adapter targets retain identity checks and verification. TV-owned power requires maintenance. Never automatically replay an uncertain wake.",
    input: Input::Named {
        key: "device",
        extra: None,
        error: "Expected only a configured wake device ID",
    },
    hints: Hints {
        idempotent: false,
        ..Hints::CONTROL
    },
};
pub(crate) fn tools(selection: Selection) -> impl Iterator<Item = &'static Tool> + Clone {
    [
        selection.support().contains(Support::WAKE).then_some(&LIST),
        selection.support().contains(Support::WAKE).then_some(&WAKE),
    ]
    .into_iter()
    .flatten()
}
pub(crate) fn validate_targets(devices: &Devices) -> Result<()> {
    let mut duplicate = false;
    devices.wake_targets(&mut |target| {
        let mut matches = 0;
        devices.wake_targets(&mut |other| {
            if target.mac == other.mac
                || target.device.family == other.device.family && target.name == other.name
            {
                matches += 1;
            }
        });
        duplicate |= matches != 1;
    });
    if duplicate {
        return Err(error(
            "Duplicate wake identity; use the existing adapter target instead",
        ));
    }
    Ok(())
}
pub(crate) fn call(devices: &mut Devices, name: &str, args: &Value) -> Result<Value> {
    validate_targets(devices)?;
    if name == "wake_list" {
        let mut targets = Vec::new();
        devices.wake_targets(&mut |t| {
            let owner = if devices.computer_owns_power(t.device) {Some("computer_on/computer_off")} else if devices.tv.as_ref().is_some_and(|tv|tv.owns_power(t.device)) {Some("tv_on/tv_off")} else {None};
            targets.push(json!({"device":t.id(),"wake_verifiable":t.verified,"power_owner":owner,"maintenance_required":owner.is_some() && !devices.maintenance}));
        });
        return Ok(json!({"devices":targets}));
    }
    if name != "wake_on_lan" {
        return Err(error("Unknown wake operation"));
    }
    let id = args["device"]
        .as_str()
        .ok_or_else(|| error("Expected wake device ID"))?;
    let mut target = None;
    devices.wake_targets(&mut |t| {
        if t.matches(id) {
            target = Some(t.device);
        }
    });
    let target = target.ok_or_else(|| error("Unknown configured Wake-on-LAN device"))?;
    Ok(json!({"device":id,"result":devices.wake_device(target)?}))
}
