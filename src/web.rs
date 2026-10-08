//! Bounded HTTP(S) requests shared by integrations. No redirects or retries.
use crate::{Result, error, http, transport::Timed};
use std::{
    io::{BufReader, Read, Write},
    net::ToSocketAddrs,
    sync::Arc,
    time::{Duration, Instant},
};

pub(crate) struct Url<'a> {
    pub secure: bool,
    pub host: &'a str,
    pub port: u16,
    pub path: &'a str,
}
impl<'a> Url<'a> {
    pub fn parse(url: &'a str) -> Result<Self> {
        if url.len() > 2048
            || !url
                .bytes()
                .all(|b| b.is_ascii_graphic() && !matches!(b, b'\\' | b'#'))
        {
            return Err(error("Invalid endpoint URL"));
        }
        let (secure, rest) = if let Some(s) = url.strip_prefix("https://") {
            (true, s)
        } else if let Some(s) = url.strip_prefix("http://") {
            (false, s)
        } else {
            return Err(error("Only HTTP(S) endpoints are supported"));
        };
        let (authority, path) = rest
            .split_once('/')
            .map_or((rest, "/"), |(a, _)| (a, &rest[a.len()..]));
        if authority.is_empty() || authority.contains('@') || authority.contains('?') {
            return Err(error("Invalid endpoint authority"));
        }
        let (host, port) = if authority.starts_with('[') {
            return Err(error("Use a DNS name or IPv4 endpoint"));
        } else if let Some((h, p)) = authority.rsplit_once(':') {
            (h, p.parse()?)
        } else {
            (authority, if secure { 443 } else { 80 })
        };
        if host.is_empty()
            || !host
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-'))
            || port == 0
        {
            return Err(error("Invalid endpoint host"));
        }
        Ok(Self {
            secure,
            host,
            port,
            path,
        })
    }
}
pub(crate) fn request(
    url: &str,
    method: &str,
    headers: &[(&str, &str)],
    body: &[u8],
    timeout: Duration,
) -> Result<http::Response> {
    let endpoint = Url::parse(url)?;
    if !matches!(method, "GET" | "POST" | "PUT" | "DELETE") || body.len() > http::MAX_BODY {
        return Err(error("Invalid bounded HTTP request"));
    }
    for (name, value) in headers {
        if name.is_empty()
            || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
            || value.len() > 2048
            || value.bytes().any(|b| b < 32 || b == 127)
        {
            return Err(error("Invalid HTTP header"));
        }
    }
    let deadline = Instant::now() + timeout;
    let addresses: Vec<_> = (endpoint.host, endpoint.port)
        .to_socket_addrs()?
        .take(8)
        .collect();
    let socket = addresses
        .iter()
        .find_map(|a| Timed::connect(*a, deadline).ok())
        .ok_or_else(|| error("Endpoint connection failed"))?;
    if endpoint.secure {
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let roots =
            rustls::RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        let mut config = rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()?
            .with_root_certificates(roots)
            .with_no_client_auth();
        config.resumption = rustls::client::Resumption::disabled();
        config.max_fragment_size = Some(1024);
        let mut conn = rustls::ClientConnection::new(
            Arc::new(config),
            rustls::pki_types::ServerName::try_from(endpoint.host.to_owned())?,
        )?;
        conn.set_buffer_limit(Some(8192));
        exchange(
            &mut rustls::StreamOwned::new(conn, socket),
            &endpoint,
            method,
            headers,
            body,
        )
    } else {
        exchange(&mut { socket }, &endpoint, method, headers, body)
    }
}
fn exchange(
    stream: &mut (impl Read + Write),
    endpoint: &Url<'_>,
    method: &str,
    headers: &[(&str, &str)],
    body: &[u8],
) -> Result<http::Response> {
    write!(
        stream,
        "{method} {} HTTP/1.1\r\nHost: {}:{}\r\nUser-Agent: Homewire/{} (+https://github.com/tsilva/homewire)\r\nContent-Length: {}\r\nConnection: close\r\n",
        endpoint.path,
        endpoint.host,
        endpoint.port,
        env!("CARGO_PKG_VERSION"),
        body.len()
    )?;
    for (name, value) in headers {
        write!(stream, "{name}: {value}\r\n")?;
    }
    stream.write_all(b"\r\n")?;
    stream.write_all(body)?;
    http::read_response(
        &mut BufReader::with_capacity(1024, stream),
        None,
        http::MAX_BODY,
    )
}
pub(crate) fn encode(value: &str) -> String {
    use std::fmt::Write;
    let mut result = String::new();
    for b in value.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            result.push(b as char);
        } else {
            let _ = write!(result, "%{b:02X}");
        }
    }
    result
}
