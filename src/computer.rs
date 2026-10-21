//! Shutdown-only signed datagrams. Neither endpoint exposes a shell or credentials.
//! Both directions are authenticated; fresh peer-bound challenges are single-use.
use crate::{Result, config::Source, device::Adapter, entity, error, media, wol};
use ring::signature::{ED25519, Ed25519KeyPair, KeyPair, UnparsedPublicKey};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    net::{SocketAddr, UdpSocket},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

const MESSAGE: usize = 98;
const FRAME: usize = MESSAGE + 64;
const DOMAIN: &[u8] = b"homewire-computer-v1\0";
const HELLO: u8 = 0;
const VALIDATE: u8 = 1;
const SHUTDOWN: u8 = 2;
const CHALLENGE: u8 = 16;
const VALIDATED: u8 = 17;
const ACCEPTED: u8 = 18;
const FAILED: u8 = 19;
const PENDING: u8 = 20;
const TTL: Duration = Duration::from_secs(10);

fn identity(name: &str) -> Result<[u8; 32]> {
    if !crate::config::valid_unique_labels(std::iter::once(name), 24, b"_-") {
        return Err(error("Invalid computer identity"));
    }
    let mut out = [0; 32];
    out[..name.len()].copy_from_slice(name.as_bytes());
    Ok(out)
}
fn key(seed: &str) -> Result<Ed25519KeyPair> {
    Ed25519KeyPair::from_seed_unchecked(&media::hex::<32>(seed)?)
        .map_err(|_| error("Invalid computer signing key"))
}
fn random() -> Result<[u8; 32]> {
    let mut bytes = [0; 32];
    getrandom::getrandom(&mut bytes)?;
    Ok(bytes)
}
fn signed(
    kind: u8,
    id: [u8; 32],
    nonce: [u8; 32],
    challenge: [u8; 32],
    key: &Ed25519KeyPair,
) -> [u8; FRAME] {
    let mut frame = [0; FRAME];
    frame[0] = 1;
    frame[1] = kind;
    frame[2..34].copy_from_slice(&id);
    frame[34..66].copy_from_slice(&nonce);
    frame[66..98].copy_from_slice(&challenge);
    let mut message = Vec::with_capacity(DOMAIN.len() + MESSAGE);
    message.extend_from_slice(DOMAIN);
    message.extend_from_slice(&frame[..MESSAGE]);
    frame[MESSAGE..].copy_from_slice(key.sign(&message).as_ref());
    frame
}
fn verify(frame: &[u8], id: [u8; 32], public: &[u8; 32]) -> Result<()> {
    if frame.len() != FRAME || frame[0] != 1 || frame[2..34] != id {
        return Err(error("Invalid computer protocol frame or identity"));
    }
    let mut message = Vec::with_capacity(DOMAIN.len() + MESSAGE);
    message.extend_from_slice(DOMAIN);
    message.extend_from_slice(&frame[..MESSAGE]);
    UnparsedPublicKey::new(&ED25519, public)
        .verify(&message, &frame[MESSAGE..])
        .map_err(|_| error("Computer signature verification failed"))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClientConfig {
    pub name: String,
    pub address: SocketAddr,
    pub server_public_key: String,
    pub signing_key: String,
    pub mac: String,
    pub wake_address: SocketAddr,
    #[serde(default = "timeout")]
    pub timeout_seconds: u64,
}
fn timeout() -> u64 {
    3
}
pub struct Manager {
    name: String,
    address: SocketAddr,
    public: [u8; 32],
    signer: Ed25519KeyPair,
    id: [u8; 32],
    mac: [u8; 6],
    wake_address: SocketAddr,
    timeout: Duration,
    _lock: crate::config::FileLock,
}
impl Manager {
    pub fn validate_shutdown(&self) -> Result<Value> {
        self.request(VALIDATE)
    }
    pub fn shutdown(&self) -> Result<Value> {
        self.request(SHUTDOWN)
    }
    pub fn status(&self) -> Result<Value> {
        self.request(HELLO)
    }
    /// Only transport unavailability qualifies. Authentication/protocol failures
    /// must abort shutdown verification rather than look like a powered-off host.
    pub fn unreachable(&self) -> Result<bool> {
        match self.status() {
            Ok(_) => Ok(false),
            Err(e)
                if e.downcast_ref::<std::io::Error>().is_some_and(|e| {
                    matches!(
                        e.kind(),
                        std::io::ErrorKind::TimedOut
                            | std::io::ErrorKind::WouldBlock
                            | std::io::ErrorKind::ConnectionRefused
                            | std::io::ErrorKind::HostUnreachable
                            | std::io::ErrorKind::NetworkUnreachable
                    )
                }) =>
            {
                Ok(true)
            }
            Err(e) => Err(e),
        }
    }
    pub fn wake(&self) -> Result<Value> {
        wol::send(self.mac, self.wake_address)?;
        Ok(json!({"sent":true,"verified":false,"evidence":"magic_packet_sent_once"}))
    }
    pub fn name(&self) -> &str {
        &self.name
    }
    pub(crate) fn shutdown_identity(&self) -> [u8; 32] {
        use sha2::{Digest, Sha256};
        let mut hash = Sha256::new();
        hash.update(self.id);
        hash.update(self.public);
        hash.update(self.signer.public_key().as_ref());
        hash.update(self.address.to_string());
        hash.finalize().into()
    }
    fn request(&self, operation: u8) -> Result<Value> {
        let status_only = operation == HELLO;
        let operation = if status_only { VALIDATE } else { operation };
        let socket = UdpSocket::bind(if self.address.is_ipv4() {
            "0.0.0.0:0"
        } else {
            "[::]:0"
        })?;
        socket.connect(self.address)?;
        let nonce = random()?;
        let until = Instant::now() + self.timeout;
        let exchange = |frame: [u8; FRAME]| -> Result<[u8; FRAME]> {
            let remaining = until
                .checked_duration_since(Instant::now())
                .ok_or_else(|| error("Computer request deadline exceeded"))?;
            socket.set_write_timeout(Some(remaining))?;
            if socket.send(&frame)? != FRAME {
                return Err(error("Incomplete computer request"));
            }
            // Ignore unauthenticated/spurious packets only within the total deadline.
            let mut invalid_response = false;
            loop {
                let remaining = until
                    .checked_duration_since(Instant::now())
                    .ok_or_else(|| {
                        if invalid_response {
                            error("Computer response authentication failed")
                        } else {
                            std::io::Error::new(
                                std::io::ErrorKind::TimedOut,
                                "Computer request deadline exceeded",
                            )
                            .into()
                        }
                    })?;
                socket.set_read_timeout(Some(remaining))?;
                let mut response = [0; FRAME + 1];
                let size = match socket.recv(&mut response) {
                    Ok(size) => size,
                    Err(_) if invalid_response => {
                        return Err(error("Computer response authentication failed"));
                    }
                    Err(e) => return Err(e.into()),
                };
                if verify(&response[..size], self.id, &self.public).is_ok()
                    && response[34..66] == nonce
                {
                    return response[..FRAME]
                        .try_into()
                        .map_err(|_| error("Invalid response size"));
                }
                invalid_response = true;
            }
        };
        let response = exchange(signed(HELLO, self.id, nonce, [0; 32], &self.signer))?;
        if response[1] == PENDING {
            return if status_only {
                Ok(json!({"power":true,"verified":true,"state":"shutdown_pending"}))
            } else {
                Err(error("Computer shutdown already pending; do not retry"))
            };
        }
        if response[1] != CHALLENGE {
            return Err(error("Computer did not return a valid challenge"));
        }
        let challenge = response[66..98]
            .try_into()
            .map_err(|_| error("Invalid challenge"))?;
        let response = exchange(signed(operation, self.id, nonce, challenge, &self.signer))?;
        if response[66..98] != challenge {
            return Err(error("Computer response challenge mismatch"));
        }
        match (operation, response[1]) {
            (VALIDATE, VALIDATED) if status_only => {
                Ok(json!({"power":true,"verified":true,"state":"running"}))
            }
            (VALIDATE, VALIDATED) => {
                Ok(json!({"authenticated":true,"validated":true,"shutdown_requested":false}))
            }
            (SHUTDOWN, ACCEPTED) => Ok(
                json!({"shutdown_requested":true,"verified":false,"evidence":"os_shutdown_queued"}),
            ),
            _ => Err(error(
                "Computer rejected request; do not automatically retry shutdown",
            )),
        }
    }
}
impl Adapter for Manager {
    const TOOLS: &'static [crate::device::Tool] = &[];
    const SUPPORT: entity::Support = entity::Support::SWITCH
        .union(entity::Support::WAKE)
        .union(entity::Support::ROUTINES);
    fn from_source(source: &Source<'_>) -> Result<Self> {
        let c: ClientConfig = source.decode()?;
        let id = identity(&c.name)?;
        let mac = wol::mac_bytes(&c.mac)?;
        if c.address.port() == 0
            || c.address.ip().is_unspecified()
            || c.address.ip().is_multicast()
            || !(1..=10).contains(&c.timeout_seconds)
            || mac == [0; 6]
            || mac[0] & 1 != 0
            || c.wake_address.port() == 0
            || c.wake_address.ip().is_unspecified()
            || c.wake_address.ip().is_multicast()
        {
            return Err(error("Invalid computer endpoint"));
        }
        Ok(Self {
            id,
            public: media::hex(&c.server_public_key)?,
            signer: key(&c.signing_key)?,
            name: c.name,
            address: c.address,
            mac,
            wake_address: c.wake_address,
            timeout: Duration::from_secs(c.timeout_seconds),
            _lock: crate::config::lock_configuration(source.path())?,
        })
    }
    fn entities<'a>(&'a self, visit: &mut impl FnMut(entity::Entity<'a>)) {
        visit(entity::Entity::new(
            crate::device::Family::Computer,
            0,
            entity::Domain::Switch,
            &self.name,
            "Homewire shutdown service",
            entity::Capabilities::Power,
        ));
    }
    fn wake_targets<'a>(&'a self, visit: &mut impl FnMut(wol::Target<'a>)) {
        visit(wol::Target {
            device: entity::Device {
                family: crate::device::Family::Computer,
                index: 0,
            },
            name: &self.name,
            mac: self.mac,
            verified: false,
        });
    }
    fn wake_device(&mut self, index: usize) -> Result<Value> {
        if index != 0 {
            return Err(error("Unknown computer"));
        }
        self.wake()
    }
    fn read_device(&mut self, index: usize) -> Result<Value> {
        if index != 0 {
            return Err(error("Unknown computer"));
        }
        self.status()
    }
    fn control_device(
        &mut self,
        index: usize,
        command: entity::Command,
        args: &Value,
    ) -> Result<Value> {
        if index != 0 || !matches!(command, entity::Command::Power) {
            return Err(error("Unsupported computer control"));
        }
        match args["power"].as_bool() {
            Some(true) => self.wake(),
            Some(false) => self.shutdown(),
            _ => Err(error("Expected computer power")),
        }
    }
    fn execute(&mut self, _: &str, _: &Value) -> Result<Value> {
        Err(error("Unknown computer operation"))
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerConfig {
    pub name: String,
    pub listen: SocketAddr,
    pub client_public_key: String,
    pub identity_key_file: PathBuf,
}
struct Challenge {
    peer: SocketAddr,
    nonce: [u8; 32],
    challenge: [u8; 32],
    issued: Instant,
}
struct Server {
    id: [u8; 32],
    public: [u8; 32],
    signer: Ed25519KeyPair,
    challenges: [Option<Challenge>; 8],
    pending: bool,
}
impl Server {
    fn handle(
        &mut self,
        frame: &[u8],
        peer: SocketAddr,
        now: Instant,
        shutdown: &mut impl FnMut() -> Result<()>,
    ) -> Result<Option<[u8; FRAME]>> {
        verify(frame, self.id, &self.public)?;
        let nonce = frame[34..66]
            .try_into()
            .map_err(|_| error("Invalid nonce"))?;
        for slot in &mut self.challenges {
            if slot
                .as_ref()
                .is_some_and(|c| now.duration_since(c.issued) >= TTL)
            {
                *slot = None;
            }
        }
        if self.pending {
            return Ok(Some(signed(PENDING, self.id, nonce, [0; 32], &self.signer)));
        }
        if frame[1] == HELLO {
            if frame[66..98] != [0; 32] {
                return Err(error("Invalid hello"));
            }
            let existing = self
                .challenges
                .iter()
                .flatten()
                .find(|c| c.peer == peer && c.nonce == nonce);
            let challenge = if let Some(c) = existing {
                c.challenge
            } else {
                let slot = self
                    .challenges
                    .iter_mut()
                    .find(|c| c.is_none())
                    .ok_or_else(|| error("Computer challenge slots exhausted"))?;
                let challenge = random()?;
                *slot = Some(Challenge {
                    peer,
                    nonce,
                    challenge,
                    issued: now,
                });
                challenge
            };
            return Ok(Some(signed(
                CHALLENGE,
                self.id,
                nonce,
                challenge,
                &self.signer,
            )));
        }
        if !matches!(frame[1], VALIDATE | SHUTDOWN) {
            return Err(error("Unknown computer operation"));
        }
        let slot = self
            .challenges
            .iter_mut()
            .find(|c| {
                c.as_ref().is_some_and(|c| {
                    c.peer == peer && c.nonce == nonce && frame[66..98] == c.challenge
                })
            })
            .ok_or_else(|| error("Expired, used or invalid computer challenge"))?;
        let challenge = slot
            .take()
            .ok_or_else(|| error("Missing challenge"))?
            .challenge;
        let kind = if frame[1] == VALIDATE {
            VALIDATED
        } else {
            // Consume before invoking the OS. An uncertain attempt cannot be replayed.
            self.pending = true;
            if shutdown().is_ok() { ACCEPTED } else { FAILED }
        };
        Ok(Some(signed(kind, self.id, nonce, challenge, &self.signer)))
    }
}

/// Native single-thread service. No runtime shell, arbitrary commands or outbound controls.
pub fn serve(path: &Path) -> Result<()> {
    let c: ServerConfig = serde_json::from_slice(&server_file(path, 8192)?)
        .map_err(|_| error("Invalid computer service configuration"))?;
    let seed = server_file(&c.identity_key_file, 128)?;
    let signer = key(std::str::from_utf8(&seed)
        .map_err(|_| error("Invalid identity key"))?
        .trim())?;
    if c.listen.port() == 0 || c.listen.ip().is_multicast() {
        return Err(error("Invalid computer service listener"));
    }
    let mut server = Server {
        id: identity(&c.name)?,
        public: media::hex(&c.client_public_key)?,
        signer,
        challenges: std::array::from_fn(|_| None),
        pending: false,
    };
    let socket = UdpSocket::bind(c.listen)?;
    socket.set_write_timeout(Some(Duration::from_secs(1)))?;
    let mut buffer = [0; FRAME + 1];
    loop {
        let (size, peer) = socket.recv_from(&mut buffer)?;
        let response = server.handle(&buffer[..size], peer, Instant::now(), &mut || {
            // Polkit permits only starting this fixed root-owned oneshot unit.
            let status = std::process::Command::new("/usr/bin/systemctl")
                .args([
                    "--no-ask-password",
                    "--no-block",
                    "start",
                    "homewire-poweroff.service",
                ])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()?;
            if status.success() {
                Ok(())
            } else {
                Err(error("OS refused shutdown"))
            }
        });
        // Invalid traffic is silently discarded; it cannot fill logs or queues.
        if let Ok(Some(response)) = response {
            let _ = socket.send_to(&response, peer);
        }
    }
}

fn server_file(path: &Path, limit: usize) -> Result<Vec<u8>> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        // LoadCredential mounts root-owned 0440 files privately for this service.
        // Ordinary device configurations retain the existing mode 0600 rule.
        if path.parent() == Some(Path::new("/run/credentials/homewire-power.service")) {
            let file = std::fs::metadata(path)?;
            let directory = std::fs::metadata(
                path.parent()
                    .ok_or_else(|| error("Missing credential directory"))?,
            )?;
            if file.uid() != 0
                || !matches!(file.mode() & 0o777, 0o400 | 0o440)
                || directory.uid() != 0
                || directory.mode() & 0o027 != 0
            {
                return Err(error("Invalid protected service credential permissions"));
            }
        } else {
            crate::config::private_permissions(path)?;
        }
    }
    #[cfg(not(unix))]
    crate::config::private_permissions(path)?;
    Ok(crate::config::read_bounded_limit(
        path,
        "Computer service credential",
        limit,
    )?)
}

/// Write a new seed once with mode 0600; return only its public verification key.
pub fn generate_key(path: &Path) -> Result<String> {
    use std::io::Write;
    let seed = random()?;
    let text: String = seed.iter().map(|b| format!("{b:02x}")).collect();
    let pair = key(&text)?;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(text.as_bytes())?;
    file.sync_all()?;
    Ok(pair
        .public_key()
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn client(address: SocketAddr) -> Manager {
        let (server, signer, _, _) = fixture();
        let mut nonce = [0; 8];
        getrandom::getrandom(&mut nonce).expect("nonce");
        let lock_path =
            std::env::temp_dir().join(format!("hw-computer-{:016x}", u64::from_ne_bytes(nonce)));
        let lock = crate::config::lock_file(
            crate::config::private_options()
                .read(true)
                .write(true)
                .create_new(true)
                .open(&lock_path)
                .expect("lock file"),
            "test fixture lock",
        )
        .expect("lock");
        std::fs::remove_file(lock_path).expect("remove fixture lock");
        Manager {
            name: "workstation".into(),
            address,
            public: server
                .signer
                .public_key()
                .as_ref()
                .try_into()
                .expect("public"),
            signer,
            id: server.id,
            mac: [2, 0, 0, 0, 0, 1],
            wake_address: "127.0.0.1:9".parse().expect("wake"),
            timeout: Duration::from_secs(1),
            _lock: lock,
        }
    }
    #[test]
    fn transport_timeout_is_unreachable_but_bad_signature_is_not() {
        let socket = UdpSocket::bind("127.0.0.1:0").expect("bind");
        let client = client(socket.local_addr().expect("address"));
        assert!(client.unreachable().expect("transport timeout"));
        let sender = std::thread::spawn(move || {
            let mut frame = [0; FRAME];
            let _ = socket.recv_from(&mut frame).expect("receive queued hello");
            let (_, peer) = socket.recv_from(&mut frame).expect("receive next hello");
            frame[1] = CHALLENGE;
            socket
                .send_to(&frame, peer)
                .expect("bad signature response");
            socket
        });
        let result = client.unreachable();
        let _socket = sender.join().expect("sender");
        assert!(
            result.is_err(),
            "unauthenticated responses cannot prove shutdown"
        );
    }
    #[test]
    fn signed_running_service_is_not_unreachable() {
        let socket = UdpSocket::bind("127.0.0.1:0").expect("bind");
        let client = client(socket.local_addr().expect("address"));
        let sender = std::thread::spawn(move || {
            let (mut server, _, _, _) = fixture();
            for _ in 0..2 {
                let mut frame = [0; FRAME + 1];
                let (n, peer) = socket.recv_from(&mut frame).expect("receive");
                let reply = server
                    .handle(&frame[..n], peer, Instant::now(), &mut || {
                        panic!("no shutdown")
                    })
                    .expect("request")
                    .expect("reply");
                socket.send_to(&reply, peer).expect("reply");
            }
        });
        assert!(!client.unreachable().expect("running"));
        sender.join().expect("sender");
    }
    fn fixture() -> (Server, Ed25519KeyPair, SocketAddr, Instant) {
        let client = key(&"11".repeat(32)).expect("client");
        let signer = key(&"22".repeat(32)).expect("server");
        let public = client.public_key().as_ref().try_into().expect("public");
        (
            Server {
                id: identity("workstation").expect("id"),
                public,
                signer,
                challenges: std::array::from_fn(|_| None),
                pending: false,
            },
            client,
            "127.0.0.1:4321".parse().expect("peer"),
            Instant::now(),
        )
    }
    fn hello(s: &mut Server, c: &Ed25519KeyPair, p: SocketAddr, t: Instant) -> [u8; 32] {
        let response = s
            .handle(&signed(HELLO, s.id, [7; 32], [0; 32], c), p, t, &mut || {
                panic!("hello cannot shutdown")
            })
            .expect("hello")
            .expect("response");
        response[66..98].try_into().expect("challenge")
    }
    #[test]
    fn validation_consumes_challenge_without_shutdown() {
        let (mut s, c, p, t) = fixture();
        let challenge = hello(&mut s, &c, p, t);
        let frame = signed(VALIDATE, s.id, [7; 32], challenge, &c);
        let response = s
            .handle(&frame, p, t, &mut || panic!("validation cannot shutdown"))
            .expect("validate")
            .expect("response");
        assert_eq!(response[1], VALIDATED);
        assert!(s.handle(&frame, p, t, &mut || panic!("replay")).is_err());
        verify(
            &response,
            s.id,
            &s.signer.public_key().as_ref().try_into().expect("public"),
        )
        .expect("server authenticated");
    }
    #[test]
    fn invalid_expired_wrong_peer_and_wrong_identity_never_shutdown() {
        let (mut s, c, p, t) = fixture();
        let challenge = hello(&mut s, &c, p, t);
        let frame = signed(SHUTDOWN, s.id, [7; 32], challenge, &c);
        let mut tampered = frame;
        tampered[MESSAGE] ^= 1;
        assert!(
            s.handle(&tampered, p, t, &mut || panic!("signature"))
                .is_err()
        );
        assert!(
            s.handle(
                &frame,
                "127.0.0.1:4322".parse().expect("peer"),
                t,
                &mut || panic!("peer")
            )
            .is_err()
        );
        assert!(
            s.handle(
                &signed(
                    SHUTDOWN,
                    identity("other").expect("id"),
                    [7; 32],
                    challenge,
                    &c
                ),
                p,
                t,
                &mut || panic!("identity")
            )
            .is_err()
        );
        assert!(
            s.handle(&frame, p, t + TTL, &mut || panic!("expired"))
                .is_err()
        );
    }
    #[test]
    fn shutdown_executes_once_even_on_executor_failure() {
        for fail in [false, true] {
            let (mut s, c, p, t) = fixture();
            let challenge = hello(&mut s, &c, p, t);
            let frame = signed(SHUTDOWN, s.id, [7; 32], challenge, &c);
            let mut count = 0;
            let mut executor = || {
                count += 1;
                if fail { Err(error("failure")) } else { Ok(()) }
            };
            let response = s
                .handle(&frame, p, t, &mut executor)
                .expect("request")
                .expect("response");
            assert_eq!(response[1], if fail { FAILED } else { ACCEPTED });
            assert_eq!(
                s.handle(&frame, p, t, &mut executor)
                    .expect("replay")
                    .expect("response")[1],
                PENDING
            );
            assert_eq!(count, 1);
        }
    }
    #[test]
    fn oversized_frames_unknown_commands_and_slot_exhaustion_are_bounded() {
        let (mut s, c, p, t) = fixture();
        assert!(
            s.handle(&[0; FRAME + 1], p, t, &mut || panic!("oversize"))
                .is_err()
        );
        assert!(
            s.handle(
                &signed(3, s.id, [7; 32], [0; 32], &c),
                p,
                t,
                &mut || panic!("unknown")
            )
            .is_err()
        );
        for n in 0..8 {
            s.handle(
                &signed(HELLO, s.id, [n; 32], [0; 32], &c),
                p,
                t,
                &mut || panic!("hello"),
            )
            .expect("slot");
        }
        assert!(
            s.handle(
                &signed(HELLO, s.id, [9; 32], [0; 32], &c),
                p,
                t,
                &mut || panic!("full")
            )
            .is_err()
        );
        assert!(
            s.handle(
                &signed(HELLO, s.id, [9; 32], [0; 32], &c),
                p,
                t + TTL,
                &mut || panic!("expire")
            )
            .is_ok()
        );
    }
}
