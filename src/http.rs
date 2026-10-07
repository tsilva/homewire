//! Small shared HTTP transport: bounded headers/bodies and one total deadline.
use crate::{Result, error};
use std::{
    io::{BufRead, BufReader, Read, Write},
    net::{SocketAddr, TcpStream},
    time::{Duration, Instant},
};

pub const MAX_BODY: usize = 64 * 1024;
const MAX_HEADERS: usize = 8192;
#[derive(Debug)]
pub(crate) struct StatusError(pub u16);
impl std::fmt::Display for StatusError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Device returned HTTP {}", self.0)
    }
}
impl std::error::Error for StatusError {}

pub(crate) fn exchange(
    address: SocketAddr,
    authority: &str,
    path: &str,
    timeout: Duration,
    content_type: &str,
    body: Option<&[u8]>,
) -> Result<Vec<u8>> {
    Ok(send(
        address,
        authority,
        Instant::now() + timeout,
        Request {
            path,
            content_type: Some(content_type),
            body,
            cookie: None,
        },
    )?
    .body)
}

pub(crate) struct Request<'a> {
    pub path: &'a str,
    pub content_type: Option<&'a str>,
    pub body: Option<&'a [u8]>,
    pub cookie: Option<&'a str>,
}
pub(crate) struct Response {
    pub body: Vec<u8>,
    pub session_cookie: Option<String>,
}
pub(crate) fn send(
    address: SocketAddr,
    authority: &str,
    deadline: Instant,
    request: Request<'_>,
) -> Result<Response> {
    let remaining = deadline
        .checked_duration_since(Instant::now())
        .filter(|d| !d.is_zero())
        .ok_or_else(|| error("Device request timed out"))?;
    let socket = TcpStream::connect_timeout(&address, remaining)?;
    let mut stream = Timed { socket, deadline };
    let path = request.path;
    let content_type = request.content_type.unwrap_or("application/octet-stream");
    let mut header = match request.body {
        Some(bytes) => format!(
            "POST {path} HTTP/1.1\r\nHost: {authority}\r\nContent-Type: {content_type}\r\nAccept: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n",
            bytes.len()
        ),
        None => format!(
            "GET {path} HTTP/1.1\r\nHost: {authority}\r\nAccept: {content_type}\r\nConnection: close\r\n"
        ),
    };
    if let Some(cookie) = request.cookie {
        validate_cookie(cookie)?;
        header.push_str(&format!("Cookie: TP_SESSIONID={cookie}\r\n"));
    }
    header.push_str("\r\n");
    stream.write_all(header.as_bytes())?;
    if let Some(bytes) = request.body {
        stream.write_all(bytes)?;
    }
    read_response(&mut BufReader::new(stream), request.content_type)
}
fn validate_cookie(value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
    {
        return Err(error("Invalid device session cookie"));
    }
    Ok(())
}

struct Timed {
    socket: TcpStream,
    deadline: Instant,
}
impl Timed {
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
fn line(reader: &mut impl BufRead, budget: &mut usize) -> Result<String> {
    let mut bytes = Vec::new();
    (&mut *reader)
        .take((*budget + 1) as u64)
        .read_until(b'\n', &mut bytes)?;
    if bytes.len() > *budget || !bytes.ends_with(b"\r\n") {
        return Err(error("Invalid or oversized device HTTP headers"));
    }
    *budget -= bytes.len();
    bytes.truncate(bytes.len() - 2);
    String::from_utf8(bytes).map_err(|_| error("Invalid device HTTP header encoding"))
}
pub(crate) fn read_body(reader: &mut impl BufRead, expected_type: &str) -> Result<Vec<u8>> {
    Ok(read_response(reader, Some(expected_type))?.body)
}
fn read_response(reader: &mut impl BufRead, expected_type: Option<&str>) -> Result<Response> {
    let mut session_cookie = None;
    let mut budget = MAX_HEADERS;
    let mut length = None;
    let mut chunked = false;
    let mut content_type = None;
    let mut final_response = false;
    for _ in 0..4 {
        let status = line(reader, &mut budget)?;
        let mut parts = status.split_whitespace();
        if !matches!(parts.next(), Some("HTTP/1.0" | "HTTP/1.1")) {
            return Err(error("Invalid device HTTP response"));
        }
        let code = parts
            .next()
            .and_then(|s| s.parse::<u16>().ok())
            .ok_or_else(|| error("Invalid device HTTP status"))?;
        length = None;
        chunked = false;
        content_type = None;
        session_cookie = None;
        loop {
            let header = line(reader, &mut budget)?;
            if header.is_empty() {
                break;
            }
            let (name, value) = header
                .split_once(':')
                .ok_or_else(|| error("Invalid device HTTP header"))?;
            let value = value.trim();
            match name.to_ascii_lowercase().as_str() {
                "content-length" => {
                    if length.is_some() {
                        return Err(error("Duplicate device HTTP content length"));
                    }
                    let n = value.parse::<usize>()?;
                    if n > MAX_BODY {
                        return Err(error("Device response exceeds 64 KiB"));
                    }
                    length = Some(n);
                }
                "transfer-encoding" => {
                    if chunked || !value.eq_ignore_ascii_case("chunked") {
                        return Err(error("Unsupported device HTTP transfer encoding"));
                    }
                    chunked = true;
                }
                "content-type" => content_type = Some(value.to_ascii_lowercase()),
                "set-cookie" => {
                    if let Some(cookie) = value
                        .split(';')
                        .next()
                        .and_then(|v| v.strip_prefix("TP_SESSIONID="))
                    {
                        validate_cookie(cookie)?;
                        if session_cookie.is_some() {
                            return Err(error("Duplicate device session cookie"));
                        }
                        session_cookie = Some(cookie.to_owned());
                    }
                }
                _ => {}
            }
        }
        if code == 100 {
            continue;
        }
        if code != 200 {
            return Err(Box::new(StatusError(code)));
        }
        final_response = true;
        break;
    }
    if !final_response {
        return Err(error("No final device HTTP response"));
    }
    // Some devices (including HP DeskJet 3630) omit Content-Type.
    if expected_type.is_some_and(|expected| {
        content_type
            .as_deref()
            .is_some_and(|s| s.split(';').next().map(str::trim) != Some(expected))
    }) {
        return Err(error("Unexpected device HTTP content type"));
    }
    if chunked && length.is_some() {
        return Err(error("Ambiguous device HTTP framing"));
    }
    let mut body = Vec::new();
    if chunked {
        let mut chunk_budget = MAX_HEADERS;
        for _ in 0..1024 {
            let header = line(reader, &mut chunk_budget)?;
            let size =
                usize::from_str_radix(header.split(';').next().unwrap_or_default().trim(), 16)?;
            if size == 0 {
                while !line(reader, &mut chunk_budget)?.is_empty() {}
                return Ok(Response {
                    body,
                    session_cookie,
                });
            }
            if size > MAX_BODY - body.len() {
                return Err(error("Device response exceeds 64 KiB"));
            }
            let start = body.len();
            body.resize(start + size, 0);
            reader.read_exact(&mut body[start..])?;
            let mut ending = [0; 2];
            reader.read_exact(&mut ending)?;
            if ending != *b"\r\n" {
                return Err(error("Invalid device HTTP chunk"));
            }
        }
        return Err(error("Too many device HTTP chunks"));
    } else if let Some(length) = length {
        body.resize(length, 0);
        reader.read_exact(&mut body)?;
    } else {
        reader.take((MAX_BODY + 1) as u64).read_to_end(&mut body)?;
        if body.len() > MAX_BODY {
            return Err(error("Device response exceeds 64 KiB"));
        }
    }
    Ok(Response {
        body,
        session_cookie,
    })
}
