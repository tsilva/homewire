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
