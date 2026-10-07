use homewire::{
    Result,
    ipp::{self, Attributes},
    printer::{Config, Endpoint, Memory, Monitor, State, Status},
};
use serde_json::json;
use std::{
    io::{BufRead, BufReader, Cursor, Read, Write},
    net::{IpAddr, Ipv4Addr, TcpListener},
    time::Duration,
};

fn endpoint() -> Endpoint {
    Endpoint {
        name: "brother".into(),
        host: IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1)),
        port: 631,
        path: "/ipp/print".into(),
        connect: None,
    }
}
fn status(state: i32, reasons: &[&str]) -> Status {
    let attributes = serde_json::from_value::<Attributes>(
        json!({"printer-state":[state],"printer-state-reasons":reasons}),
    )
    .expect("attributes");
    Status::from_attributes("brother", &attributes).expect("status")
}
fn attribute(bytes: &mut Vec<u8>, tag: u8, name: &str, value: &[u8]) {
    bytes.push(tag);
    bytes.extend_from_slice(&(name.len() as u16).to_be_bytes());
    bytes.extend_from_slice(name.as_bytes());
    bytes.extend_from_slice(&(value.len() as u16).to_be_bytes());
    bytes.extend_from_slice(value);
}
// Synthetic fixture encoded directly from RFC 8010, independently of request().
fn response() -> Vec<u8> {
    let mut bytes = vec![1, 1, 0, 1, 0, 0, 0, 1, 1];
    attribute(&mut bytes, 0x47, "attributes-charset", b"utf-8");
    bytes.push(5); // Unsupported attribute group must not overwrite live state.
    attribute(&mut bytes, 0x23, "printer-state", &5_i32.to_be_bytes());
    bytes.push(4);
    attribute(&mut bytes, 0x23, "printer-state", &3_i32.to_be_bytes());
    attribute(&mut bytes, 0x44, "printer-state-reasons", b"none");
    attribute(&mut bytes, 0x42, "marker-names", b"black");
    attribute(&mut bytes, 0x42, "", b"color");
    attribute(&mut bytes, 0x21, "marker-levels", &80_i32.to_be_bytes());
    attribute(&mut bytes, 0x21, "", &(-2_i32).to_be_bytes());
    bytes.push(3);
    bytes
}

#[test]
fn decodes_multi_values_and_preserves_unknown_supply_levels() -> Result<()> {
    let attrs = ipp::decode(&response())?;
    let value = Status::from_attributes("brother", &attrs)?;
    assert!(value.state == State::Idle);
    assert!(value.faults_known && value.faults.is_empty());
    assert_eq!(value.supplies[0].percent, Some(80));
    assert_eq!(value.supplies[1].percent, None);
    assert_eq!(value.supplies[1].reported_level, Some(-2));
    assert_eq!(value.supplies[1].name.as_deref(), Some("color"));
    Ok(())
}
#[test]
fn truncated_and_mismatched_ipp_messages_fail_closed() {
    let frame = response();
    for n in 0..frame.len() {
        assert!(ipp::decode(&frame[..n]).is_err());
    }
    let mut rejected = frame.clone();
    rejected[2] = 4;
    assert!(ipp::decode(&rejected).is_err());
    let mut wrong_id = frame;
    wrong_id[7] = 2;
    assert!(ipp::decode(&wrong_id).is_err());
    assert!(ipp::decode(&vec![0; ipp::MAX_BODY + 1]).is_err());
}
#[test]
fn http_supports_missing_type_chunking_and_interim_response() -> Result<()> {
    let body = response();
    let mut plain =
        format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", body.len()).into_bytes();
    plain.extend_from_slice(&body);
    assert_eq!(ipp::http_body(&mut Cursor::new(plain))?, body);
    let mut chunked = b"HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nContent-Type: application/ipp; charset=utf-8\r\n\r\n".to_vec();
    for chunk in body.chunks(7) {
        chunked.extend_from_slice(format!("{:x};ignored=yes\r\n", chunk.len()).as_bytes());
        chunked.extend_from_slice(chunk);
        chunked.extend_from_slice(b"\r\n");
    }
    chunked.extend_from_slice(b"0\r\nX-Example: trailer\r\n\r\n");
    assert_eq!(
        ipp::http_body(&mut BufReader::with_capacity(3, Cursor::new(chunked)))?,
        body
    );
    Ok(())
}
#[test]
fn http_rejects_redirects_errors_ambiguous_framing_and_oversized_input() {
    for bytes in [
        b"HTTP/1.1 302 Found\r\nLocation: http://example.invalid\r\n\r\n".as_slice(),
        b"HTTP/1.1 401 Unauthorized\r\n\r\n".as_slice(),
        b"HTTP/1.1 200 OK\r\nContent-Type: text/html\r\n\r\n".as_slice(),
        b"HTTP/1.1 200 OK\r\nContent-Length: 65537\r\n\r\n".as_slice(),
        b"HTTP/1.1 200 OK\r\nContent-Length: 1\r\nContent-Length: 2\r\n\r\n".as_slice(),
        b"HTTP/1.1 200 OK\r\nContent-Length: 1\r\nTransfer-Encoding: chunked\r\n\r\n".as_slice(),
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n10001\r\n".as_slice(),
        b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\nx".as_slice(),
    ] {
        assert!(ipp::http_body(&mut Cursor::new(bytes)).is_err());
    }
    let mut oversized = b"HTTP/1.1 200 OK\r\n\r\n".to_vec();
    oversized.resize(oversized.len() + ipp::MAX_BODY + 1, 0);
    assert!(ipp::http_body(&mut Cursor::new(oversized)).is_err());
    assert!(ipp::http_body(&mut Cursor::new(vec![b'x'; 8193])).is_err());
}
#[test]
fn network_query_is_only_get_printer_attributes_and_handles_fragmentation() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let mut target = endpoint();
    target.connect = Some(listener.local_addr()?);
    let worker = std::thread::spawn(move || -> Result<Vec<u8>> {
        let (mut socket, _) = listener.accept()?;
        socket.set_read_timeout(Some(Duration::from_secs(3)))?;
        let mut reader = BufReader::new(socket.try_clone()?);
        let mut first = String::new();
        reader.read_line(&mut first)?;
        assert_eq!(first, "POST /ipp/print HTTP/1.1\r\n");
        let mut length = 0;
        loop {
            let mut line = String::new();
            reader.read_line(&mut line)?;
            if line == "\r\n" {
                break;
            }
            if let Some(value) = line.strip_prefix("Content-Length: ") {
                length = value.trim().parse::<usize>()?;
            }
        }
        assert!(length > 0 && length < 4096);
        let mut request = vec![0; length];
        reader.read_exact(&mut request)?;
        let body = response();
        socket.write_all(
            format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", body.len()).as_bytes(),
        )?;
        for chunk in body.chunks(3) {
            socket.write_all(chunk)?;
        }
        Ok(request)
    });
    let value = target.read(Duration::from_secs(3))?;
    assert!(value.state == State::Idle);
    let sent = worker.join().expect("worker")?;
    assert_eq!(&sent[..8], &[1, 1, 0, 11, 0, 0, 0, 1]);
    assert!(
        sent.windows(b"ipp://192.0.2.1:631/ipp/print".len())
            .any(|w| w == b"ipp://192.0.2.1:631/ipp/print")
    );
    assert_eq!(sent.last(), Some(&3));
    Ok(())
}
#[test]
fn fault_classification_matches_ha_and_does_not_guess_from_supply_levels() {
    for reason in [
        "media-empty-warning",
        "toner-low-report",
        "vendor-fault-error",
    ] {
        let value = status(3, &[reason]);
        assert!(value.faults_known);
        assert_eq!(value.faults, vec![reason]);
    }
    for reason in [
        "none",
        "sleep",
        "power-save",
        "offline",
        "connecting-to-device",
    ] {
        assert!(status(3, &[reason]).faults.is_empty());
    }
    assert!(!status(5, &["paused"]).faults_known);
    assert!(!status(3, &["vendor-unknown"]).faults_known);
    assert!(!status(99, &["media-jam"]).faults_known);
    assert!(!status(3, &[]).faults_known);
    assert_eq!(
        status(3, &[" TONER-LOW-WARNING ", "toner-low-warning"]).faults,
        vec!["toner-low-warning"]
    );
}
#[test]
fn faults_debounce_deduplicate_and_survive_restart() -> Result<()> {
    let mut monitor = Monitor::default();
    let target = endpoint();
    let fault = status(3, &["media-jam-error"]);
    assert!(monitor.observe(&target, Some(&fault), 0).is_none());
    assert!(monitor.observe(&target, Some(&fault), 2).is_none());
    let event = monitor
        .observe(&target, Some(&fault), 3)
        .expect("fault event");
    assert_eq!(event.event, "fault");
    let restored: Memory = serde_json::from_slice(&serde_json::to_vec(&monitor.memory)?)?;
    let mut restarted = Monitor::default();
    restarted.memory = restored;
    assert!(restarted.observe(&target, Some(&fault), 100).is_none());
    assert!(restarted.observe(&target, None, 101).is_none());
    assert_eq!(
        restarted.memory.devices["brother"].faults,
        vec!["media-jam-error"]
    );
    Ok(())
}
#[test]
fn unknown_readings_interrupt_recovery_without_clearing_saved_faults() {
    let mut monitor = Monitor::default();
    let target = endpoint();
    let fault = status(3, &["cover-open"]);
    let clear = status(3, &["none"]);
    monitor.observe(&target, Some(&fault), 0);
    assert!(monitor.observe(&target, Some(&fault), 3).is_some());
    assert!(monitor.observe(&target, Some(&clear), 10).is_none());
    assert!(monitor.observe(&target, None, 39).is_none());
    assert!(monitor.observe(&target, Some(&clear), 100).is_none());
    assert!(monitor.observe(&target, Some(&clear), 129).is_none());
    let recovered = monitor
        .observe(&target, Some(&clear), 130)
        .expect("recovery");
    assert_eq!(recovered.event, "recovered");
    assert_eq!(recovered.previous_faults, vec!["cover-open"]);
    assert!(monitor.observe(&target, Some(&clear), 200).is_none());
}
#[test]
fn config_rejects_duplicate_names_header_injection_and_unbounded_counts() {
    let valid = Config {
        printers: vec![endpoint()],
        timeout_seconds: 8,
        poll_seconds: 30,
    };
    assert!(valid.validate().is_ok());
    let duplicate = Config {
        printers: vec![endpoint(), endpoint()],
        ..valid
    };
    assert!(duplicate.validate().is_err());
    for path in [
        "/ipp/print\r\nHost: other",
        "not/a/path",
        "/ipp/print?override=yes",
    ] {
        let mut target = endpoint();
        target.path = path.into();
        let config = Config {
            printers: vec![target],
            timeout_seconds: 8,
            poll_seconds: 30,
        };
        assert!(config.validate().is_err());
    }
}
#[test]
fn supplies_do_not_create_faults_without_reported_reasons() -> Result<()> {
    let attrs: Attributes = serde_json::from_value(
        json!({"printer-state":[3],"printer-state-reasons":["none"],"marker-levels":[0]}),
    )?;
    assert!(Status::from_attributes("hp", &attrs)?.faults.is_empty());
    Ok(())
}

#[test]
fn monitoring_persists_before_emitting_and_restart_does_not_repeat_a_fault() -> Result<()> {
    use homewire::printer::Manager;
    use std::fs;
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let address = listener.local_addr()?;
    let temp = std::env::temp_dir().join(format!(
        "homewire-printer-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos()
    ));
    fs::create_dir(&temp)?;
    let config_path = temp.join("printers.json");
    fs::write(
        &config_path,
        serde_json::to_vec(
            &json!({"printers":[{"name":"brother","host":"192.0.2.1","connect":address.to_string()}],"timeout_seconds":3,"poll_seconds":5}),
        )?,
    )?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&config_path, fs::Permissions::from_mode(0o600))?;
    }
    let worker = std::thread::spawn(move || -> Result<()> {
        for _ in 0..3 {
            let (mut socket, _) = listener.accept()?;
            socket.set_read_timeout(Some(Duration::from_secs(3)))?;
            let mut reader = BufReader::new(socket.try_clone()?);
            let mut size = 0;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line)?;
                if line == "\r\n" {
                    break;
                }
                if let Some(value) = line.strip_prefix("Content-Length: ") {
                    size = value.trim().parse::<usize>()?;
                }
            }
            let mut request = vec![0; size];
            reader.read_exact(&mut request)?;
            assert_eq!(&request[..4], &[1, 1, 0, 11]);
            let mut body = vec![1, 1, 0, 0, 0, 0, 0, 1, 4];
            attribute(&mut body, 0x23, "printer-state", &3_i32.to_be_bytes());
            attribute(&mut body, 0x44, "printer-state-reasons", b"cover-open");
            body.push(3);
            socket.write_all(
                format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", body.len()).as_bytes(),
            )?;
            socket.write_all(&body)?;
        }
        Ok(())
    });
    let mut manager = Manager::open(&config_path)?;
    assert!(Manager::open(&config_path).is_err());
    assert!(manager.check()?.events.is_empty());
    std::thread::sleep(Duration::from_secs(3));
    let check = manager.check()?;
    assert_eq!(check.events.len(), 1);
    let persisted: Memory =
        serde_json::from_slice(&fs::read(config_path.with_extension("state.json"))?)?;
    assert_eq!(persisted.devices["brother"].faults, vec!["cover-open"]);
    drop(manager);
    let mut restarted = Manager::open(&config_path)?;
    let check = restarted.check()?;
    assert!(check.events.is_empty());
    assert_eq!(check.active_faults["brother"], vec!["cover-open"]);
    drop(restarted);
    worker.join().expect("worker")?;
    fs::remove_dir_all(temp)?;
    Ok(())
}
