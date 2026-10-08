//! Minimal synchronous WebSocket for Samsung's bounded text messages.
use crate::{Result, error, transport::Timed};
use base64::{Engine, engine::general_purpose::STANDARD};
use sha1::{Digest, Sha1};
use std::{
    io::{Read, Write},
    net::SocketAddr,
    time::Instant,
};
const MAX: usize = 8192;
pub(crate) struct WebSocket(Timed);
impl WebSocket {
    pub(crate) fn open(
        address: SocketAddr,
        authority: &str,
        path: &str,
        deadline: Instant,
    ) -> Result<Self> {
        if !path.starts_with('/') || path.bytes().any(|b| b <= 32 || b >= 127) {
            return Err(error("Invalid WebSocket path"));
        }
        let mut random = [0; 16];
        getrandom::getrandom(&mut random).map_err(|_| error("WebSocket entropy unavailable"))?;
        let key = STANDARD.encode(random);
        let accept = STANDARD.encode(Sha1::digest(format!(
            "{key}258EAFA5-E914-47DA-95CA-C5AB0DC85B11"
        )));
        let mut stream = Timed::connect(address, deadline)?;
        write!(
            stream,
            "GET {path} HTTP/1.1\r\nHost: {authority}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\r\n"
        )?;
        let mut header = Vec::new();
        while !header.ends_with(b"\r\n\r\n") {
            if header.len() == MAX {
                return Err(error("Oversized WebSocket upgrade"));
            }
            let mut byte = [0];
            stream.read_exact(&mut byte)?;
            header.push(byte[0]);
        }
        let header =
            std::str::from_utf8(&header).map_err(|_| error("Invalid WebSocket upgrade"))?;
        let mut lines = header.split("\r\n");
        if lines.next().and_then(|l| l.split_whitespace().nth(1)) != Some("101") {
            return Err(error("WebSocket upgrade rejected"));
        }
        let (mut upgraded, mut connection, mut accepted) = (false, false, false);
        for line in lines {
            if let Some((name, value)) = line.split_once(':') {
                let value = value.trim();
                match name.to_ascii_lowercase().as_str() {
                    "upgrade" => upgraded = value.eq_ignore_ascii_case("websocket"),
                    "connection" => {
                        connection = value
                            .split(',')
                            .any(|v| v.trim().eq_ignore_ascii_case("upgrade"))
                    }
                    "sec-websocket-accept" => accepted = value == accept,
                    _ => {}
                }
            }
        }
        if !(upgraded && connection && accepted) {
            return Err(error("Invalid WebSocket upgrade proof"));
        }
        Ok(Self(stream))
    }
    fn send(&mut self, opcode: u8, body: &[u8]) -> Result<()> {
        if body.len() > MAX {
            return Err(error("Oversized WebSocket message"));
        }
        let mut mask = [0; 4];
        getrandom::getrandom(&mut mask).map_err(|_| error("WebSocket entropy unavailable"))?;
        let mut header = vec![0x80 | opcode];
        if body.len() < 126 {
            header.push(0x80 | body.len() as u8);
        } else {
            header.push(0x80 | 126);
            header.extend_from_slice(&(body.len() as u16).to_be_bytes());
        }
        header.extend_from_slice(&mask);
        self.0.write_all(&header)?;
        let mut masked = body.to_vec();
        for (i, b) in masked.iter_mut().enumerate() {
            *b ^= mask[i % 4];
        }
        self.0.write_all(&masked)?;
        Ok(())
    }
    pub(crate) fn text(&mut self, text: &str) -> Result<()> {
        self.send(1, text.as_bytes())
    }
    pub(crate) fn read(&mut self) -> Result<String> {
        // Deadline bounds time; count also bounds a peer's endless control frames.
        for _ in 0..64 {
            let mut header = [0; 2];
            self.0.read_exact(&mut header)?;
            if header[0] & 0x70 != 0 || header[1] & 0x80 != 0 {
                return Err(error("Unsupported WebSocket framing"));
            }
            let opcode = header[0] & 15;
            let fin = header[0] & 0x80 != 0;
            let length = match header[1] & 127 {
                126 => {
                    let mut b = [0; 2];
                    self.0.read_exact(&mut b)?;
                    u16::from_be_bytes(b) as usize
                }
                127 => return Err(error("Oversized WebSocket frame")),
                n => n as usize,
            };
            if length > MAX || !fin || (opcode >= 8 && length > 125) {
                return Err(error("Unsupported or oversized WebSocket frame"));
            }
            let mut body = vec![0; length];
            self.0.read_exact(&mut body)?;
            match opcode {
                1 => return String::from_utf8(body).map_err(|_| error("Invalid WebSocket text")),
                8 => return Err(error("Samsung remote connection closed")),
                9 => self.send(10, &body)?,
                10 => {}
                _ => return Err(error("Unsupported WebSocket opcode")),
            }
        }
        Err(error("Too many WebSocket control messages"))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::{net::TcpListener, thread, time::Duration};
    #[test]
    fn rejects_upgrade_without_accept_proof_and_oversized_frames() -> Result<()> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let addr = listener.local_addr()?;
        let worker = thread::spawn(move || -> Result<()> {
            let (mut socket, _) = listener.accept()?;
            socket.set_read_timeout(Some(Duration::from_secs(2)))?;
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") {
                let mut b = [0];
                socket.read_exact(&mut b)?;
                request.push(b[0]);
            }
            socket.write_all(b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: wrong\r\n\r\n")?;
            Ok(())
        });
        assert!(
            WebSocket::open(
                addr,
                "localhost",
                "/socket.io/1/websocket/test",
                Instant::now() + Duration::from_secs(2)
            )
            .is_err()
        );
        worker.join().expect("server")?;
        for header in [
            [0x81, 126, 0x20, 0x01],
            [0x09, 0, 0, 0],
            [0x81, 0x80, 0, 0],
            [0x81, 127, 0, 0],
        ] {
            let listener = TcpListener::bind("127.0.0.1:0")?;
            let addr = listener.local_addr()?;
            let worker = thread::spawn(move || -> Result<()> {
                let (mut socket, _) = listener.accept()?;
                socket.write_all(&header)?;
                Ok(())
            });
            let mut ws = WebSocket(Timed::connect(
                addr,
                Instant::now() + Duration::from_secs(2),
            )?);
            assert!(ws.read().is_err());
            worker.join().expect("server")?;
        }
        Ok(())
    }
}
