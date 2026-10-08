type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
use serde_json::Value;
use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicUsize, Ordering},
};
pub struct Directory(pub PathBuf);
impl Directory {
    pub fn new() -> Result<Self> {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "homewire-media-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path)?;
        Ok(Self(path))
    }
    pub fn write(&self, name: &str, bytes: &[u8]) -> Result<PathBuf> {
        let path = self.0.join(name);
        fs::write(&path, bytes)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
        }
        Ok(path)
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
