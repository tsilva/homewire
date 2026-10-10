use chrono::{DateTime, Utc};
use homewire::{
    Result,
    awair::{Config, Status},
};
use serde_json::{Value, json};
use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
    net::TcpListener,
    path::PathBuf,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

fn now() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-10-07T18:15:10.250Z")
        .expect("date")
        .with_timezone(&Utc)
}
fn reading() -> Value {
    json!({"timestamp":"2026-10-07T19:15:08.125+01:00","score":91,
        "temp":24.75,"humid":51.2,"co2":620,"voc":180.5,"pm25":3.25,
        "co2_est":400,"pm10_est":9,"new_sensor":123})
}
fn decode(value: &Value) -> Result<Status> {
    Status::decode(&serde_json::to_vec(value)?, now(), 120)
}
fn config() -> Config {
    serde_json::from_value(json!({"host":"192.0.2.12"})).expect("config")
}
#[path = "support/mod.rs"]
mod support;
struct PrivateFile(PathBuf, #[allow(dead_code)] support::Directory);
impl PrivateFile {
    fn new(value: &Value) -> Result<Self> {
        let directory = support::Directory::new()?;
        Ok(Self(directory.config("awair.json", value)?, directory))
    }
}

#[test]
fn readings_preserve_units_fractional_values_and_actual_sensors() -> Result<()> {
    let s = decode(&reading())?;
    assert_eq!(s.measured_at, "2026-10-07T18:15:08.125+00:00");
    assert_eq!(s.observed_at, "2026-10-07T18:15:10.250+00:00");
    assert_eq!(s.age_seconds, 2.125);
    assert_eq!(s.awair_score, 91);
    assert_eq!(s.temperature_c, 24.75);
    assert_eq!(s.humidity_percent, 51.2);
    assert_eq!(s.co2_ppm, 620.0);
    assert_eq!(s.tvoc_ppb, 180.5);
    assert_eq!(s.pm25_ug_m3, 3.25);
    Ok(())
}
#[test]
fn missing_null_or_wrong_type_measurements_never_default_to_zero() {
    for field in ["timestamp", "score", "temp", "humid", "co2", "voc", "pm25"] {
        let mut v = reading();
        v.as_object_mut().expect("object").remove(field);
        assert!(decode(&v).is_err(), "missing {field}");
        for bad in [Value::Null, json!(true), json!([])] {
            let mut v = reading();
            v[field] = bad;
            assert!(decode(&v).is_err(), "bad {field}");
        }
    }
}
#[test]
fn invalid_measurement_ranges_and_oversized_bodies_are_rejected() {
    for (field, value) in [
        ("score", json!(101)),
        ("score", json!(-1)),
        ("humid", json!(100.1)),
        ("humid", json!(-0.1)),
        ("co2", json!(-1)),
        ("voc", json!(-1)),
        ("pm25", json!(-1)),
    ] {
        let mut v = reading();
        v[field] = value;
        assert!(decode(&v).is_err(), "{field}");
    }
    assert!(Status::decode(&vec![b' '; 65537], now(), 120).is_err());
    let overflow = reading().to_string().replace("24.75", "1e999");
    assert!(Status::decode(overflow.as_bytes(), now(), 120).is_err());
    let mut zero = reading();
    for field in ["score", "humid", "co2", "voc", "pm25"] {
        zero[field] = json!(0);
    }
    assert!(decode(&zero).is_ok());
}
#[test]
fn freshness_checks_exact_boundaries_and_reveals_small_clock_skew() -> Result<()> {
    let mut v = reading();
    for (offset, valid) in [
        (-120_000, true),
        (-120_001, false),
        (30_000, true),
        (30_001, false),
    ] {
        v["timestamp"] = json!((now() + chrono::Duration::milliseconds(offset)).to_rfc3339());
        assert_eq!(decode(&v).is_ok(), valid, "offset {offset}");
    }
    v["timestamp"] = json!((now() + chrono::Duration::seconds(5)).to_rfc3339());
    assert_eq!(decode(&v)?.age_seconds, -5.0);
    for timestamp in ["bad", "2026-10-07T18:15:08", ""] {
        v["timestamp"] = json!(timestamp);
        assert!(decode(&v).is_err());
    }
    assert!(Status::decode(&serde_json::to_vec(&reading())?, now(), 3601).is_err());
    Ok(())
}
#[test]
fn configuration_enforces_private_files_and_bounded_options() -> Result<()> {
    let file = PrivateFile::new(&json!({"host":"192.0.2.12"}))?;
    let cfg = Config::load(&file.0)?;
    assert_eq!(cfg.port, 80);
    assert_eq!(cfg.timeout_seconds, 8);
    assert_eq!(cfg.max_age_seconds, 120);
    for patch in [
        json!({"port":0}),
        json!({"connect":"127.0.0.1:0"}),
        json!({"timeout_seconds":0}),
        json!({"timeout_seconds":31}),
        json!({"max_age_seconds":9}),
        json!({"max_age_seconds":3601}),
        json!({"token":"unexpected"}),
        json!({"host":"awair.local"}),
    ] {
        let mut v = json!({"host":"192.0.2.12"});
        v.as_object_mut()
            .expect("object")
            .extend(patch.as_object().expect("object").clone());
        fs::write(&file.0, serde_json::to_vec(&v)?)?;
        assert!(Config::load(&file.0).is_err());
    }
    fs::write(&file.0, vec![b' '; 8193])?;
    assert!(Config::load(&file.0).is_err());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::write(&file.0, b"{\"host\":\"192.0.2.12\"}")?;
        fs::set_permissions(&file.0, fs::Permissions::from_mode(0o644))?;
        assert!(Config::load(&file.0).is_err());
    }
    Ok(())
}

// Each fake device checks that only the fixed read endpoint is requested.
fn mock(response: Vec<u8>) -> Result<(Config, thread::JoinHandle<Result<()>>)> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let mut cfg = config();
    cfg.connect = Some(listener.local_addr()?);
    let worker = thread::spawn(move || -> Result<()> {
        let (mut socket, _) = listener.accept()?;
        socket.set_read_timeout(Some(Duration::from_secs(3)))?;
        let mut reader = BufReader::new(socket.try_clone()?);
        let mut request = String::new();
        loop {
            let mut line = String::new();
            reader.by_ref().take(8193).read_line(&mut line)?;
            assert!(request.len() + line.len() <= 8192 && !line.is_empty());
            request.push_str(&line);
            if line == "\r\n" {
                break;
            }
        }
        assert!(request.starts_with("GET /air-data/latest HTTP/1.1\r\n"));
        assert!(request.contains("Host: 192.0.2.12:80\r\n"));
        assert!(request.contains("Accept: application/json\r\n"));
        assert!(!request.contains("Authorization:") && !request.contains("current_time"));
        // Fragment the response to exercise reading across packet boundaries.
        for chunk in response.chunks(5) {
            socket.write_all(chunk)?;
        }
        Ok(())
    });
    Ok((cfg, worker))
}
fn chunked_reading() -> Vec<u8> {
    let mut value = reading();
    value["timestamp"] = json!(Utc::now().to_rfc3339());
    let body = value.to_string();
    format!("HTTP/1.1 200 OK\r\nContent-Type: application/json; charset=utf-8\r\nTransfer-Encoding: chunked\r\n\r\n{:x}\r\n{body}\r\n0\r\n\r\n",body.len()).into_bytes()
}
#[test]
fn network_get_handles_chunked_json_with_real_host_and_tunnel_override() -> Result<()> {
    let (cfg, worker) = mock(chunked_reading())?;
    assert_eq!(cfg.read()?.co2_ppm, 620.0);
    worker.join().expect("worker")?;
    Ok(())
}
#[test]
fn network_failures_do_not_return_a_measurement() -> Result<()> {
    for response in [
        b"HTTP/1.1 302 Found\r\nLocation: http://example.invalid\r\nContent-Length: 0\r\n\r\n"
            .as_slice(),
        b"HTTP/1.1 503 Unavailable\r\nContent-Length: 0\r\n\r\n".as_slice(),
        b"HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: 0\r\n\r\n".as_slice(),
        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}".as_slice(),
        b"HTTP/1.1 200 OK\r\nContent-Length: 20\r\n\r\nx".as_slice(),
    ] {
        let (cfg, worker) = mock(response.to_vec())?;
        assert!(cfg.read().is_err());
        worker.join().expect("worker")?;
    }
    Ok(())
}
#[test]
fn total_request_deadline_stops_a_slow_response() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let mut cfg = config();
    cfg.connect = Some(listener.local_addr()?);
    cfg.timeout_seconds = 1;
    let worker = thread::spawn(move || -> Result<()> {
        let (mut socket, _) = listener.accept()?;
        socket.set_read_timeout(Some(Duration::from_secs(2)))?;
        let mut reader = BufReader::new(socket.try_clone()?);
        loop {
            let mut line = String::new();
            reader.read_line(&mut line)?;
            if line == "\r\n" {
                break;
            }
        }
        socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\n")?;
        for _ in 0..4 {
            thread::sleep(Duration::from_millis(400));
            if socket.write_all(b" ").is_err() {
                break;
            }
        }
        Ok(())
    });
    let start = Instant::now();
    assert!(cfg.read().is_err());
    assert!(start.elapsed() < Duration::from_secs(2));
    worker.join().expect("worker")?;
    Ok(())
}
#[test]
fn awair_only_cli_and_mcp_work_without_other_device_configuration() -> Result<()> {
    for serve in [false, true] {
        let (cfg, worker) = mock(chunked_reading())?;
        let file = PrivateFile::new(&json!({"host":"192.0.2.12","connect":cfg.connect}))?;
        let mut child = Command::new(env!("CARGO_BIN_EXE_homewire"))
            .args([
                "--awair",
                file.0.to_str().expect("path"),
                if serve { "serve" } else { "awair-status" },
            ])
            .args(if serve {
                vec!["--legacy-tools"]
            } else {
                vec![]
            })
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        if serve {
            let mut stdin = child.stdin.take().expect("stdin");
            for request in [
                json!({"jsonrpc":"2.0","id":1,"method":"initialize"}),
                json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
                json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
                json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"awair_status"}}),
            ] {
                writeln!(stdin, "{request}")?;
            }
        }
        let output = child.wait_with_output()?;
        assert!(
            output.status.success(),
            "{:?}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.stderr.is_empty());
        let values: Vec<Value> = String::from_utf8(output.stdout)?
            .lines()
            .map(serde_json::from_str)
            .collect::<std::result::Result<_, _>>()?;
        if serve {
            assert_eq!(values.len(), 3);
            assert_eq!(
                values[1]["result"]["tools"]
                    .as_array()
                    .expect("tools")
                    .len(),
                1
            );
            assert_eq!(values[2]["result"]["structuredContent"]["co2_ppm"], 620.0);
        } else {
            assert_eq!(values.len(), 1);
            assert_eq!(values[0]["co2_ppm"], 620.0);
        }
        worker.join().expect("worker")?;
    }
    Ok(())
}
