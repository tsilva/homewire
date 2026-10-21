//! Private Unix-socket transport. One request per connection; no control retries.
use crate::{Result, error, mcp};
use serde_json::Value;
use std::{
    fs::{self, OpenOptions},
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
    time::{Duration, Instant},
};
const REPLY_LIMIT: usize = 1024 * 1024;
const FRAME_TIMEOUT: Duration = Duration::from_secs(5);
const CALL_TIMEOUT: Duration = Duration::from_secs(300);
/// Room for both API workers and ordinary bridge clients, without an unbounded backlog.
pub const DEVICE_QUEUE_CAPACITY: usize = 4;

pub struct Server {
    pub incoming: Receiver<Pending>,
    path: PathBuf,
    _lock: crate::config::FileLock,
    stopped: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}

/// A bounded, already framed device request. Only the device owner executes it.
pub struct Pending {
    stream: UnixStream,
    request: Value,
}
impl Pending {
    pub fn exchange(mut self, handle: impl FnOnce(Value) -> Option<Value>) -> Result<()> {
        write_reply(&mut self.stream, handle(self.request))
    }
    fn reject_busy(mut self) -> Result<()> {
        let reply = self.request.get("id").map(|id| {
            mcp::rpc_error(
                id.clone(),
                -32000,
                "Device owner is busy; request was not queued or executed",
            )
        });
        write_reply(&mut self.stream, reply)
    }
}

impl Server {
    /// Parent must already be a private, real directory. An OS lock protects
    /// stale socket replacement across crashes and competing daemon startups.
    pub fn bind(path: &Path) -> Result<Self> {
        Self::bind_with_protocol(path, |request| {
            mcp::handle_selected(
                request,
                crate::device::Selection::default(),
                mcp::Options::default(),
                |_, _| Err(error("Device calls belong to the owner")),
            )
        })
    }
    /// Protocol-only replies use the existing accept thread. Device calls stay
    /// on the owner, and a full queue never blocks protocol traffic.
    pub fn bind_with_protocol(
        path: &Path,
        protocol: impl Fn(Value) -> Option<Value> + Send + 'static,
    ) -> Result<Self> {
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
        let lock = crate::config::lock_file(lock, "Another daemon owns this socket")?;
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
        let (sender, incoming) = mpsc::sync_channel(DEVICE_QUEUE_CAPACITY);
        let stopped = Arc::new(AtomicBool::new(false));
        let stop = Arc::clone(&stopped);
        let worker = thread::Builder::new()
            .name("ipc-accept".into())
            .stack_size(512 * 1024)
            .spawn(move || {
                for connection in listener.incoming() {
                    if stop.load(Ordering::Acquire) {
                        break;
                    }
                    match connection {
                        Ok(mut stream) => {
                            let request = match read_request(&mut stream) {
                                Ok(Some(request)) => request,
                                Ok(None) => {
                                    let _ = write_reply(
                                        &mut stream,
                                        Some(mcp::rpc_error(Value::Null, -32700, "Invalid JSON")),
                                    );
                                    continue;
                                }
                                Err(e) => {
                                    eprintln!("IPC frame rejected: {e}");
                                    continue;
                                }
                            };
                            if let Some(reply) = mcp::invalid_request(&request) {
                                let _ = write_reply(&mut stream, Some(reply));
                            } else if crate::api::protocol_only(&request)
                                || !matches!(
                                    request["method"].as_str(),
                                    Some("tools/call" | "homewire/api")
                                )
                            {
                                let _ = write_reply(&mut stream, protocol(request));
                            } else {
                                match sender.try_send(Pending { stream, request }) {
                                    Ok(()) => (),
                                    Err(mpsc::TrySendError::Full(pending)) => {
                                        if let Err(e) = pending.reject_busy() {
                                            eprintln!("IPC busy reply failed: {e}");
                                        }
                                    }
                                    Err(mpsc::TrySendError::Disconnected(_)) => break,
                                }
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
// Use one absolute deadline per frame/reply, including partial reads/writes.
// A trickling client must not monopolize the protocol listener indefinitely.
struct Framed<'a> {
    stream: &'a mut UnixStream,
    deadline: Instant,
}
impl<'a> Framed<'a> {
    fn new(stream: &'a mut UnixStream) -> Self {
        Self {
            stream,
            deadline: Instant::now() + FRAME_TIMEOUT,
        }
    }
    fn remaining(&self) -> io::Result<Duration> {
        self.deadline
            .checked_duration_since(Instant::now())
            // Socket timeouts use microseconds. Avoid rounding a fractional
            // second up to an invalid one-million-microsecond timeval field.
            .map(|left| Duration::from_micros(left.as_micros() as u64))
            .filter(|left| !left.is_zero())
            .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "IPC frame deadline exceeded"))
    }
}
impl Read for Framed<'_> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        self.stream.set_read_timeout(Some(self.remaining()?))?;
        self.stream.read(bytes)
    }
}
impl Write for Framed<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.stream.set_write_timeout(Some(self.remaining()?))?;
        self.stream.write(bytes)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.stream.flush()
    }
}
fn read_request(stream: &mut UnixStream) -> Result<Option<Value>> {
    let line = mcp::read_message(&mut BufReader::with_capacity(1024, Framed::new(stream)))?
        .ok_or_else(|| error("Empty IPC request"))?;
    Ok(serde_json::from_str(&line).ok())
}
fn write_reply(stream: &mut UnixStream, reply: Option<Value>) -> Result<()> {
    let bytes = serde_json::to_vec(&reply)?;
    if bytes.len() >= REPLY_LIMIT {
        return Err(error("IPC reply exceeds 1 MiB"));
    }
    let mut framed = Framed::new(stream);
    framed.write_all(&bytes)?;
    framed.write_all(b"\n")?;
    framed.flush()?;
    Ok(())
}
/// Direct one-request exchange for callers that own their own listener.
pub fn exchange(
    stream: &mut UnixStream,
    handle: impl FnOnce(Value) -> Option<Value>,
) -> Result<()> {
    let reply = match read_request(stream)? {
        Some(request) => handle(request),
        None => Some(mcp::rpc_error(Value::Null, -32700, "Invalid JSON")),
    };
    write_reply(stream, reply)
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
