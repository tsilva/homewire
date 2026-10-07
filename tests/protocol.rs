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
}
