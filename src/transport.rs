//! Deadline-bound socket shared by HTTP, WebSocket and TLS.
use crate::{Result, error};
use std::{
    io::{Read, Write},
    net::{SocketAddr, TcpStream},
    time::{Duration, Instant},
};
pub(crate) struct Timed {
    socket: TcpStream,
    deadline: Instant,
}
impl Timed {
    pub(crate) fn accepted(socket: TcpStream, deadline: Instant) -> Self {
        Self { socket, deadline }
    }
    pub(crate) fn connect(address: SocketAddr, deadline: Instant) -> Result<Self> {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .filter(|d| !d.is_zero())
            .ok_or_else(|| error("Device request timed out"))?;
        Ok(Self {
            socket: TcpStream::connect_timeout(&address, remaining)?,
            deadline,
        })
    }

    pub(crate) fn set_deadline(&mut self, deadline: Instant) {
        self.deadline = deadline;
    }
    fn remaining(&self) -> std::io::Result<Duration> {
        self.deadline
            .checked_duration_since(Instant::now())
            .filter(|d| !d.is_zero())
            .ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::TimedOut, "Device request timed out")
            })
    }
}
impl Read for Timed {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        self.socket.set_read_timeout(Some(self.remaining()?))?;
        self.socket.read(bytes)
    }
}
impl Write for Timed {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.socket.set_write_timeout(Some(self.remaining()?))?;
        self.socket.write(bytes)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.socket.flush()
    }
}

/// Shared by an adapter and its I/O lane. Busy callers fail before network I/O.
#[derive(Clone, Default)]
pub(crate) struct Gate(std::sync::Arc<std::sync::atomic::AtomicBool>);
pub(crate) struct Lease(std::sync::Arc<std::sync::atomic::AtomicBool>);
impl Gate {
    pub(crate) fn enter(&self) -> Result<Lease> {
        self.0
            .compare_exchange(
                false,
                true,
                std::sync::atomic::Ordering::Acquire,
                std::sync::atomic::Ordering::Relaxed,
            )
            .map_err(|_| error("Device is busy; no command was sent"))?;
        Ok(Lease(self.0.clone()))
    }
}
impl Drop for Lease {
    fn drop(&mut self) {
        self.0.store(false, std::sync::atomic::Ordering::Release);
    }
}

#[cfg(test)]
mod gate_tests {
    use super::*;
    #[test]
    fn cloned_device_gate_fails_fast_and_releases_on_drop() -> Result<()> {
        let owner = Gate::default();
        let worker = owner.clone();
        let lease = owner.enter()?;
        assert!(worker.enter().is_err());
        drop(lease);
        assert!(worker.enter().is_ok());
        Ok(())
    }
}
