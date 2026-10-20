//! One bounded TV power operation: optional network wake, pinned state, one key.
use super::{RemoteManager, Status};
use crate::{Result, error};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    net::SocketAddr,
    time::{Duration, Instant},
};

#[derive(Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PowerOnKey {
    #[default]
    Wakeup,
    Power,
}
fn ready_seconds() -> u64 {
    45
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Wake {
    pub(super) mac: String,
    pub(super) wake_address: SocketAddr,
    #[serde(default = "ready_seconds")]
    pub(super) ready_seconds: u64,
}
impl Wake {
    pub(super) fn validate(&self) -> Result<()> {
        let mac = crate::wol::mac_bytes(&self.mac)?;
        if mac == [0; 6]
            || mac[0] & 1 != 0
            || self.wake_address.port() == 0
            || self.wake_address.ip().is_unspecified()
            || self.wake_address.ip().is_multicast()
            || !(1..=120).contains(&self.ready_seconds)
        {
            return Err(error(
                "Invalid TCL network wake endpoint or readiness interval",
            ));
        }
        Ok(())
    }
}

#[derive(Serialize)]
pub(super) struct Outcome {
    requested_power: bool,
    applied: bool,
    verified: bool,
    outcome: &'static str,
    phase: &'static str,
    /// None means a write was attempted but its completion is uncertain.
    command_sent: Option<bool>,
    wake_packet_sent: bool,
    cast_command_sent: Option<bool>,
    status: Option<Status>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}
impl Outcome {
    pub(super) fn new(power: bool) -> Self {
        Self {
            requested_power: power,
            applied: false,
            verified: false,
            outcome: "not_sent",
            phase: "connect",
            command_sent: Some(false),
            wake_packet_sent: false,
            cast_command_sent: Some(false),
            status: None,
            error: None,
        }
    }
    fn failure(mut self, failure: &dyn std::fmt::Display) -> Result<Value> {
        self.error = Some(failure.to_string().chars().take(512).collect());
        Ok(serde_json::to_value(self)?)
    }
    fn confirmed(mut self, status: Option<Status>) -> Result<Value> {
        self.verified = true;
        self.outcome = "confirmed";
        self.phase = "complete";
        self.status = status;
        Ok(serde_json::to_value(self)?)
    }
}
fn unavailable(failure: &(dyn std::error::Error + Send + Sync + 'static)) -> bool {
    failure.downcast_ref::<std::io::Error>().is_some_and(|e| {
        matches!(
            e.kind(),
            std::io::ErrorKind::TimedOut
                | std::io::ErrorKind::WouldBlock
                | std::io::ErrorKind::ConnectionRefused
                | std::io::ErrorKind::ConnectionReset
                | std::io::ErrorKind::ConnectionAborted
                | std::io::ErrorKind::NotConnected
                | std::io::ErrorKind::HostUnreachable
                | std::io::ErrorKind::NetworkUnreachable
                | std::io::ErrorKind::UnexpectedEof
        )
    })
}

pub(super) fn apply<const TCL: bool>(
    manager: &mut RemoteManager<TCL>,
    power: bool,
) -> Result<Value> {
    let mut result = Outcome::new(power);
    let timeout = Duration::from_secs(manager.config.timeout_seconds);
    let mut recorded = false;
    let first = manager.config.remote::<TCL>(Instant::now() + timeout);
    if power && manager.config.cast_wake.is_some() {
        match first {
            Ok(mut remote) if remote.status.as_ref().is_some_and(|s| s.power) => {
                manager
                    .shutdown_guard
                    .as_mut()
                    .expect("TCL power guard")
                    .confirmed(true)?;
                return result.confirmed(remote.status.take());
            }
            Ok(remote) => {
                manager
                    .shutdown_guard
                    .as_mut()
                    .expect("TCL power guard")
                    .confirmed(false)?;
                drop(remote);
            }
            Err(failure) if unavailable(failure.as_ref()) => {}
            Err(failure) => return Err(failure),
        }
        return cast_wake(manager, result);
    }
    let mut remote = match first {
        Ok(remote) => remote,
        Err(failure) if unavailable(failure.as_ref()) => {
            let Some(wake) = manager.config.wake.as_ref().filter(|_| power) else {
                return result.failure(failure.as_ref());
            };
            // No power key has been attempted. Persist the logical on operation
            // before its one configured magic packet, including across restart.
            manager
                .shutdown_guard
                .as_mut()
                .expect("TCL power guard")
                .record_attempt(true)?;
            recorded = true;
            result.phase = "network_wake";
            result.outcome = "unknown";
            result.applied = true;
            if let Err(failure) =
                crate::wol::send(crate::wol::mac_bytes(&wake.mac)?, wake.wake_address)
            {
                return result.failure(failure.as_ref());
            }
            result.wake_packet_sent = true;
            result.phase = "network_recovery";
            let deadline = Instant::now() + Duration::from_secs(wake.ready_seconds);
            loop {
                if Instant::now() >= deadline {
                    return result
                        .failure(&"TCL network recovery was not confirmed before its deadline");
                }
                match manager
                    .config
                    .remote::<TCL>(deadline.min(Instant::now() + timeout))
                {
                    Ok(remote) => break remote,
                    Err(failure) if unavailable(failure.as_ref()) => {
                        if Instant::now() >= deadline {
                            return result.failure(failure.as_ref());
                        }
                        std::thread::sleep(
                            Duration::from_secs(2)
                                .min(deadline.saturating_duration_since(Instant::now())),
                        );
                    }
                    // Identity, credential and protocol failures never trigger
                    // a replacement pin, another packet, or a power command.
                    Err(failure) => return Err(failure),
                }
            }
        }
        Err(failure) => return Err(failure),
    };
    let guard = manager.shutdown_guard.as_mut().expect("TCL power guard");
    let current = remote.status.as_ref().expect("initial pinned power").power;
    guard.confirmed(current)?;
    if current == power {
        return result.confirmed(remote.status.take());
    }
    result.phase = "prepare";
    if let Err(failure) = remote.prepare_control::<TCL>(Instant::now() + timeout) {
        return result.failure(failure.as_ref());
    }
    if !recorded {
        guard.record_attempt(power)?;
    }
    result.phase = "send";
    result.applied = true;
    result.outcome = "unknown";
    result.command_sent = None;
    let code = if power && manager.config.power_on_key == PowerOnKey::Wakeup {
        224
    } else {
        26
    };
    if let Err(failure) = remote.key(code) {
        return result.failure(failure.as_ref());
    }
    result.command_sent = Some(true);
    result.phase = "verify";
    remote
        .stream
        .sock
        .set_deadline(Instant::now() + Duration::from_secs(manager.config.verify_seconds));
    // Verification stays on this session. Never reconnect or replay a key.
    if let Err(failure) = remote.until(|r| r.status.as_ref().is_some_and(|s| s.power == power)) {
        return result.failure(failure.as_ref());
    }
    guard.confirmed(power)?;
    result.confirmed(remote.status.take())
}

pub(super) fn cast_wake<const TCL: bool>(
    manager: &mut RemoteManager<TCL>,
    mut result: Outcome,
) -> Result<Value> {
    let guard = manager.shutdown_guard.as_mut().expect("TCL power guard");
    guard.record_attempt(true)?;
    result.phase = "cast_activation";
    let wake = manager
        .config
        .cast_wake
        .as_ref()
        .expect("configured Cast wake");
    // The TV may temporarily drop both TCP services while entering standby.
    // Share one total recovery/activation/verification deadline. Only failures
    // known to precede any Cast STOP/LAUNCH may reconnect or send network wake.
    let ready = manager.config.wake.as_ref().map_or(45, |w| w.ready_seconds);
    let deadline = Instant::now() + Duration::from_secs(ready);
    let timeout = Duration::from_secs(manager.config.timeout_seconds);
    loop {
        result.phase = "cast_activation";
        match wake.activate(manager.config.host, deadline) {
            Ok(sent) => {
                result.cast_command_sent = Some(sent);
                result.applied |= sent;
                break;
            }
            Err(failure) => {
                result.cast_command_sent = failure.command_sent;
                result.applied |= failure.command_sent != Some(false);
                let recoverable =
                    failure.command_sent == Some(false) && unavailable(failure.cause.as_ref());
                if failure.command_sent != Some(false) && unavailable(failure.cause.as_ref()) {
                    // Standby exit may tear down Cast before LAUNCH replies.
                    // Verify the pinned power on read-only connections; never
                    // resend activation or substitute a network wake here.
                    break;
                }
                if !recoverable || Instant::now() >= deadline {
                    result.outcome = if result.applied {
                        "unknown"
                    } else {
                        "not_sent"
                    };
                    if !result.applied {
                        guard.cancel_unsent()?;
                    }
                    return result.failure(failure.cause.as_ref());
                }
                if let Some(network) = manager.config.wake.as_ref()
                    && !result.wake_packet_sent
                {
                    result.phase = "network_wake";
                    result.applied = true;
                    result.outcome = "unknown";
                    if let Err(failure) =
                        crate::wol::send(crate::wol::mac_bytes(&network.mac)?, network.wake_address)
                    {
                        return result.failure(failure.as_ref());
                    }
                    result.wake_packet_sent = true;
                }
                result.phase = "cast_recovery";
                // A network wake may already have powered the TV on. Check its
                // pinned identity/state before considering a Cast launch.
                if result.wake_packet_sent {
                    match manager
                        .config
                        .remote::<TCL>(deadline.min(Instant::now() + timeout))
                    {
                        Ok(mut remote) if remote.status.as_ref().is_some_and(|s| s.power) => {
                            guard.confirmed(true)?;
                            return result.confirmed(remote.status.take());
                        }
                        Ok(_) => {}
                        Err(failure) if unavailable(failure.as_ref()) => {}
                        Err(failure) => return result.failure(failure.as_ref()),
                    }
                }
                if Instant::now() >= deadline {
                    result.outcome = if result.applied {
                        "unknown"
                    } else {
                        "not_sent"
                    };
                    if !result.applied {
                        guard.cancel_unsent()?;
                    }
                    return result
                        .failure(&"TCL Cast recovery was not confirmed before its deadline");
                }
                std::thread::sleep(
                    Duration::from_secs(2).min(deadline.saturating_duration_since(Instant::now())),
                );
            }
        }
    }
    if !result.applied {
        guard.cancel_unsent()?;
    }
    result.phase = "verify_cast_wake";
    result.outcome = if result.applied {
        "unknown"
    } else {
        "not_sent"
    };
    // Only read-side connections are retried. No LAUNCH, magic packet or key
    // is repeated, and Cast's own flags never substitute for the pinned power.
    loop {
        if Instant::now() >= deadline {
            return result.failure(&"TCL power-on was not confirmed before its deadline");
        }
        match manager
            .config
            .remote::<TCL>(deadline.min(Instant::now() + timeout))
        {
            Ok(mut remote) => {
                if remote.status.as_ref().is_some_and(|s| s.power) {
                    guard.confirmed(true)?;
                    return result.confirmed(remote.status.take());
                }
                remote
                    .stream
                    .sock
                    .set_deadline(deadline.min(Instant::now() + Duration::from_secs(2)));
                match remote.until(|r| r.status.as_ref().is_some_and(|s| s.power)) {
                    Ok(()) => {
                        guard.confirmed(true)?;
                        return result.confirmed(remote.status.take());
                    }
                    Err(failure) if unavailable(failure.as_ref()) => {}
                    Err(failure) => return result.failure(failure.as_ref()),
                }
            }
            Err(failure) if unavailable(failure.as_ref()) => {}
            Err(failure) => return result.failure(failure.as_ref()),
        }
        std::thread::sleep(
            Duration::from_secs(2).min(deadline.saturating_duration_since(Instant::now())),
        );
    }
}
