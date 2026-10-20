use super::*;
use rustls::{
    ServerConfig, ServerConnection,
    pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject},
};
use sha2::{Digest, Sha256};
use std::{net::TcpListener, thread};
mod support {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/support/mod.rs"));
}
fn reply(
    stream: &mut StreamOwned<ServerConnection, std::net::TcpStream>,
    namespace: &str,
    value: &Value,
) -> Result<()> {
    let mut bytes = Vec::new();
    pb::integer(1, 0, &mut bytes);
    pb::bytes(2, b"receiver-0", &mut bytes);
    pb::bytes(3, b"sender-0", &mut bytes);
    pb::bytes(4, namespace.as_bytes(), &mut bytes);
    pb::integer(5, 0, &mut bytes);
    pb::bytes(6, value.to_string().as_bytes(), &mut bytes);
    stream.write_all(&(bytes.len() as u32).to_be_bytes())?;
    stream.write_all(&bytes)?;
    stream.flush()?;
    Ok(())
}

pub(crate) fn server(
    expected: Option<Value>,
    has_media: bool,
) -> Result<(SocketAddr, thread::JoinHandle<Result<()>>, String)> {
    server_at(expected, has_media, "127.0.0.1:0".parse()?)
}
pub(crate) fn server_at(
    expected: Option<Value>,
    has_media: bool,
    address: SocketAddr,
) -> Result<(SocketAddr, thread::JoinHandle<Result<()>>, String)> {
    let certificate =
        CertificateDer::from_pem_slice(include_bytes!("../../tests/fixtures/media/test-cert.pem"))?;
    let key =
        PrivateKeyDer::from_pem_slice(include_bytes!("../../tests/fixtures/media/test-key.pem"))?;
    let pin = Sha256::digest(certificate.as_ref())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let config =
        ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()?
            .with_no_client_auth()
            .with_single_cert(vec![certificate], key)?;
    let listener = TcpListener::bind(address)?;
    let address = listener.local_addr()?;
    let worker = thread::spawn(move || -> Result<()> {
        let (socket, _) = listener.accept()?;
        socket.set_read_timeout(Some(Duration::from_secs(2)))?;
        socket.set_write_timeout(Some(Duration::from_secs(2)))?;
        let mut stream = StreamOwned::new(ServerConnection::new(Arc::new(config))?, socket);
        let mut launched = has_media;
        let receiver = |active: bool| {
            let mut applications =
                vec![json!({"appId":"native-app","transportId":"native-1","namespaces":[]})];
            if active {
                applications.push(
                    json!({"appId":"CC1AD845","sessionId":"session-1","transportId":"app-1","namespaces":[{"name":MEDIA}]}),
                );
            }
            let mut status =
                json!({"volume":{"level":0.27,"muted":false},"applications":applications});
            if !expected.as_ref().is_some_and(|e| e["omit_standby"] == true) {
                status["isStandBy"] =
                    json!(expected.as_ref().is_none_or(|e| e["standby"] != false));
            }
            status
        };
        let mut control_count = 0;
        for _ in 0..16 {
            let mut length = [0; 4];
            stream.read_exact(&mut length)?;
            let length = u32::from_be_bytes(length) as usize;
            assert!(length > 0 && length <= pb::MAX);
            let mut bytes = vec![0; length];
            stream.read_exact(&mut bytes)?;
            let frame = Message::new(&bytes)?;
            let ns = frame.text(4)?.expect("namespace");
            let request: Value = serde_json::from_str(frame.text(6)?.expect("payload"))?;
            if ns == CONNECTION {
                assert_eq!(request["type"], "CONNECT");
                continue;
            }
            let kind = request["type"].as_str().expect("type");
            let mut done = false;
            let mut response = if ns == RECEIVER && kind == "GET_STATUS" {
                if expected.is_none() && !has_media {
                    done = true;
                }
                if expected.as_ref().is_some_and(|e| {
                    e["action"] == "wake" && (e["standby"] == false || e["omit_standby"] == true)
                }) {
                    done = true;
                }
                json!({"type":"RECEIVER_STATUS","status":receiver(launched)})
            } else if ns == RECEIVER && kind == "STOP" {
                assert!(matches!(
                    expected.as_ref().expect("activation")["action"].as_str(),
                    Some("load" | "wake")
                ));
                assert_eq!(request["sessionId"], "session-1");
                assert!(launched);
                if expected.as_ref().expect("load")["refuse_stop"] == true {
                    done = true;
                } else {
                    launched = false;
                }
                json!({"type":"RECEIVER_STATUS","status":receiver(launched)})
            } else if ns == RECEIVER && kind == "LAUNCH" {
                assert!(matches!(
                    expected.as_ref().expect("activation")["action"].as_str(),
                    Some("load" | "wake")
                ));
                assert_eq!(request["appId"], "CC1AD845");
                assert!(
                    !launched,
                    "existing default receiver must be stopped before replacing URL"
                );
                launched = true;
                if expected
                    .as_ref()
                    .is_some_and(|e| e["disconnect_launch"] == true)
                {
                    // The launch was consumed, but its acknowledgement is lost.
                    // A reconnect/replay would produce a second actuation.
                    return Ok(());
                }
                if expected.as_ref().is_some_and(|e| e["action"] == "wake") {
                    control_count += 1;
                    done = true;
                }
                json!({"type":"RECEIVER_STATUS","status":receiver(true)})
            } else if ns == RECEIVER && kind == "SET_VOLUME" {
                control_count += 1;
                done = true;
                let expected = expected.as_ref().expect("volume command");
                let mut observed = receiver(launched);
                if expected["action"] == "volume" {
                    assert_eq!(request["volume"], json!({"level":0.25}));
                    observed["volume"]["level"] = json!(0.25);
                } else {
                    assert_eq!(request["volume"], json!({"muted":true}));
                    observed["volume"]["muted"] = json!(true);
                }
                json!({"type":"RECEIVER_STATUS","status":observed})
            } else {
                assert_eq!(ns, MEDIA);
                assert_eq!(frame.text(3)?, Some("app-1"));
                assert!(launched, "must not query native app without media support");
                let mut status = json!({"mediaSessionId":7,"playerState":"PAUSED","currentTime":5,"media":{"contentId":"https://media.example/test.mp4","duration":300}});
                if kind == "GET_STATUS" {
                    if expected.is_none() {
                        done = true;
                    }
                } else {
                    control_count += 1;
                    done = true;
                    let expected = expected.as_ref().expect("playback command");
                    match expected["action"].as_str().expect("action") {
                        "seek" => {
                            assert_eq!(kind, "SEEK");
                            assert_eq!(request["currentTime"], 120);
                            status["currentTime"] = json!(120);
                        }
                        "play" => {
                            assert_eq!(kind, "PLAY");
                            status["playerState"] = json!("PLAYING");
                        }
                        "pause" => assert_eq!(kind, "PAUSE"),
                        "stop" => {
                            assert_eq!(kind, "STOP");
                            status["playerState"] = json!("IDLE");
                        }
                        "load" => {
                            assert_eq!(kind, "LOAD");
                            assert_eq!(request["media"]["contentId"], status["media"]["contentId"]);
                            status["playerState"] = json!("PLAYING");
                        }
                        _ => panic!("unexpected test control"),
                    }
                    if kind != "LOAD" {
                        assert_eq!(request["mediaSessionId"], 7);
                    }
                }
                json!({"type":"MEDIA_STATUS","status":[status]})
            };
            response["requestId"] = request["requestId"].clone();
            let mut metadata_reply = None;
            if kind == "LOAD" {
                let mut loading = response.clone();
                loading["status"][0]["playerState"] = json!("IDLE");
                loading["status"][0]["extendedStatus"] = json!({"playerState":"LOADING"});
                reply(&mut stream, ns, &loading)?;
                reply(&mut stream, HEARTBEAT, &json!({"type":"PING"}))?;
                let mut length = [0; 4];
                stream.read_exact(&mut length)?;
                let length = u32::from_be_bytes(length) as usize;
                assert!(length > 0 && length <= pb::MAX);
                let mut bytes = vec![0; length];
                stream.read_exact(&mut bytes)?;
                let pong = Message::new(&bytes)?;
                assert_eq!(pong.text(4)?, Some(HEARTBEAT));
                assert_eq!(
                    serde_json::from_str::<Value>(pong.text(6)?.expect("pong"))?["type"],
                    "PONG"
                );
                if has_media {
                    response["requestId"] = json!(0);
                    let mut unrelated = response.clone();
                    unrelated["status"][0]["mediaSessionId"] = json!(99);
                    reply(&mut stream, ns, &unrelated)?;
                    unrelated = response.clone();
                    unrelated["status"][0]["media"]["contentId"] =
                        json!("https://media.example/other.mp4");
                    reply(&mut stream, ns, &unrelated)?;
                    metadata_reply = Some(response.clone());
                    response["status"][0]
                        .as_object_mut()
                        .expect("status object")
                        .remove("media");
                }
            }
            reply(&mut stream, ns, &response)?;
            if let Some(mut metadata) = metadata_reply {
                let mut length = [0; 4];
                stream.read_exact(&mut length)?;
                let length = u32::from_be_bytes(length) as usize;
                assert!(length > 0 && length <= pb::MAX);
                let mut bytes = vec![0; length];
                stream.read_exact(&mut bytes)?;
                let frame = Message::new(&bytes)?;
                assert_eq!(frame.text(4)?, Some(MEDIA));
                assert_eq!(frame.text(3)?, Some("app-1"));
                let query: Value = serde_json::from_str(frame.text(6)?.expect("metadata query"))?;
                assert_eq!(
                    query["type"], "GET_STATUS",
                    "must read metadata, never repeat LOAD"
                );
                metadata["requestId"] = query["requestId"].clone();
                if expected.as_ref().expect("load")["wrong_metadata"] == true {
                    metadata["status"][0]["media"]["contentId"] =
                        json!("https://media.example/changed.mp4");
                }
                reply(&mut stream, MEDIA, &metadata)?;
            }
            if done {
                assert_eq!(
                    control_count,
                    usize::from(expected.as_ref().is_some_and(|v| v["refuse_stop"] != true
                        && v["standby"] != false
                        && v["omit_standby"] != true))
                );
                return Ok(());
            }
        }
        Err(error("Cast test frame limit exceeded"))
    });
    Ok((address, worker, pin))
}

#[test]
fn cast_wake_connection_and_tls_obey_the_outer_recovery_deadline() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let address = listener.local_addr()?;
    let worker = thread::spawn(move || -> Result<()> {
        let (_socket, _) = listener.accept()?;
        thread::sleep(Duration::from_millis(250));
        Ok(())
    });
    let wake: Wake = serde_json::from_value(json!({"server_cert_sha256":"00".repeat(32),
        "port":address.port(),"timeout_seconds":8}))?;
    let started = Instant::now();
    let failure = match wake.activate(address.ip(), started + Duration::from_millis(80)) {
        Ok(_) => panic!("stalled TLS must fail"),
        Err(failure) => failure,
    };
    assert_eq!(failure.command_sent, Some(false));
    assert!(started.elapsed() < Duration::from_secs(1));
    worker.join().expect("stalled TLS server")?;
    Ok(())
}

#[test]
fn cast_wake_launches_once_without_loading_content_and_preserves_active_apps() -> Result<()> {
    for (standby, has_media) in [(true, false), (true, true), (false, false), (false, true)] {
        let (address, worker, pin) =
            server(Some(json!({"action":"wake","standby":standby})), has_media)?;
        let wake: Wake = serde_json::from_value(
            json!({"server_cert_sha256":pin,"port":address.port(),"timeout_seconds":2}),
        )?;
        wake.validate()?;
        let activated = wake
            .activate(address.ip(), Instant::now() + Duration::from_secs(3))
            .map_err(|e| e.cause)?;
        assert_eq!(activated, standby);
        worker.join().expect("Cast wake server")?;
    }
    Ok(())
}

#[test]
fn cast_wake_rejects_unknown_standby_and_does_not_launch_after_failed_stop() -> Result<()> {
    for expected in [
        json!({"action":"wake","omit_standby":true}),
        json!({"action":"wake","refuse_stop":true}),
    ] {
        let (address, worker, pin) = server(Some(expected.clone()), true)?;
        let wake: Wake = serde_json::from_value(
            json!({"server_cert_sha256":pin,"port":address.port(),"timeout_seconds":2}),
        )?;
        let failure = match wake.activate(address.ip(), Instant::now() + Duration::from_secs(3)) {
            Ok(_) => panic!("wake must fail"),
            Err(e) => e,
        };
        assert_eq!(failure.command_sent, Some(expected["refuse_stop"] == true));
        worker.join().expect("Cast wake server")?;
    }
    Ok(())
}

#[test]
fn pinned_cast_reads_native_receiver_and_media_status_and_verifies_controls() -> Result<()> {
    for expected in [
        None,
        Some(json!({"action":"volume","percent":25})),
        Some(json!({"action":"mute","muted":true})),
        Some(json!({"action":"seek","seconds":120})),
        Some(json!({"action":"play"})),
        Some(json!({"action":"pause"})),
        Some(json!({"action":"stop"})),
        Some(json!({"action":"load"})),
    ] {
        for has_media in [false, true] {
            if !has_media
                && expected.as_ref().is_some_and(|v| {
                    matches!(
                        v["action"].as_str(),
                        Some("seek" | "play" | "pause" | "stop")
                    )
                })
            {
                continue;
            }
            let temp = support::Directory::new()?;
            let (address, worker, pin) = server(expected.clone(), has_media)?;
            let path=temp.config("cast.json",&json!({"name":"tcl_cast","host":"192.0.2.40","connect":address,"server_cert_sha256":pin,"timeout_seconds":2}))?;
            let mut manager = Manager::open(&path)?;
            assert!(
                !crate::entity::tools(crate::device::Selection {
                    cast: true,
                    ..Default::default()
                })
                .find(|t| t.name == "media_play")
                .expect("play tool")
                .hints
                .idempotent
            );
            if let Some(command) = &expected {
                let result = if command["action"] == "load" {
                    manager.control_device(0,Command::Play,&json!({"media":{"url":"https://media.example/test.mp4","content_type":"video/mp4"}}))?
                } else {
                    manager.control_device(0, Command::Playback, &json!({"command":command}))?
                };
                assert_eq!(result["verified"], true);
                if command["action"] == "load" {
                    assert_eq!(result["status"]["status"][0]["mediaSessionId"], 7);
                    assert_eq!(
                        result["status"]["status"][0]["media"]["contentId"],
                        "https://media.example/test.mp4"
                    );
                }
            } else {
                let result = manager.read_device(0)?;
                assert_eq!(result["receiver"]["volume"]["level"], 0.27);
                assert_eq!(result["media"].is_null(), !has_media);
                if has_media {
                    assert_eq!(result["media"]["playerState"], "PAUSED");
                }
            }
            worker.join().expect("Cast server")?;
        }
    }
    Ok(())
}
#[test]
fn unconfirmed_receiver_stop_prevents_launch_and_load() -> Result<()> {
    let temp = support::Directory::new()?;
    let (address, worker, pin) = server(Some(json!({"action":"load","refuse_stop":true})), true)?;
    let path = temp.config("cast.json", &json!({"name":"cast_tv","host":"192.0.2.40","connect":address,"server_cert_sha256":pin,"timeout_seconds":2}))?;
    let manager = Manager::open(&path)?;
    let failure = manager
        .control(
            Command::Play,
            &json!({"media":{"url":"https://media.example/test.mp4","content_type":"video/mp4"}}),
        )
        .expect_err("stop was not confirmed");
    assert!(failure.to_string().contains("no launch or load sent"));
    worker.join().expect("Cast server")?;
    Ok(())
}
#[test]
fn incremental_load_completion_requires_fresh_matching_metadata() -> Result<()> {
    let temp = support::Directory::new()?;
    let (address, worker, pin) =
        server(Some(json!({"action":"load","wrong_metadata":true})), true)?;
    let path = temp.config("cast.json", &json!({"name":"cast_tv","host":"192.0.2.40","connect":address,"server_cert_sha256":pin,"timeout_seconds":2}))?;
    let manager = Manager::open(&path)?;
    let failure = manager
        .control(
            Command::Play,
            &json!({"media":{"url":"https://media.example/test.mp4","content_type":"video/mp4"}}),
        )
        .expect_err("metadata changed");
    assert!(
        failure
            .to_string()
            .contains("session changed before verification")
    );
    worker.join().expect("Cast server")?;
    Ok(())
}
