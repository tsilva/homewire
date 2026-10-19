use homewire::{
    Result,
    tuya::{decode_response, decrypt, encode_request, encrypt},
};
use serde_json::json;
const KEY: &[u8; 16] = b"0123456789abcdef";
// Generated independently using TinyTuya 1.20.0 with synthetic key/data.
const GOLDEN: &str = "000055aa00000007000000080000005b00000000332e33000000000000000000000000c27ee03f8be481e63320de7dc6eb429675b719495a3d2ed013cad6f27d16fa0e34ceb82a8c4b76c4e649309245ada65a6224a8b0cec2f624615d9889c2e83bbfe4606c430000aa55";
fn golden() -> Vec<u8> {
    (0..GOLDEN.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&GOLDEN[i..i + 2], 16).expect("fixture hex"))
        .collect()
}
#[test]
fn decodes_independent_tinytuya_status_fixture() -> Result<()> {
    assert_eq!(
        decode_response(&golden(), KEY)?,
        json!({"dps":{"1":true,"2":55,"4":"manual","16":61,"19":0}})
    );
    Ok(())
}
#[test]
fn corruption_is_rejected_before_decryption() {
    let mut frame = golden();
    frame[42] ^= 1;
    assert!(decode_response(&frame, KEY).is_err());
}
#[test]
fn device_rejection_reports_reachability_and_key_recovery_without_payload() {
    let mut frame = golden();
    frame[16..20].copy_from_slice(&1_u32.to_be_bytes());
    let end = frame.len();
    let crc = crc32fast::hash(&frame[..end - 8]);
    frame[end - 8..end - 4].copy_from_slice(&crc.to_be_bytes());
    let message = decode_response(&frame, KEY)
        .expect_err("rejected")
        .to_string();
    assert!(message.contains("code 1"));
    assert!(message.contains("device is reachable"));
    assert!(message.contains("local key"));
    assert!(message.contains("re-pairing"));
    assert!(!message.contains(std::str::from_utf8(KEY).expect("key")));
    assert!(!message.contains("dps"));

    frame[end - 8] ^= 1;
    assert!(
        decode_response(&frame, KEY)
            .expect_err("checksum")
            .to_string()
            .contains("checksum")
    );
}
#[test]
fn wrong_key_is_rejected() {
    assert!(decode_response(&golden(), b"fedcba9876543210").is_err());
}
#[test]
fn truncated_response_is_rejected() {
    let frame = golden();
    for len in [0, 3, 16, 27, frame.len() - 1] {
        assert!(decode_response(&frame[..len], KEY).is_err());
    }
}
#[test]
fn pkcs7_handles_empty_exact_and_partial_blocks() -> Result<()> {
    for payload in [vec![], vec![b'x'; 16], vec![b'x'; 31]] {
        assert_eq!(decrypt(&encrypt(&payload, KEY), KEY)?, payload);
    }
    Ok(())
}
#[test]
fn request_has_command_specific_header_and_no_return_code() -> Result<()> {
    let query = encode_request(1, 10, b"{}", KEY)?;
    assert_eq!(query.len(), 40);
    assert_eq!(&query[16..32], encrypt(b"{}", KEY));
    let control = encode_request(2, 7, b"{}", KEY)?;
    assert_eq!(&control[16..19], b"3.3");
    assert_eq!(control.len(), 55);
    Ok(())
}
#[test]
fn requests_are_bounded() {
    assert!(encode_request(1, 7, &vec![0; 16 * 1024], KEY).is_err());
    assert!(decode_response(&vec![0; 16 * 1024 + 17], KEY).is_err());
}

#[test]
fn client_merges_owned_direct_and_nested_data_points_across_status_frames() -> Result<()> {
    use homewire::{config::Config, tuya::Client};
    use std::{
        io::{Read, Write},
        net::TcpListener,
        time::Duration,
    };
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let address = listener.local_addr()?;
    let worker = std::thread::spawn(move || -> Result<()> {
        let (mut stream, _) = listener.accept()?;
        stream.set_read_timeout(Some(Duration::from_secs(2)))?;
        let mut header = [0; 16];
        stream.read_exact(&mut header)?;
        assert_eq!(u32::from_be_bytes(header[8..12].try_into()?), 10);
        let mut query = vec![0; u32::from_be_bytes(header[12..].try_into()?) as usize];
        stream.read_exact(&mut query)?;
        for payload in [
            json!({"dps":{"1":true,"2":55}}),
            json!({"data":{"dps":{"4":"manual","19":0}}}),
        ] {
            let ciphertext = encrypt(&serde_json::to_vec(&payload)?, KEY);
            let mut frame = Vec::new();
            for value in [0x55aa_u32, 1, 8, ciphertext.len() as u32 + 12, 0] {
                frame.extend_from_slice(&value.to_be_bytes());
            }
            frame.extend_from_slice(&ciphertext);
            frame.extend_from_slice(&crc32fast::hash(&frame).to_be_bytes());
            frame.extend_from_slice(&0xaa55_u32.to_be_bytes());
            stream.write_all(&frame)?;
        }
        Ok(())
    });
    let config: Config = serde_json::from_value(json!({
        "device_id":"synthetic", "host":"192.0.2.1", "local_key":"0123456789abcdef",
        "protocol_version":"3.3", "timeout_seconds":2,
    }))?;
    let mut client = Client::new(&config, Some(address))?;
    assert_eq!(
        client.status()?,
        serde_json::from_value(json!({"1":true,"2":55,"4":"manual","19":0}))?
    );
    worker.join().expect("worker")?;
    Ok(())
}
