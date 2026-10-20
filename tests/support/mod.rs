type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
use serde_json::Value;
use std::{fs, path::PathBuf};
#[allow(dead_code)]
pub struct Directory(pub PathBuf);
#[allow(dead_code)]
impl Directory {
    pub fn new() -> Result<Self> {
        // This helper is included by multiple test modules, each with its own
        // scope. Keep directories unique and short enough for macOS IPC paths.
        let mut nonce = [0; 8];
        getrandom::getrandom(&mut nonce)?;
        let path = std::env::temp_dir().join(format!("hw-{:016x}", u64::from_ne_bytes(nonce)));
        fs::create_dir(&path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700))?;
        }
        Ok(Self(path))
    }
    pub fn write(&self, name: &str, bytes: &[u8]) -> Result<PathBuf> {
        let path = self.0.join(name);
        Self::write_private(&path, bytes)?;
        Ok(path)
    }
    pub fn write_private(path: &std::path::Path, bytes: &[u8]) -> Result<()> {
        fs::write(path, bytes)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
        }
        Ok(())
    }
    pub fn config(&self, name: &str, value: &Value) -> Result<PathBuf> {
        self.write(name, &serde_json::to_vec(value)?)
    }
    pub fn certs(&self) -> Result<(PathBuf, PathBuf)> {
        Ok((
            self.write(
                "cert.pem",
                include_bytes!("../fixtures/media/test-cert.pem"),
            )?,
            self.write("key.pem", include_bytes!("../fixtures/media/test-key.pem"))?,
        ))
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
