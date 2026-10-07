use homewire::{Result, klap::Session};
use serde_json::Value;

fn hex(value: &str) -> Vec<u8> {
    (0..value.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&value[i..i + 2], 16).expect("hex"))
        .collect()
}
fn vector() -> Value {
    serde_json::from_str(include_str!("fixtures/klap.json")).expect("fixture")
}
fn session(v: &Value) -> Result<Session> {
    Session::new(
        &hex(v["local"].as_str().expect("local"))
            .try_into()
            .expect("seed"),
        &hex(v["remote"].as_str().expect("remote"))
            .try_into()
            .expect("seed"),
        &hex(v["auth"].as_str().expect("auth"))
            .try_into()
            .expect("auth"),
    )
}
#[test]
fn matches_independent_aes_cbc_signature_and_signed_sequence_vector() -> Result<()> {
    let v = vector();
    let mut s = session(&v)?;
    let (frame, seq) = s.encrypt(v["request"].as_str().expect("request").as_bytes())?;
    assert_eq!(frame, hex(v["request_frame"].as_str().expect("frame")));
    assert_eq!(i64::from(seq), v["sequence"].as_i64().expect("seq"));
    assert_eq!(
        s.decrypt(&hex(v["response_frame"].as_str().expect("frame")))?,
        v["response"].as_str().expect("response").as_bytes()
    );
    Ok(())
}
#[test]
fn corrupt_truncated_oversized_and_replayed_responses_fail() -> Result<()> {
    let v = vector();
    let mut s = session(&v)?;
    s.encrypt(b"query")?;
    let frame = hex(v["response_frame"].as_str().expect("frame"));
    for n in 0..frame.len() {
        assert!(s.decrypt(&frame[..n]).is_err());
    }
    for i in [0, 31, 32, frame.len() - 1] {
        let mut bad = frame.clone();
        bad[i] ^= 1;
        assert!(s.decrypt(&bad).is_err());
    }
    assert!(s.decrypt(&vec![0; 16 * 1024 + 49]).is_err());
    // Re-sign a ciphertext with a deliberately invalid PKCS7 final byte.
    let mut bad_padding = frame.clone();
    let padding = 16 - v["response"].as_str().expect("response").len() % 16;
    let index = bad_padding.len() - 17;
    bad_padding[index] ^= padding as u8;
    let local = hex(v["local"].as_str().expect("local"));
    let remote = hex(v["remote"].as_str().expect("remote"));
    let auth = hex(v["auth"].as_str().expect("auth"));
    let signature = homewire::klap::hash(&[b"ldk", &local, &remote, &auth]);
    let seq = (v["sequence"].as_i64().expect("seq") as i32).to_be_bytes();
    let hash = homewire::klap::hash(&[&signature[..28], &seq, &bad_padding[32..]]);
    bad_padding[..32].copy_from_slice(&hash);
    assert!(s.decrypt(&bad_padding).is_err());
    assert!(s.encrypt(&vec![0; 16 * 1024 + 1]).is_err());
    s.encrypt(b"next")?;
    assert!(s.decrypt(&frame).is_err());
    Ok(())
}
#[test]
fn empty_exact_and_partial_blocks_round_trip() -> Result<()> {
    for data in [vec![], vec![0; 16], vec![1; 17], vec![2; 16384]] {
        let mut s = Session::new(&[1; 16], &[2; 16], &[3; 32])?;
        let (frame, _) = s.encrypt(&data)?;
        assert_eq!(s.decrypt(&frame)?, data);
    }
    Ok(())
}
