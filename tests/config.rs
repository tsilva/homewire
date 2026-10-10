use homewire::{
    Result,
    config::{Config, Store},
};
use std::{fs, path::PathBuf};

#[path = "support/mod.rs"]
mod support;
struct Temp(support::Directory);
impl Temp {
    fn new() -> Result<Self> {
        Ok(Self(support::Directory::new()?))
    }
    fn config(&self) -> Result<PathBuf> {
        self.0.write("device.json", br#"{"device_id":"test-device","host":"192.0.2.1","local_key":"0123456789abcdef","protocol_version":"3.3"}"#)
    }
}

#[test]
fn state_survives_restart_and_lock_excludes_a_second_controller() -> Result<()> {
    let temp = Temp::new()?;
    let path = temp.config()?;
    let config = Config::load(&path)?;
    let store = Store::open(&path, &config.device_id)?;
    assert!(Store::open(&path, &config.device_id).is_err());
    let mut memory = store.read()?;
    memory.base_mode = Some("purify".into());
    memory
        .schedule_attempts
        .insert("night".into(), "2026-10-07".into());
    store.save(&memory)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(path.with_extension("state.json"))?
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    drop(store);
    let store = Store::open(&path, &config.device_id)?;
    let restored = store.read()?;
    assert_eq!(restored.base_mode.as_deref(), Some("purify"));
    assert_eq!(
        restored.schedule_attempts.get("night").map(String::as_str),
        Some("2026-10-07")
    );
    Ok(())
}

#[test]
fn wrong_device_or_corrupt_state_never_restores_a_sleep_assumption() -> Result<()> {
    let temp = Temp::new()?;
    let path = temp.config()?;
    let store = Store::open(&path, "test-device")?;
    for bytes in [
        br#"{"device_id":"other","base_mode":"purify"}"#.as_slice(),
        br#"{"device_id":"test-device","base_mode":"sleep"}"#.as_slice(),
        b"broken state".as_slice(),
    ] {
        fs::write(path.with_extension("state.json"), bytes)?;
        assert!(store.read().is_err());
    }
    Ok(())
}

#[test]
fn oversized_files_are_rejected() -> Result<()> {
    let temp = Temp::new()?;
    let path = temp.config()?;
    fs::write(&path, vec![b' '; 8193])?;
    assert!(Config::load(&path).is_err());
    let store = Store::open(&path, "test-device")?;
    fs::write(path.with_extension("state.json"), vec![b' '; 8193])?;
    assert!(store.read().is_err());
    Ok(())
}

#[test]
fn oversized_save_keeps_the_previous_durable_state() -> Result<()> {
    let temp = Temp::new()?;
    let path = temp.config()?;
    let store = Store::open(&path, "test-device")?;
    let mut memory = store.read()?;
    memory.base_mode = Some("purify".into());
    store.save(&memory)?;
    let previous = fs::read(path.with_extension("state.json"))?;
    memory
        .schedule_attempts
        .insert("night".into(), "x".repeat(8192));
    assert!(store.save(&memory).is_err());
    assert_eq!(fs::read(path.with_extension("state.json"))?, previous);
    assert_eq!(store.read()?.base_mode.as_deref(), Some("purify"));
    Ok(())
}

#[cfg(unix)]
#[test]
fn shared_config_permissions_are_rejected() -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let temp = Temp::new()?;
    let path = temp.config()?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644))?;
    assert!(Config::load(&path).is_err());
    Ok(())
}
