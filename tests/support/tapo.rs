#![allow(dead_code)]
use homewire::{
    Result,
    klap::{Session, hash},
};
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Read, Write},
    net::{SocketAddr, TcpListener},
    thread,
    time::Duration,
};
pub fn read_request(socket: &mut std::net::TcpStream) -> Result<(String, Vec<u8>, String)> {
    socket.set_read_timeout(Some(Duration::from_secs(3)))?;
    let mut reader = BufReader::new(socket.try_clone()?);
    let mut headers = String::new();
    let mut length = 0;
    loop {
        let mut line = String::new();
        reader.read_line(&mut line)?;
        assert!(!line.is_empty());
        headers.push_str(&line);
        assert!(headers.len() < 8192);
        if let Some(v) = line.strip_prefix("Content-Length: ") {
            length = v.trim().parse()?;
        }
        if line == "\r\n" {
            break;
        }
    }
    assert!(length <= 16384 + 48);
    let mut body = vec![0; length];
    reader.read_exact(&mut body)?;
    let path = headers
        .lines()
        .next()
        .expect("first")
        .split_whitespace()
        .nth(1)
        .expect("path")
        .to_owned();
    assert!(headers.starts_with("POST "));
    assert!(headers.contains("Host: 192.0.2.20:80\r\n"));
    Ok((path, body, headers))
}
pub fn respond(socket: &mut std::net::TcpStream, body: &[u8], cookie: bool) -> Result<()> {
    let extra = if cookie {
        "Set-Cookie: TP_SESSIONID=synthetic-session; Path=/\r\nSet-Cookie: TIMEOUT=86400\r\n"
    } else {
        ""
    };
    socket.write_all(
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\n{extra}\r\n",
            body.len()
        )
        .as_bytes(),
    )?;
    socket.write_all(body)?;
    Ok(())
}
pub fn mock_sessions(
    sessions: Vec<(Vec<&'static str>, Option<usize>)>,
    initial: Value,
    energy: Option<(Value, Value)>,
) -> Result<(SocketAddr, thread::JoinHandle<Result<Vec<Value>>>)> {
    mock_sessions_with_energy_replies(
        sessions,
        initial,
        energy.map(|(usage, meter)| {
            (
                json!({"error_code":0,"result":usage}),
                json!({"error_code":0,"result":meter}),
            )
        }),
    )
}
pub fn mock_sessions_with_energy_replies(
    sessions: Vec<(Vec<&'static str>, Option<usize>)>,
    initial: Value,
    energy: Option<(Value, Value)>,
) -> Result<(SocketAddr, thread::JoinHandle<Result<Vec<Value>>>)> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let address = listener.local_addr()?;
    let worker = thread::spawn(move || -> Result<Vec<Value>> {
        let mut requests = vec![];
        let mut state = initial;
        for (methods, denied) in sessions {
            let remote = [7; 16];
            let auth = [0; 32];
            let (mut socket, _) = listener.accept()?;
            let (path, local, headers) = read_request(&mut socket)?;
            assert_eq!(path, "/app/handshake1");
            assert!(!headers.contains("Cookie:"));
            let local: [u8; 16] = local.try_into().expect("seed");
            let mut body = remote.to_vec();
            body.extend_from_slice(&hash(&[&local, &remote, &auth]));
            respond(&mut socket, &body, true)?;
            drop(socket);
            let (mut socket, _) = listener.accept()?;
            let (path, proof, headers) = read_request(&mut socket)?;
            assert_eq!(path, "/app/handshake2");
            assert_eq!(proof, hash(&[&remote, &local, &auth]));
            assert!(headers.contains("Cookie: TP_SESSIONID=synthetic-session\r\n"));
            assert!(!headers.contains("TIMEOUT="));
            respond(&mut socket, &[], false)?;
            drop(socket);
            for (i, method) in methods.into_iter().enumerate() {
                let (mut socket, _) = listener.accept()?;
                let (path, body, headers) = read_request(&mut socket)?;
                assert!(headers.contains("Cookie: TP_SESSIONID=synthetic-session\r\n"));
                let mut codec = Session::new(&local, &remote, &auth)?;
                let mut seq = 0;
                for _ in 0..=i {
                    seq = codec.encrypt(b"")?.1;
                }
                assert_eq!(path, format!("/app/request?seq={seq}"));
                let request: Value = serde_json::from_slice(&codec.decrypt(&body)?)?;
                assert_eq!(request["method"], method);
                if denied == Some(i) {
                    socket.write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n")?;
                    requests.push(request);
                    break;
                }
                let response = if method == "set_device_info" {
                    state
                        .as_object_mut()
                        .expect("object")
                        .extend(request["params"].as_object().expect("params").clone());
                    json!({"error_code":0})
                } else if method == "get_energy_usage" {
                    energy.as_ref().expect("energy fixture").0.clone()
                } else if method == "get_emeter_data" {
                    energy.as_ref().expect("energy fixture").1.clone()
                } else {
                    json!({"error_code":0,"result":state})
                };
                let mut codec = Session::new(&local, &remote, &auth)?;
                for _ in 0..i {
                    codec.encrypt(b"")?;
                }
                let body = codec.encrypt(&serde_json::to_vec(&response)?)?.0;
                respond(&mut socket, &body, false)?;
                requests.push(request);
            }
        }
        Ok(requests)
    });
    Ok((address, worker))
}
