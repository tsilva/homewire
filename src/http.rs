//! Small shared HTTP transport: bounded headers/bodies and one total deadline.
use crate::{Result, error};
use std::{
    io::{BufRead, BufReader, Read, Write},
    net::SocketAddr,
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
            max_response_bytes: MAX_BODY,
        },
    )?
    .body)
}

pub(crate) struct Request<'a> {
    pub path: &'a str,
    pub content_type: Option<&'a str>,
    pub body: Option<&'a [u8]>,
    pub cookie: Option<&'a str>,
    pub max_response_bytes: usize,
}
pub(crate) struct Response {
    pub body: Vec<u8>,
    pub session_cookie: Option<String>,
    pub cookies: String,
}
pub(crate) fn send(
    address: SocketAddr,
    authority: &str,
    deadline: Instant,
    request: Request<'_>,
) -> Result<Response> {
    let mut stream = crate::transport::Timed::connect(address, deadline)?;
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
        header.push_str("Cookie: TP_SESSIONID=");
        header.push_str(cookie);
        header.push_str("\r\n");
    }
    header.push_str("\r\n");
    stream.write_all(header.as_bytes())?;
    if let Some(bytes) = request.body {
        stream.write_all(bytes)?;
    }
    // Headers are small; large bodies still read directly into their own storage.
    read_response(
        &mut BufReader::with_capacity(1024, stream),
        request.content_type,
        request.max_response_bytes,
    )
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
    Ok(read_response(reader, Some(expected_type), MAX_BODY)?.body)
}
pub(crate) fn read_response(
    reader: &mut impl BufRead,
    expected_type: Option<&str>,
    limit: usize,
) -> Result<Response> {
    let limit = limit.min(MAX_BODY);
    let mut session_cookie = None;
    let mut cookies = String::new();
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
        cookies.clear();
        loop {
            let header = line(reader, &mut budget)?;
            if header.is_empty() {
                break;
            }
            let (name, value) = header
                .split_once(':')
                .ok_or_else(|| error("Invalid device HTTP header"))?;
            let value = value.trim();
            if name.eq_ignore_ascii_case("set-cookie") {
                let pair = value.split(';').next().unwrap_or_default();
                if let Some((key, val)) = pair.split_once('=') {
                    if !key
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
                        || val.bytes().any(|b| b < 33 || b == 127)
                        || cookies.len() + pair.len() > 2048
                    {
                        return Err(error("Invalid HTTP cookie"));
                    }
                    if !cookies.is_empty() {
                        cookies.push_str("; ");
                    }
                    cookies.push_str(pair);
                }
            }
            if name.eq_ignore_ascii_case("content-length") {
                if length.is_some() {
                    return Err(error("Duplicate device HTTP content length"));
                }
                let n = value.parse::<usize>()?;
                // Error bodies are never read. Preserve the typed HTTP rejection
                // even when its declared body exceeds a handshake's small limit.
                if (200..300).contains(&code) && n > limit {
                    return Err(error("Device response exceeds its body limit"));
                }
                length = Some(n);
            } else if name.eq_ignore_ascii_case("transfer-encoding") {
                if chunked || !value.eq_ignore_ascii_case("chunked") {
                    return Err(error("Unsupported device HTTP transfer encoding"));
                }
                chunked = true;
            } else if name.eq_ignore_ascii_case("content-type") {
                content_type = expected_type.map(|expected| {
                    value
                        .split(';')
                        .next()
                        .unwrap_or_default()
                        .trim()
                        .eq_ignore_ascii_case(expected)
                });
            } else if name.eq_ignore_ascii_case("set-cookie")
                && let Some(cookie) = value
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
        if code == 100 {
            continue;
        }
        if !(200..300).contains(&code) {
            return Err(Box::new(StatusError(code)));
        }
        final_response = true;
        break;
    }
    if !final_response {
        return Err(error("No final device HTTP response"));
    }
    // Some devices (including HP DeskJet 3630) omit Content-Type.
    if content_type == Some(false) {
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
                    cookies,
                });
            }
            if size > limit - body.len() {
                return Err(error("Device response exceeds its body limit"));
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
        reader.take((limit + 1) as u64).read_to_end(&mut body)?;
        if body.len() > limit {
            return Err(error("Device response exceeds its body limit"));
        }
    }
    Ok(Response {
        body,
        session_cookie,
        cookies,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn security_rejections_keep_their_status_with_a_zero_body_budget() {
        let response = "HTTP/1.1 403 Forbidden\r\nContent-Length: 128\r\n\r\n";
        let err = read_response(&mut Cursor::new(response), None, 0)
            .err()
            .expect("rejection");
        assert_eq!(
            err.downcast_ref::<StatusError>().expect("HTTP status").0,
            403
        );
    }

    #[test]
    fn protocol_limits_reject_declared_bodies_before_reading_them() {
        // No body is provided: the error must be the size guard, not EOF.
        for response in [
            "HTTP/1.1 200 OK\r\nContent-Length: 17\r\n\r\n",
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n11\r\n",
        ] {
            let err = read_response(&mut Cursor::new(response), None, 16)
                .err()
                .expect("oversized body");
            assert!(err.to_string().contains("body limit"));
        }
    }

    #[test]
    fn protocol_limits_cover_fixed_chunked_and_eof_bodies() -> Result<()> {
        for headers in [
            "Content-Length: 4\r\n",
            "Transfer-Encoding: chunked\r\n",
            "",
        ] {
            let payload = if headers.starts_with("Transfer") {
                "4\r\ndata\r\n0\r\n\r\n"
            } else {
                "data"
            };
            let response = format!("HTTP/1.1 200 OK\r\n{headers}\r\n{payload}");
            assert_eq!(
                read_response(&mut Cursor::new(&response), None, 4)?.body,
                b"data"
            );
            assert!(read_response(&mut Cursor::new(&response), None, 3).is_err());
        }
        Ok(())
    }
}
