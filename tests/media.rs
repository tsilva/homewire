use homewire::{
    Result,
    device::{Adapter, Devices, Selection},
    mcp, samsung,
};
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Read, Write},
    net::{SocketAddr, TcpListener, UdpSocket},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant},
};
#[path = "support/media.rs"]
mod support;
use aes::Aes128;
use base64::{Engine, engine::general_purpose::STANDARD};
use cipher::{BlockDecrypt, KeyInit};
use sha1::{Digest, Sha1};
struct Samsung {
    address: SocketAddr,
    on: Arc<AtomicBool>,
    keys: Arc<Mutex<Vec<String>>>,
    wrong: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<Result<()>>>,
    queries: Arc<AtomicUsize>,
}
fn ws_send(stream: &mut impl Write, text: &str) -> Result<()> {
    let b = text.as_bytes();
    stream.write_all(&[0x81, b.len() as u8])?;
    stream.write_all(b)?;
    Ok(())
}
fn ws_read(stream: &mut impl Read) -> Result<String> {
    let mut h = [0; 2];
    stream.read_exact(&mut h)?;
    assert_eq!(h[0], 0x81);
    assert_ne!(h[1] & 128, 0);
    let len = if h[1] & 127 == 126 {
        let mut n = [0; 2];
        stream.read_exact(&mut n)?;
        u16::from_be_bytes(n) as usize
    } else {
        (h[1] & 127) as usize
    };
    assert!(len <= 8192);
    let mut mask = [0; 4];
    stream.read_exact(&mut mask)?;
    let mut b = vec![0; len];
    stream.read_exact(&mut b)?;
    for (i, b) in b.iter_mut().enumerate() {
        *b ^= mask[i % 4];
    }
    Ok(String::from_utf8(b)?)
}
impl Samsung {
    fn new(explicit: bool) -> Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let address = listener.local_addr()?;
        listener.set_nonblocking(true)?;
        let on = Arc::new(AtomicBool::new(true));
        let wrong = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(false));
        let keys = Arc::new(Mutex::new(Vec::new()));
        let queries = Arc::new(AtomicUsize::new(0));
        let (power, bad, done, commands, count) = (
            on.clone(),
            wrong.clone(),
            stop.clone(),
            keys.clone(),
            queries.clone(),
        );
        let worker = thread::spawn(move || -> Result<()> {
            while !done.load(Ordering::Relaxed) {
                let socket = match listener.accept() {
                    Ok((s, _)) => s,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(e) => return Err(e.into()),
                };
                socket.set_nonblocking(false)?;
                socket.set_read_timeout(Some(Duration::from_secs(2)))?;
                socket.set_write_timeout(Some(Duration::from_secs(2)))?;
                let mut stream = BufReader::new(socket);
                let mut line = String::new();
                stream.read_line(&mut line)?;
                let path = line
                    .split_whitespace()
                    .nth(1)
                    .unwrap_or_default()
                    .to_string();
                let mut wskey = String::new();
                loop {
                    line.clear();
                    stream.read_line(&mut line)?;
                    if line == "\r\n" {
                        break;
                    }
                    if let Some(v) = line.strip_prefix("Sec-WebSocket-Key: ") {
                        wskey = v.trim().into();
                    }
                }
                if path.starts_with("/socket.io/1/websocket/") {
                    let accept = STANDARD.encode(Sha1::digest(format!(
                        "{wskey}258EAFA5-E914-47DA-95CA-C5AB0DC85B11"
                    )));
                    write!(
                        stream.get_mut(),
                        "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n"
                    )?;
                    assert_eq!(ws_read(&mut stream)?, "1::/com.samsung.companion");
                    ws_send(stream.get_mut(), "2::")?;
                    assert_eq!(ws_read(&mut stream)?, "2::");
                    ws_send(stream.get_mut(), "1::/com.samsung.companion")?;
                    if let Ok(command) = ws_read(&mut stream) {
                        let value: Value = serde_json::from_str(
                            command
                                .strip_prefix("5::/com.samsung.companion:")
                                .expect("companion"),
                        )?;
                        assert_eq!(value["args"][0]["Session_Id"], 42);
                        let mut b: Vec<u8> =
                            serde_json::from_str(value["args"][0]["body"].as_str().expect("body"))?;
                        assert_eq!(b.len() % 16, 0);
                        let cipher = Aes128::new_from_slice(&[0; 16]).expect("key");
                        for block in b.chunks_exact_mut(16) {
                            cipher.decrypt_block(
                                cipher::generic_array::GenericArray::from_mut_slice(block),
                            );
                        }
                        let pad = *b.last().expect("padding") as usize;
                        assert!((1..=16).contains(&pad));
                        assert!(b[b.len() - pad..].iter().all(|n| *n as usize == pad));
                        b.truncate(b.len() - pad);
                        let value: Value = serde_json::from_slice(&b)?;
                        assert_eq!(value["method"], "POST");
                        assert_eq!(value["body"]["api"], "SendRemoteKey");
                        let key = value["body"]["param3"].as_str().expect("key").to_string();
                        if key == "KEY_POWEROFF" {
                            power.store(false, Ordering::Relaxed);
                        }
                        commands.lock().expect("keys").push(key);
                    }
                } else {
                    let body = if path == "/api/v2/" {
                        count.fetch_add(1, Ordering::Relaxed);
                        let mut v = json!({"device":{"modelName":if bad.load(Ordering::Relaxed){"wrong"}else{"UE40H6200"},"id":"test-id"}});
                        if explicit {
                            v["device"]["PowerState"] = json!(if power.load(Ordering::Relaxed) {
                                "on"
                            } else {
                                "standby"
                            });
                        }
                        v.to_string()
                    } else {
                        assert_eq!(path, "/socket.io/1/");
                        "session:60:60:websocket".into()
                    };
                    write!(
                        stream.get_mut(),
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )?;
                }
            }
            Ok(())
        });
        Ok(Self {
            address,
            on,
            keys,
            wrong,
            stop,
            worker: Some(worker),
            queries,
        })
    }
    fn config(&self, temp: &support::Directory, wake: SocketAddr) -> Result<std::path::PathBuf> {
        temp.config("samsung.json",&json!({"host":"192.0.2.194","connect":self.address,"info_connect":self.address,"device_id":"test-id","mac":"00:11:22:33:44:55","token":"00".repeat(16),"session_id":42,"wake_address":wake,"timeout_seconds":2,"verify_seconds":2}))
    }
}
impl Drop for Samsung {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(w) = self.worker.take() {
            w.join().expect("server").expect("server protocol");
        }
    }
}
#[test]
fn samsung_ecb_command_shutdown_verification_and_restart_guard() -> Result<()> {
    for explicit in [true, false] {
        let server = Samsung::new(explicit)?;
        let temp = support::Directory::new()?;
        let path = server.config(&temp, "127.0.0.1:9".parse()?)?;
        let mut manager = samsung::Manager::open(&path)?;
        assert!(samsung::Manager::open(&path).is_err());
        assert_eq!(manager.status()?.power, Some(true));
        let result = manager.apply(false)?;
        assert_eq!(result["verified"], explicit);
        assert_eq!(
            server.keys.lock().expect("keys").as_slice(),
            ["KEY_POWEROFF"]
        );
        drop(manager);
        let mut manager = samsung::Manager::open(&path)?;
        if explicit {
            assert_eq!(manager.apply(false)?["applied"], false);
        } else {
            assert!(manager.apply(false).is_err());
        }
        assert_eq!(server.keys.lock().expect("keys").len(), 1);
    }
    Ok(())
}
#[test]
fn samsung_wake_magic_packet_and_identity_guard() -> Result<()> {
    let server = Samsung::new(true)?;
    let temp = support::Directory::new()?;
    let socket = UdpSocket::bind("127.0.0.1:0")?;
    socket.set_read_timeout(Some(Duration::from_secs(3)))?;
    let path = server.config(&temp, socket.local_addr()?)?;
    server.on.store(false, Ordering::Relaxed);
    let power = server.on.clone();
    let worker = thread::spawn(move || -> Result<()> {
        let mut packet = [0; 103];
        let n = socket.recv(&mut packet)?;
        assert_eq!(n, 102);
        assert_eq!(packet[..6], [255; 6]);
        for block in packet[6..102].chunks_exact(6) {
            assert_eq!(block, [0, 17, 34, 51, 68, 85]);
        }
        power.store(true, Ordering::Relaxed);
        Ok(())
    });
    let mut devices = Devices {
        samsung: Some(samsung::Manager::open(&path)?),
        ..Devices::default()
    };
    let reply = mcp::handle_entities(json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"wake_on_lan","arguments":{"device":"samsung.samsung"}}}), &mut devices, true).expect("reply");
    assert_eq!(
        reply["result"]["structuredContent"]["result"]["verified"],
        true
    );
    worker.join().expect("wake")?;
    server.wrong.store(true, Ordering::Relaxed);
    assert!(
        homewire::entity::call(
            &mut devices,
            "wake_on_lan",
            &json!({"device":"samsung.samsung"})
        )
        .is_err()
    );
    assert!(
        devices
            .samsung
            .as_mut()
            .expect("manager")
            .apply(false)
            .is_err()
    );
    assert!(server.keys.lock().expect("keys").is_empty());
    Ok(())
}
#[test]
fn samsung_keys_are_masked_encrypted_and_power_toggle_is_not_exposed() -> Result<()> {
    let server = Samsung::new(false)?;
    let temp = support::Directory::new()?;
    let path = server.config(&temp, "127.0.0.1:9".parse()?)?;
    let mut devices = Devices {
        samsung: Some(samsung::Manager::open(&path)?),
        ..Devices::default()
    };
    let count = server.queries.load(Ordering::Relaxed);
    assert!(
        devices
            .call("samsung_key", &json!({"key":"power"}))
            .is_err()
    );
    assert_eq!(count, server.queries.load(Ordering::Relaxed));
    assert_eq!(
        devices.call("samsung_key", &json!({"key":"hdmi"}))?["sent"],
        true
    );
    let until = Instant::now() + Duration::from_secs(2);
    while server.keys.lock().expect("keys").is_empty() && Instant::now() < until {
        thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(server.keys.lock().expect("keys").as_slice(), ["KEY_HDMI"]);
    Ok(())
}
#[test]
fn media_catalog_and_mcp_gate_all_remote_writes_without_io() -> Result<()> {
    let selection = Selection::from_filter(|name| matches!(name, "samsung" | "digi"));
    let request = json!({"jsonrpc":"2.0","id":1,"method":"tools/list"});
    let result = mcp::handle_selected(request, selection, mcp::Options::default(), |_, _| {
        panic!("no I/O")
    })
    .expect("response");
    assert_eq!(
        result["result"]["tools"].as_array().expect("tools").len(),
        2
    );
    for name in ["samsung_apply", "samsung_key", "digi_apply", "digi_key"] {
        let result=mcp::handle_selected(json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":name,"arguments":if name.ends_with("_key"){json!({"key":"mute"})}else{json!({"power":false})}}}),selection,mcp::Options::default(),|_,_|panic!("write bypassed gate")).expect("response");
        assert_eq!(result["result"]["isError"], true);
    }
    for adapter in [samsung::Manager::TOOLS, homewire::androidtv::Manager::TOOLS] {
        for tool in adapter {
            assert!(tool.input.validate(&json!({"extra":true})).is_err());
        }
    }
    // Private relative credential files remain portable with the config directory.
    let temp = support::Directory::new()?;
    temp.certs()?;
    let path=temp.config("digi.json",&json!({"host":"192.0.2.30","server_cert_sha256":"00".repeat(32),"client_cert":"cert.pem","client_key":"key.pem"}))?;
    let mut manager = homewire::androidtv::Manager::open(&path)?;
    assert!(manager.execute("unknown", &json!({})).is_err());
    Ok(())
}

#[test]
fn tv_failed_attempt_survives_restart_and_conflicting_config_owners_fail() -> Result<()> {
    let temp = support::Directory::new()?;
    temp.certs()?;
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let closed = listener.local_addr()?;
    drop(listener);
    temp.config("samsung.json",&json!({"host":"192.0.2.194","device_id":"test-id","mac":"00:11:22:33:44:55","token":"00".repeat(16),"session_id":42,"wake_address":"127.0.0.1:9","connect":closed,"info_connect":closed}))?;
    temp.config("digi.json",&json!({"host":"192.0.2.30","server_cert_sha256":"00".repeat(32),"client_cert":"cert.pem","client_key":"key.pem","connect":closed}))?;
    let mut plugs: Value = serde_json::from_str(include_str!("../examples/plugs.json"))?;
    plugs["plugs"]
        .as_array_mut()
        .expect("plugs")
        .retain(|p| p["name"] == "tv");
    plugs["plugs"][0]["auth_hash"] = json!("00".repeat(32));
    plugs["plugs"][0]["connect"] = json!(closed);
    temp.config("plugs.json", &plugs)?;
    let mut value: Value = serde_json::from_str(include_str!("../examples/tv.json"))?;
    value["shutdown_policy"] = json!("assume_off");
    let path = temp.config("tv.json", &value)?;
    assert!(homewire::tv::Manager::open(&path).is_err());
    value["shutdown_policy"] = json!("fixed_delay");
    let path = temp.config("tv.json", &value)?;
    let mut manager = homewire::tv::Manager::open(&path)?;
    assert!(samsung::Manager::open(&temp.0.join("samsung.json")).is_err());
    assert!(homewire::androidtv::Manager::open(&temp.0.join("digi.json")).is_err());
    assert!(manager.apply(true).is_err());
    drop(manager);
    let mut devices = Devices {
        tv: Some(homewire::tv::Manager::open(&path)?),
        ..Devices::default()
    };
    let selected = devices.selection();
    assert!(selected.tv && selected.plugs && selected.samsung && selected.digi);
    assert_eq!(
        devices.call("plug_list", &json!({}))?["plugs"]
            .as_array()
            .expect("plugs")
            .len(),
        1
    );
    assert!(devices.plugs().is_some());
    assert!(
        devices
            .tv
            .as_mut()
            .expect("workflow")
            .apply(true)
            .expect_err("pending intent")
            .to_string()
            .contains("180 seconds")
    );
    Ok(())
}

#[test]
fn media_cli_rejects_missing_extra_or_invalid_arguments_before_network_io() -> Result<()> {
    for args in [
        vec!["samsung-set"],
        vec!["digi-set"],
        vec!["tv-set"],
        vec!["samsung-status", "extra"],
        vec!["digi-status", "extra"],
        vec!["tv-set", "invalid"],
    ] {
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_homewire"))
            .args(args)
            .output()?;
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert!(!String::from_utf8_lossy(&output.stderr).contains("panicked"));
    }
    Ok(())
}
