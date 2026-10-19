#![cfg(unix)]
use homewire::{Result, ipc};
use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    os::unix::{
        fs::PermissionsExt,
        net::{UnixListener, UnixStream},
    },
    path::PathBuf,
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

#[path = "support/mod.rs"]
mod support;
struct Fixture {
    dir: support::Directory,
    daemon: Option<Child>,
}
impl Fixture {
    fn new() -> Result<Self> {
        let dir = support::Directory::new()?;
        dir.write("awair.json", b"{\"host\":\"192.0.2.1\"}")?;
        Ok(Self { dir, daemon: None })
    }
    fn socket(&self) -> PathBuf {
        self.dir.0.join("homewire.sock")
    }
    fn start(&mut self) -> Result<()> {
        self.daemon = Some(
            Command::new(env!("CARGO_BIN_EXE_homewire"))
                .arg("--awair")
                .arg(self.dir.0.join("awair.json"))
                .arg("daemon")
                .arg(self.socket())
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .spawn()?,
        );
        let until = Instant::now() + Duration::from_secs(5);
        while UnixStream::connect(self.socket()).is_err() {
            assert!(Instant::now() < until, "daemon startup timeout");
            assert!(
                self.daemon.as_mut().expect("child").try_wait()?.is_none(),
                "daemon failed"
            );
            thread::sleep(Duration::from_millis(10));
        }
        Ok(())
    }
    fn stop(&mut self) {
        if let Some(mut p) = self.daemon.take() {
            let _ = p.kill();
            let _ = p.wait();
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.stop();
    }
}
fn list(f: &Fixture) -> Result<Value> {
    Ok(ipc::request(
        &f.socket(),
        &json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}).to_string(),
    )?
    .expect("response"))
}
#[test]
fn daemon_survives_bridge_eof_and_restarts_with_stale_socket() -> Result<()> {
    let mut f = Fixture::new()?;
    f.start()?;
    for _ in 0..2 {
        let mut bridge = Command::new(env!("CARGO_BIN_EXE_homewire"))
            .arg("bridge")
            .arg(f.socket())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let mut input = bridge.stdin.take().expect("stdin");
        writeln!(
            input,
            "{}",
            json!({"jsonrpc":"2.0","method":"notifications/initialized"})
        )?;
        writeln!(
            input,
            "{}",
            json!({"jsonrpc":"2.0","id":1,"method":"tools/list"})
        )?;
        drop(input);
        let output = bridge.wait_with_output()?;
        assert!(output.status.success());
        assert!(output.stderr.is_empty());
        let replies: Vec<_> = String::from_utf8(output.stdout)?
            .lines()
            .map(str::to_owned)
            .collect();
        assert_eq!(replies.len(), 1); // notification ACK remains private
        assert_eq!(
            serde_json::from_str::<Value>(&replies[0])?["result"]["tools"]
                .as_array()
                .expect("tools")
                .len(),
            2
        );
        assert!(f.daemon.as_mut().expect("daemon").try_wait()?.is_none());
    }
    f.stop();
    f.start()?;
    assert!(list(&f)?["result"]["tools"].is_array());
    Ok(())
}
#[test]
fn malformed_and_disconnected_clients_do_not_stop_owner() -> Result<()> {
    let mut f = Fixture::new()?;
    f.start()?;
    let bad = ipc::request(&f.socket(), "{broken\n")?.expect("parse reply");
    assert_eq!(bad["error"]["code"], -32700);
    drop(UnixStream::connect(f.socket())?);
    assert!(ipc::request(&f.socket(), &"x".repeat(65537)).is_err());
    assert!(list(&f)?["result"]["tools"].is_array());
    // A competing owner fails without unlinking the first owner's socket.
    assert!(ipc::Server::bind(&f.socket()).is_err());
    assert!(list(&f)?["result"]["tools"].is_array());
    Ok(())
}
#[test]
fn private_parent_and_non_socket_replacement_are_enforced() -> Result<()> {
    let f = Fixture::new()?;
    fs::set_permissions(&f.dir.0, fs::Permissions::from_mode(0o755))?;
    assert!(ipc::Server::bind(&f.socket()).is_err());
    fs::set_permissions(&f.dir.0, fs::Permissions::from_mode(0o700))?;
    fs::write(f.socket(), "keep")?;
    assert!(ipc::Server::bind(&f.socket()).is_err());
    assert_eq!(fs::read_to_string(f.socket())?, "keep");
    Ok(())
}
#[test]
fn bridge_never_retries_after_connection_loses_reply() -> Result<()> {
    let f = Fixture::new()?;
    let listener = UnixListener::bind(f.socket())?;
    let worker = thread::spawn(move || -> Result<()> {
        let (stream, _) = listener.accept()?;
        // Drop after accepting a write; caller cannot know control outcome.
        drop(stream);
        listener.set_nonblocking(true)?;
        thread::sleep(Duration::from_millis(50));
        assert_eq!(
            listener.accept().expect_err("no retry").kind(),
            std::io::ErrorKind::WouldBlock
        );
        Ok(())
    });
    assert!(
        ipc::request(
            &f.socket(),
            &json!({"jsonrpc":"2.0","id":1,"method":"tools/call"}).to_string()
        )
        .is_err()
    );
    worker.join().expect("worker")?;
    Ok(())
}

#[test]
fn daemon_background_jobs_run_without_an_mcp_client() -> Result<()> {
    let f = Fixture::new()?;
    let endpoint = std::net::TcpListener::bind("127.0.0.1:0")?;
    let address = endpoint.local_addr()?;
    drop(endpoint);
    let config = f.dir.0.join("printers.json");
    fs::write(&config,json!({"printers":[{"name":"mock","host":"192.0.2.1","connect":address.to_string()}],"poll_seconds":5,"timeout_seconds":1}).to_string())?;
    fs::set_permissions(&config, fs::Permissions::from_mode(0o600))?;
    let mut child = Command::new(env!("CARGO_BIN_EXE_homewire"))
        .arg("--printers")
        .arg(config)
        .arg("--watch-printers")
        .arg("daemon")
        .arg(f.socket())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()?;
    thread::sleep(Duration::from_secs(6));
    child.kill()?;
    let output = child.wait_with_output()?;
    assert!(String::from_utf8(output.stderr)?.contains("Printer unavailable"));
    Ok(())
}

#[test]
fn protocol_replies_bypass_blocked_owner_and_full_device_queue() -> Result<()> {
    use std::io::{BufRead, BufReader};
    let f = Fixture::new()?;
    let server = ipc::Server::bind_with_protocol(&f.socket(), |request| {
        if request["method"] == "homewire/api" {
            homewire::api::dispatch_protocol(homewire::device::Selection::ALL, request, false)
        } else {
            homewire::mcp::handle_capabilities(
                request,
                homewire::device::Selection::ALL,
                false,
                |_, _| Err(homewire::error("Device calls belong to the owner")),
            )
        }
    })?;
    let connect = |id| -> Result<UnixStream> {
        let mut stream = UnixStream::connect(f.socket())?;
        stream.set_read_timeout(Some(Duration::from_secs(2)))?;
        writeln!(
            stream,
            "{}",
            json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{"name":"fake_control"}})
        )?;
        Ok(stream)
    };
    let response = |stream: UnixStream| -> Result<Value> {
        let mut line = String::new();
        BufReader::new(stream).read_line(&mut line)?;
        Ok(serde_json::from_str(&line)?)
    };
    let first = connect(41)?;
    let active = server.incoming.recv_timeout(Duration::from_secs(2))?;
    let queued_clients: Vec<_> = (0..ipc::DEVICE_QUEUE_CAPACITY)
        .map(|i| connect(42 + i as u64))
        .collect::<Result<_>>()?;
    // FIFO acceptance of the following ping proves the queued call was parsed.
    let start = Instant::now();
    for method in ["ping", "initialize", "tools/list"] {
        let reply = ipc::request(
            &f.socket(),
            &json!({"jsonrpc":"2.0","id":43,"method":method}).to_string(),
        )?
        .expect("protocol reply");
        assert!(reply.get("result").is_some(), "{reply}");
    }
    assert!(start.elapsed() < Duration::from_secs(2));
    let scoped = ipc::request(&f.socket(), &json!({"jsonrpc":"2.0","id":1,"method":"homewire/api","params":{"role":"agent","route":"/mcp","tools":["entity_list"],"data":{"jsonrpc":"2.0","id":9,"method":"tools/list"}}}).to_string())?.expect("agent metadata reply");
    let tools = scoped["result"]["result"]["tools"]
        .as_array()
        .expect("scoped tools");
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0]["name"], "entity_list");
    let rejected = response(connect(99)?)?;
    assert_eq!(rejected["error"]["code"], -32000);
    assert!(
        rejected["error"]["message"]
            .as_str()
            .expect("message")
            .contains("not queued or executed")
    );
    let invalid = ipc::request(
        &f.socket(),
        r#"{"jsonrpc":"2.0","id":true,"method":"tools/call"}"#,
    )?
    .expect("invalid reply");
    assert_eq!(invalid["error"]["code"], -32600);
    assert!(
        ipc::request(
            &f.socket(),
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#
        )?
        .is_none()
    );
    let queued: Vec<_> = (0..ipc::DEVICE_QUEUE_CAPACITY)
        .map(|_| server.incoming.recv_timeout(Duration::from_secs(2)))
        .collect::<std::result::Result<_, _>>()?;
    let mut executed = Vec::new();
    active.exchange(|request| {
        executed.push(request["id"].clone());
        Some(json!({"ok":true}))
    })?;
    for pending in queued {
        pending.exchange(|request| {
            executed.push(request["id"].clone());
            Some(json!({"ok":true}))
        })?;
    }
    let expected: Vec<_> = std::iter::once(json!(41))
        .chain((0..ipc::DEVICE_QUEUE_CAPACITY).map(|i| json!(42 + i)))
        .collect();
    assert_eq!(executed, expected);
    assert_eq!(response(first)?, json!({"ok":true}));
    for client in queued_clients {
        assert_eq!(response(client)?, json!({"ok":true}));
    }
    assert!(matches!(
        server.incoming.try_recv(),
        Err(std::sync::mpsc::TryRecvError::Empty)
    ));
    Ok(())
}

#[test]
fn daemon_answers_protocol_while_printer_monitor_is_waiting_on_network() -> Result<()> {
    use std::io::{BufRead, BufReader, Read};
    use std::sync::mpsc;
    let mut f = Fixture::new()?;
    let endpoint = std::net::TcpListener::bind("127.0.0.1:0")?;
    let address = endpoint.local_addr()?;
    let config = f.dir.0.join("printers.json");
    fs::write(&config,json!({"printers":[{"name":"mock","host":"192.0.2.1","connect":address.to_string()}],"poll_seconds":5,"timeout_seconds":3}).to_string())?;
    fs::set_permissions(&config, fs::Permissions::from_mode(0o600))?;
    let (entered, waiting) = mpsc::sync_channel(1);
    let (release, resume) = mpsc::sync_channel(1);
    let device = thread::spawn(move || -> std::io::Result<()> {
        let (mut socket, _) = endpoint.accept()?;
        socket.set_read_timeout(Some(Duration::from_secs(2)))?;
        let mut bytes = [0; 1024];
        assert!(socket.read(&mut bytes)? > 0);
        entered.send(()).expect("test receiver");
        let _ = resume.recv_timeout(Duration::from_secs(5));
        // No IPP reply: retain explicit unavailable handling instead of inventing success.
        Ok(())
    });
    f.daemon = Some(
        Command::new(env!("CARGO_BIN_EXE_homewire"))
            .arg("--printers")
            .arg(config)
            .arg("--watch-printers")
            .arg("daemon")
            .arg(f.socket())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()?,
    );
    waiting.recv_timeout(Duration::from_secs(8))?;
    let start = Instant::now();
    let ping =
        ipc::request(&f.socket(), r#"{"jsonrpc":"2.0","id":7,"method":"ping"}"#)?.expect("ping");
    assert_eq!(ping, json!({"jsonrpc":"2.0","id":7,"result":{}}));
    assert_eq!(
        list(&f)?["result"]["tools"]
            .as_array()
            .expect("tools")
            .len(),
        2
    );
    assert!(start.elapsed() < Duration::from_secs(1));
    release.send(())?;
    device.join().expect("device")?;
    // A real owner request establishes that polling finished and dispatch continues.
    let mut stream = UnixStream::connect(f.socket())?;
    stream.set_read_timeout(Some(Duration::from_secs(4)))?;
    writeln!(
        stream,
        "{}",
        json!({"jsonrpc":"2.0","id":8,"method":"tools/call","params":{"name":"entity_list"}})
    )?;
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line)?;
    assert!(serde_json::from_str::<Value>(&line)?["result"].is_object());
    let mut child = f.daemon.take().expect("child");
    let stopped = child.kill();
    let output = child.wait_with_output()?;
    stopped?;
    assert!(String::from_utf8(output.stderr)?.contains("Printer unavailable"));
    Ok(())
}

#[test]
fn fragmented_client_cannot_extend_the_listener_frame_deadline() -> Result<()> {
    use std::io::{BufRead, BufReader};
    let f = Fixture::new()?;
    let _server = ipc::Server::bind(&f.socket())?;
    let mut trickle = UnixStream::connect(f.socket())?;
    trickle.write_all(b"{")?;
    let sender = thread::spawn(move || {
        for _ in 0..35 {
            thread::sleep(Duration::from_millis(200));
            if trickle.write_all(b" ").is_err() {
                break;
            }
        }
    });
    let mut client = UnixStream::connect(f.socket())?;
    client.set_read_timeout(Some(Duration::from_secs(7)))?;
    writeln!(
        client,
        "{}",
        json!({"jsonrpc":"2.0","id":17,"method":"ping"})
    )?;
    let started = Instant::now();
    let mut line = String::new();
    BufReader::new(client).read_line(&mut line)?;
    assert_eq!(
        serde_json::from_str::<Value>(&line)?,
        json!({"jsonrpc":"2.0","id":17,"result":{}})
    );
    assert!(started.elapsed() < Duration::from_millis(6500));
    sender.join().expect("sender");
    Ok(())
}

#[test]
fn admitted_request_is_served_before_another_overdue_background_poll() -> Result<()> {
    use std::io::{BufRead, BufReader, Read};
    use std::sync::mpsc;
    let mut f = Fixture::new()?;
    let air = std::net::TcpListener::bind("127.0.0.1:0")?;
    let printer = std::net::TcpListener::bind("127.0.0.1:0")?;
    let air_config = f.dir.config(
        "awair.json",
        &json!({"host":"192.0.2.1","connect":air.local_addr()?.to_string(),"timeout_seconds":10}),
    )?;
    let printer_config = f.dir.config("printers.json", &json!({"printers":[{"name":"mock","host":"192.0.2.2","connect":printer.local_addr()?.to_string()}],"poll_seconds":5,"timeout_seconds":3}))?;
    let (entered, waiting) = mpsc::sync_channel(1);
    let (release, resume) = mpsc::sync_channel(1);
    let air_device = thread::spawn(move || -> Result<()> {
        let (mut socket, _) = air.accept()?;
        socket.set_read_timeout(Some(Duration::from_secs(2)))?;
        let mut bytes = [0; 1024];
        assert!(socket.read(&mut bytes)? > 0);
        entered.send(())?;
        resume.recv_timeout(Duration::from_secs(8))?;
        let body = json!({"timestamp":chrono::Utc::now().to_rfc3339(),"score":91,"temp":24.75,"humid":51.2,"co2":620,"voc":180.5,"pm25":3.25}).to_string();
        write!(
            socket,
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )?;
        Ok(())
    });
    f.daemon = Some(
        Command::new(env!("CARGO_BIN_EXE_homewire"))
            .arg("--awair")
            .arg(air_config)
            .arg("--printers")
            .arg(printer_config)
            .arg("--watch-printers")
            .arg("daemon")
            .arg(f.socket())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?,
    );
    let until = Instant::now() + Duration::from_secs(3);
    while !f.socket().exists() {
        assert!(Instant::now() < until);
        thread::sleep(Duration::from_millis(10));
    }
    let path = f.socket();
    let foreground = thread::spawn(move || {
        ipc::request(&path,&json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"entity_status","arguments":{"entities":["sensor.awair_temperature"]}}}).to_string())
    });
    waiting.recv_timeout(Duration::from_secs(3))?;
    let mut queued = UnixStream::connect(f.socket())?;
    queued.set_read_timeout(Some(Duration::from_millis(1500)))?;
    writeln!(
        queued,
        "{}",
        json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"entity_list"}})
    )?;
    // The printer deadline becomes overdue while the foreground read is active.
    thread::sleep(Duration::from_secs(6));
    release.send(())?;
    air_device.join().expect("air device")?;
    let reply = foreground.join().expect("foreground")?.expect("reply");
    assert_eq!(reply["result"]["isError"], false, "{reply}");
    let mut line = String::new();
    BufReader::new(queued).read_line(&mut line)?;
    assert_eq!(serde_json::from_str::<Value>(&line)?["id"], 2);
    // Background work must resume after the admitted batch, rather than starve.
    printer.set_nonblocking(true)?;
    let until = Instant::now() + Duration::from_secs(2);
    let mut poll = loop {
        match printer.accept() {
            Ok((socket, _)) => break socket,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                assert!(Instant::now() < until, "background poll starved");
                thread::sleep(Duration::from_millis(10));
            }
            Err(e) => return Err(e.into()),
        }
    };
    poll.set_read_timeout(Some(Duration::from_secs(1)))?;
    let mut bytes = [0; 1024];
    assert!(poll.read(&mut bytes)? > 0);
    Ok(())
}

#[test]
fn deeply_nested_frames_do_not_stop_the_protocol_listener() -> Result<()> {
    let mut f = Fixture::new()?;
    f.start()?;
    for depth in [16, 64, 128, 192] {
        let nested = format!("{}0{}", "[".repeat(depth), "]".repeat(depth));
        let frame = format!(r#"{{"jsonrpc":"2.0","id":1,"method":"ping","params":{nested}}}"#);
        eprintln!("Testing depth {depth}");
        let reply = match ipc::request(&f.socket(), &frame) {
            Ok(reply) => reply.expect("reply"),
            Err(e) => {
                let mut child = f.daemon.take().expect("daemon");
                let stopped = child.kill();
                let output = child.wait_with_output()?;
                let _ = stopped;
                panic!(
                    "depth {depth}: {e}; status {}; stderr {}",
                    output.status,
                    String::from_utf8_lossy(&output.stderr)
                );
            }
        };
        assert!(reply.get("result").is_some() || reply["error"]["code"] == -32700);
        assert!(f.daemon.as_mut().expect("daemon").try_wait()?.is_none());
    }
    assert!(list(&f)?["result"]["tools"].is_array());
    Ok(())
}
