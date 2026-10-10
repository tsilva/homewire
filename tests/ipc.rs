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
                .stderr(Stdio::null())
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
