use homewire::{Result, api, cast, device::Adapter, dlna, entity::Command};
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream},
    sync::Arc,
    thread,
    time::Duration,
};
mod support {
    include!("support/media.rs");
}
fn request(stream: &mut impl Read) -> Result<(String, Vec<u8>)> {
    let mut r = BufReader::new(stream);
    let mut line = String::new();
    r.read_line(&mut line)?;
    let first = line.clone();
    let mut n = 0;
    loop {
        line.clear();
        r.read_line(&mut line)?;
        if line == "\r\n" {
            break;
        }
        if let Some((key, v)) = line.split_once(':')
            && key.eq_ignore_ascii_case("content-length")
        {
            n = v.trim().parse()?;
        }
    }
    let mut b = vec![0; n];
    r.read_exact(&mut b)?;
    Ok((first, b))
}
fn response(stream: &mut impl Write, body: &[u8]) -> Result<()> {
    write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )?;
    stream.write_all(body)?;
    Ok(())
}
fn post(address: std::net::SocketAddr, path: &str, token: &str, body: &str) -> Result<String> {
    let mut s = TcpStream::connect(address)?;
    s.set_read_timeout(Some(Duration::from_secs(3)))?;
    write!(
        s,
        "POST {path} HTTP/1.1\r\nHost: {address}\r\nAuthorization: Bearer {token}\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    )?;
    let mut reply = String::new();
    let mut r = BufReader::new(s);
    let mut len = 0;
    let mut line = String::new();
    loop {
        line.clear();
        r.read_line(&mut line)?;
        reply.push_str(&line);
        if line == "\r\n" {
            break;
        }
        if let Some((k, v)) = line.split_once(':')
            && k.eq_ignore_ascii_case("content-length")
        {
            len = v.trim().parse::<usize>()?;
        }
    }
    let mut b = vec![0; len];
    r.read_exact(&mut b)?;
    reply.push_str(std::str::from_utf8(&b)?);
    Ok(reply)
}
#[test]
fn scoped_assistant_proxy_is_opt_in_and_sends_once() -> Result<()> {
    let t = support::Directory::new()?;
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let url = format!("http://{}/webhook?agent=phone", listener.local_addr()?);
    let worker = thread::spawn(move || -> Result<()> {
        let (mut s, _) = listener.accept()?;
        let (path, body) = request(&mut s)?;
        assert!(path.contains("/webhook?agent=phone"));
        assert_eq!(
            serde_json::from_slice::<Value>(&body)?["message"],
            "read status"
        );
        response(&mut s, br#"{"response":"Ready"}"#)?;
        Ok(())
    });
    let cfg=t.config("api.json",&json!({"listen":"127.0.0.1:0","dashboard_token":"0".repeat(64),"clients":[{"name":"no","token":"1".repeat(64)},{"name":"yes","token":"2".repeat(64),"assistant":true}],"assistant":{"url":url,"bearer_token":"synthetic-private"}}))?;
    let server = api::Server::start(&cfg, &t.0.join("unused.sock"))?;
    assert!(
        post(
            server.local_addr(),
            "/api/assistant",
            &"1".repeat(64),
            r#"{"message":"read status"}"#
        )?
        .starts_with("HTTP/1.1 403")
    );
    assert!(
        post(
            server.local_addr(),
            "/api/assistant",
            "bad",
            r#"{"message":"read status"}"#
        )?
        .starts_with("HTTP/1.1 401")
    );
    assert!(
        post(
            server.local_addr(),
            "/api/assistant",
            &"2".repeat(64),
            r#"{"message":"read status"}"#
        )?
        .contains("Ready")
    );
    worker.join().expect("server")?;
    let rejected = post(
        server.local_addr(),
        "/api/assistant",
        &"2".repeat(64),
        &"x".repeat(8193),
    );
    assert!(match rejected {
        Ok(reply) => reply.starts_with("HTTP/1.1 413"),
        Err(e) => e
            .downcast_ref::<std::io::Error>()
            .is_some_and(|e| e.kind() == std::io::ErrorKind::ConnectionReset),
    });
    drop(server);
    Ok(())
}
#[test]
fn dlna_verifies_identity_before_soap_and_parses_readback() -> Result<()> {
    let t = support::Directory::new()?;
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let url = format!("http://{}/description", listener.local_addr()?);
    let worker = thread::spawn(move || -> Result<()> {
        for n in 0..5 {
            let (mut s, _) = listener.accept()?;
            let (path, body) = request(&mut s)?;
            let reply = if n == 0 {
                assert!(path.starts_with("GET "));
                "<root><device><UDN>uuid:samsung</UDN><serviceList><service><serviceType>urn:schemas-upnp-org:service:AVTransport:1</serviceType><controlURL>/av</controlURL></service><service><serviceType>urn:schemas-upnp-org:service:RenderingControl:1</serviceType><controlURL>/render</controlURL></service></serviceList></device></root>".to_owned()
            } else {
                let b = String::from_utf8(body)?;
                let (action, value) = if b.contains("GetTransportInfo") {
                    (
                        "GetTransportInfo",
                        "<CurrentTransportState>STOPPED</CurrentTransportState>",
                    )
                } else if b.contains("GetPositionInfo") {
                    (
                        "GetPositionInfo",
                        "<RelTime>00:00:12</RelTime><TrackDuration>00:03:00</TrackDuration><TrackURI>http://example.org/audio</TrackURI>",
                    )
                } else if b.contains("GetVolume") {
                    ("GetVolume", "<CurrentVolume>23</CurrentVolume>")
                } else {
                    assert!(b.contains("GetMute"));
                    ("GetMute", "<CurrentMute>0</CurrentMute>")
                };
                format!(
                    "<s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\"><s:Body><{action}Response>{value}</{action}Response></s:Body></s:Envelope>"
                )
            };
            response(&mut s, reply.as_bytes())?;
        }
        Ok(())
    });
    let path = t.config(
        "dlna.json",
        &json!({"name":"samsung","description_url":url,"udn":"uuid:samsung"}),
    )?;
    let mut m = dlna::Manager::open(&path)?;
    let state = m.read_device(0)?;
    assert_eq!(state["volume_percent"], 23);
    assert_eq!(state["muted"], false);
    worker.join().expect("server")?;
    drop(m);
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let url = format!("http://{}/description", listener.local_addr()?);
    let worker = thread::spawn(move || -> Result<()> {
        let (mut s, _) = listener.accept()?;
        request(&mut s)?;
        response(&mut s, b"<root><UDN>uuid:other</UDN></root>")?;
        Ok(())
    });
    let path = t.config(
        "dlna2.json",
        &json!({"name":"samsung","description_url":url,"udn":"uuid:samsung"}),
    )?;
    let mut m = dlna::Manager::open(&path)?;
    assert!(
        m.control_device(0, Command::Playback, &json!({"command":{"action":"stop"}}))
            .is_err()
    );
    worker.join().expect("server")?;
    Ok(())
}
#[test]
fn cast_rejects_changed_tls_identity_before_commands() -> Result<()> {
    use rustls::pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject};
    let t = support::Directory::new()?;
    let (cert, key) = t.certs()?;
    let cfg = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()?
    .with_no_client_auth()
    .with_single_cert(
        vec![CertificateDer::from_pem_slice(&std::fs::read(cert)?)?],
        PrivateKeyDer::from_pem_slice(&std::fs::read(key)?)?,
    )?;
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let address = listener.local_addr()?;
    let worker = thread::spawn(move || -> Result<()> {
        let (s, _) = listener.accept()?;
        s.set_read_timeout(Some(Duration::from_secs(3)))?;
        let mut tls = rustls::StreamOwned::new(rustls::ServerConnection::new(Arc::new(cfg))?, s);
        let mut b = [0; 4];
        assert!(tls.read_exact(&mut b).is_err());
        Ok(())
    });
    let path=t.config("cast.json",&json!({"name":"cast","host":"127.0.0.1","port":address.port(),"server_cert_sha256":"0".repeat(64)}))?;
    let mut m = cast::Manager::open(&path)?;
    assert!(m.read_device(0).is_err());
    worker.join().expect("server")?;
    Ok(())
}
