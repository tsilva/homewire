//! Private Unix-socket transport. One request per connection; no control retries.
use crate::{Result, error, mcp};
use serde_json::Value;
use std::{
    fs::{self, File, OpenOptions},
    io::{self, BufRead, BufReader, Read, Write},
    os::unix::{
        fs::{FileTypeExt, OpenOptionsExt, PermissionsExt},
        net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver},
    },
    thread,
    time::Duration,
};
const REPLY_LIMIT: usize = 1024 * 1024;
const FRAME_TIMEOUT: Duration = Duration::from_secs(5);
const CALL_TIMEOUT: Duration = Duration::from_secs(300);

pub struct Server {
    pub incoming: Receiver<UnixStream>,
    path: PathBuf,
    _lock: File,
    stopped: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}
impl Server {
    /// Parent must already be a private, real directory. An OS lock protects
    /// stale socket replacement across crashes and competing daemon startups.
    pub fn bind(path: &Path) -> Result<Self> {
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .ok_or_else(|| error("Socket needs a private parent directory"))?;
        let metadata = fs::symlink_metadata(parent)?;
        if !metadata.is_dir() || metadata.permissions().mode() & 0o077 != 0 {
            return Err(error(
                "Socket parent must be a private directory (chmod 700)",
            ));
        }
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(path.with_extension("socket.lock"))?;
        crate::config::lock_file(&lock, "Another daemon owns this socket")?;
        if let Ok(metadata) = fs::symlink_metadata(path) {
            if !metadata.file_type().is_socket() {
                return Err(error("Refusing to replace a non-socket path"));
            }
            match UnixStream::connect(path) {
                Ok(_) => return Err(error("Socket is already listening")),
                Err(e) if e.kind() == io::ErrorKind::ConnectionRefused => fs::remove_file(path)?,
                Err(e) => return Err(e.into()),
            }
        }
        let listener = UnixListener::bind(path)?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
        std::fs::write(path.with_extension("pid"), std::process::id().to_string())?;
        fs::set_permissions(
            path.with_extension("pid"),
            fs::Permissions::from_mode(0o600),
        )?;
        let (sender, incoming) = mpsc::sync_channel(1);
        let stopped = Arc::new(AtomicBool::new(false));
        let stop = Arc::clone(&stopped);
        let worker = thread::Builder::new()
            .name("ipc-accept".into())
            .stack_size(128 * 1024)
            .spawn(move || {
                for connection in listener.incoming() {
                    if stop.load(Ordering::Acquire) {
                        break;
                    }
                    match connection {
                        Ok(stream) => {
                            if sender.send(stream).is_err() {
                                break;
                            }
                        }
                        Err(e) => {
                            eprintln!("IPC accept failed: {e}");
                            break;
                        }
                    }
                }
            })?;
        Ok(Self {
            incoming,
            path: path.into(),
            _lock: lock,
            stopped,
            worker: Some(worker),
        })
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Release);
        // Drop the queue before joining so a blocked send can finish. Wake an
        // accept blocked without clients; the stop flag suppresses this request.
        let (_, empty) = mpsc::sync_channel(1);
        drop(std::mem::replace(&mut self.incoming, empty));
        let _ = UnixStream::connect(&self.path);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        let _ = fs::remove_file(&self.path);
        let _ = fs::remove_file(self.path.with_extension("pid"));
    }
}
/// The caller owns serialization and scheduling; disconnected/malformed clients
/// are isolated here and never terminate the device-owning daemon.
pub fn exchange(
    stream: &mut UnixStream,
    handle: impl FnOnce(Value) -> Option<Value>,
) -> Result<()> {
    stream.set_read_timeout(Some(FRAME_TIMEOUT))?;
    stream.set_write_timeout(Some(FRAME_TIMEOUT))?;
    let line = mcp::read_message(&mut BufReader::with_capacity(1024, &mut *stream))?
        .ok_or_else(|| error("Empty IPC request"))?;
    let reply = match serde_json::from_str(&line) {
        Ok(request) => handle(request),
        Err(_) => Some(mcp::rpc_error(Value::Null, -32700, "Invalid JSON")),
    };
    let bytes = serde_json::to_vec(&reply)?;
    if bytes.len() >= REPLY_LIMIT {
        return Err(error("IPC reply exceeds 1 MiB"));
    }
    stream.write_all(&bytes)?;
    stream.write_all(b"\n")?;
    stream.flush()?;
    Ok(())
}
/// Connect exactly once. A timeout/disconnect after a write is an uncertain
/// outcome and must never trigger an automatic retry.
pub fn request(path: &Path, line: &str) -> Result<Option<Value>> {
    if line.len() > mcp::MAX_MESSAGE {
        return Err(error("IPC request exceeds 64 KiB"));
    }
    let mut stream = UnixStream::connect(path)?;
    stream.set_read_timeout(Some(CALL_TIMEOUT))?;
    stream.set_write_timeout(Some(FRAME_TIMEOUT))?;
    stream.write_all(line.as_bytes())?;
    if !line.ends_with('\n') {
        stream.write_all(b"\n")?;
    }
    let mut bytes = Vec::new();
    let n = BufReader::with_capacity(1024, stream)
        .take((REPLY_LIMIT + 1) as u64)
        .read_until(b'\n', &mut bytes)?;
    if n == 0 || n > REPLY_LIMIT || bytes.last() != Some(&b'\n') {
        return Err(error(
            "Incomplete or oversized IPC reply; outcome may be uncertain",
        ));
    }
    Ok(serde_json::from_slice(&bytes)?)
}
pub fn bridge(path: &Path) -> Result<()> {
    let mut input = BufReader::with_capacity(1024, io::stdin());
    let mut output = io::stdout().lock();
    while let Some(line) = mcp::read_message(&mut input)? {
        if let Some(reply) = request(path, &line)? {
            mcp::write_message(&mut output, &reply)?;
        }
    }
    Ok(())
}
