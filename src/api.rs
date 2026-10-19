//! Small HTTPS frontend to the single device owner. One bounded connection at a
//! time, no async runtime, device locks, duplicated scheduler or state cache.
use crate::{Result, config, device::Devices, entity, error, ipc, mcp};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};
trait ApiTransport: Read + Write {
    fn response_deadline(&mut self);
}
impl ApiTransport for crate::transport::Timed {
    fn response_deadline(&mut self) {
        self.set_deadline(Instant::now() + Duration::from_secs(310));
    }
}
impl ApiTransport for rustls::StreamOwned<rustls::ServerConnection, crate::transport::Timed> {
    fn response_deadline(&mut self) {
        self.sock.response_deadline();
    }
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Client {
    name: String,
    token: String,
    #[serde(default)]
    assistant: bool,
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Assistant {
    url: String,
    bearer_token: String,
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct AgentEndpoint {
    listen: SocketAddr,
    token: String,
    tools: Vec<String>,
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    listen: SocketAddr,
    dashboard_token: String,
    #[serde(default)]
    clients: Vec<Client>,
    #[serde(default)]
    certificate: Option<PathBuf>,
    #[serde(default)]
    private_key: Option<PathBuf>,
    #[serde(default)]
    assistant: Option<Assistant>,
    #[serde(default)]
    agent: Option<AgentEndpoint>,
    #[serde(skip)]
    agent_only: bool,
}
pub struct Settings {
    config: Config,
    tls: Option<Arc<rustls::ServerConfig>>,
}
impl Settings {
    pub fn from_source(source: &config::Source<'_>) -> Result<Self> {
        use rustls::pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject};
        let mut cfg: Config = source.decode()?;
        let path = source.path();
        for file in [&mut cfg.certificate, &mut cfg.private_key]
            .into_iter()
            .flatten()
        {
            if file.is_relative() && !config::environment_secret_path(file) {
                *file = path.parent().unwrap_or(Path::new(".")).join(&*file);
            }
        }
        if !token_valid(&cfg.dashboard_token)
            || cfg.clients.len() > 8
            || !config::valid_unique_labels(cfg.clients.iter().map(|c| c.name.as_str()), 32, b"-_")
            || cfg.clients.iter().enumerate().any(|(i, c)| {
                !token_valid(&c.token)
                    || equal(&c.token, &cfg.dashboard_token)
                    || cfg.clients[..i].iter().any(|p| equal(&p.token, &c.token))
            })
        {
            return Err(error("API needs distinct 32-byte hexadecimal tokens"));
        }
        if let Some(a) = &cfg.assistant {
            let u = crate::web::Url::parse(&a.url)?;
            if u.host != "127.0.0.1"
                || !u.path.starts_with("/webhook")
                || a.bearer_token.is_empty()
                || a.bearer_token.len() > 256
                || a.bearer_token.bytes().any(|b| !b.is_ascii_graphic())
            {
                return Err(error(
                    "Assistant requires a loopback ZeroClaw webhook and its dedicated gateway token",
                ));
            }
        }
        if let Some(a) = &cfg.agent
            && (!a.listen.ip().is_loopback()
                || !token_valid(&a.token)
                || equal(&a.token, &cfg.dashboard_token)
                || cfg.clients.iter().any(|c| equal(&a.token, &c.token))
                || a.tools.is_empty()
                || a.tools.len() > 32
                || a.tools.iter().enumerate().any(|(i, n)| {
                    n.is_empty()
                        || n.len() > 64
                        || !n.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
                        || a.tools[..i].contains(n)
                }))
        {
            return Err(error(
                "Agent MCP requires loopback, a distinct token and an explicit tool scope",
            ));
        }
        let tls = match (&cfg.certificate, &cfg.private_key) {
            (Some(cert), Some(key)) => {
                config::private_permissions(key)?;
                let cert = CertificateDer::from_pem_slice(&config::read_bounded(
                    cert,
                    "API certificate",
                )?)?;
                let key = PrivateKeyDer::from_pem_slice(&config::read_bounded(key, "API key")?)?;
                let provider = Arc::new(rustls::crypto::ring::default_provider());
                Some(Arc::new(
                    rustls::ServerConfig::builder_with_provider(provider)
                        .with_safe_default_protocol_versions()?
                        .with_no_client_auth()
                        .with_single_cert(vec![cert], key)?,
                ))
            }
            (None, None) if cfg.listen.ip().is_loopback() => None,
            _ => {
                return Err(error(
                    "LAN API requires an HTTPS certificate and private key",
                ));
            }
        };
        Ok(Self { config: cfg, tls })
    }
}
pub struct Server {
    listen: SocketAddr,
    stopped: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
    _agent: Option<Box<Server>>,
}
// Fixed-size counters live at the trusted endpoint, outside ZeroClaw. Denied
// attempts never reach the device owner; time uses a monotonic clock.
struct AgentBudget {
    minute: (Instant, u16),
    hour: (Instant, u16),
}
impl AgentBudget {
    fn new(now: Instant) -> Self {
        Self {
            minute: (now, 0),
            hour: (now, 0),
        }
    }
    fn admit(&mut self, data: &Value, now: Instant) -> bool {
        if now.duration_since(self.minute.0) >= Duration::from_secs(60) {
            self.minute = (now, 0);
        }
        if now.duration_since(self.hour.0) >= Duration::from_secs(3600) {
            self.hour = (now, 0);
        }
        let control = data["method"] == "tools/call"
            && entity::tools(crate::device::Selection::ALL)
                .any(|t| Some(t.name) == data["params"]["name"].as_str() && !t.hints.read_only);
        if self.minute.1 >= 30 || (control && self.hour.1 >= 20) {
            return false;
        }
        self.minute.1 += 1;
        self.hour.1 += u16::from(control);
        true
    }
}
impl Server {
    pub fn local_addr(&self) -> SocketAddr {
        self.listen
    }
    pub fn start(path: &Path, socket: &Path) -> Result<Self> {
        Self::start_settings(Settings::from_source(&config::Source::file(path))?, socket)
    }
    pub fn start_settings(settings: Settings, socket: &Path) -> Result<Self> {
        let Settings { config: cfg, tls } = settings;
        let agent = if let Some(a) = &cfg.agent {
            let mut restricted = cfg.clone();
            restricted.listen = a.listen;
            restricted.certificate = None;
            restricted.private_key = None;
            restricted.assistant = None;
            restricted.clients.clear();
            restricted.agent_only = true;
            Some(Box::new(Self::listen(restricted, socket, None)?))
        } else {
            None
        };
        let mut server = Self::listen(cfg, socket, tls)?;
        server._agent = agent;
        Ok(server)
    }
    fn listen(cfg: Config, socket: &Path, tls: Option<Arc<rustls::ServerConfig>>) -> Result<Self> {
        let listener = TcpListener::bind(cfg.listen)?;
        let listen = listener.local_addr()?;
        let stopped = Arc::new(AtomicBool::new(false));
        let stop = stopped.clone();
        let socket = socket.to_owned();
        let worker = thread::Builder::new()
            .name("homewire-api".into())
            .stack_size(256 * 1024)
            .spawn(move || {
                let mut budget = AgentBudget::new(Instant::now());
                for stream in listener.incoming() {
                    if stop.load(Ordering::Acquire) {
                        break;
                    }
                    let Ok(stream) = stream else { break };
                    let stream = crate::transport::Timed::accepted(
                        stream,
                        Instant::now() + Duration::from_secs(10),
                    );
                    let result = if let Some(tls) = &tls {
                        match rustls::ServerConnection::new(tls.clone()) {
                            Ok(mut conn) => {
                                conn.set_buffer_limit(Some(8192));
                                handle(
                                    &mut rustls::StreamOwned::new(conn, stream),
                                    &cfg,
                                    &socket,
                                    &mut budget,
                                )
                            }
                            Err(e) => Err(e.into()),
                        }
                    } else {
                        handle(&mut { stream }, &cfg, &socket, &mut budget)
                    };
                    if result.is_err() {
                        eprintln!("API connection rejected or interrupted");
                    }
                }
            })?;
        Ok(Self {
            listen,
            stopped,
            worker: Some(worker),
            _agent: None,
        })
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Release);
        let _ = TcpStream::connect(self.listen);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
fn token_valid(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())
}
fn equal(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.bytes().zip(b.bytes()).fold(0, |v, (x, y)| v | (x ^ y)) == 0
}
fn handle(
    stream: &mut impl ApiTransport,
    cfg: &Config,
    socket: &Path,
    budget: &mut AgentBudget,
) -> Result<()> {
    let mut input = BufReader::with_capacity(1024, &mut *stream);
    let mut line = String::new();
    input.by_ref().take(1025).read_line(&mut line)?;
    let parts: Vec<_> = line.trim_end().split(' ').collect();
    if parts.len() != 3 || parts[2] != "HTTP/1.1" {
        return Err(error("Invalid HTTP request"));
    }
    let (method, path) = (parts[0].to_owned(), parts[1].to_owned());
    let mut auth = String::new();
    let mut length = None;
    let mut total = line.len();
    let mut origin = None;
    let mut host = None;
    loop {
        line.clear();
        input.by_ref().take(4097).read_line(&mut line)?;
        total += line.len();
        if line.is_empty() || total > 4096 || !line.ends_with("\r\n") {
            return Err(error("Invalid bounded HTTP header"));
        }
        if line == "\r\n" {
            break;
        }
        let (k, v) = line
            .trim_end()
            .split_once(':')
            .ok_or_else(|| error("Invalid header"))?;
        let v = v.trim();
        match k.to_ascii_lowercase().as_str() {
            "authorization" => {
                if !auth.is_empty() {
                    return Err(error("Duplicate credentials"));
                }
                auth = v.into();
            }
            "content-length" => {
                if length.is_some() {
                    return Err(error("Duplicate body length"));
                }
                length = Some(v.parse::<usize>()?);
            }
            "transfer-encoding" => return Err(error("Chunked requests are unsupported")),
            "origin" => origin = Some(v.to_owned()),
            "host" => host = Some(v.to_owned()),
            _ => {}
        }
    }
    if origin.is_some_and(|o| {
        host.is_none_or(|h| {
            o != format!(
                "{}://{h}",
                if cfg.certificate.is_some() {
                    "https"
                } else {
                    "http"
                }
            )
        })
    }) {
        return response(stream, 403, b"{}", "application/json");
    }
    if method == "GET" && path == "/" && !cfg.agent_only {
        return response(
            stream,
            200,
            include_bytes!("dashboard.html"),
            "text/html; charset=utf-8",
        );
    }
    let token = auth.strip_prefix("Bearer ").unwrap_or_default();
    if cfg.agent_only {
        let agent = cfg
            .agent
            .as_ref()
            .ok_or_else(|| error("Missing agent scope"))?;
        if !equal(token, &agent.token) {
            return response(stream, 401, b"{}", "application/json");
        }
        if method != "POST" || path != "/mcp" {
            return response(stream, 404, b"{}", "application/json");
        }
        let len = length.unwrap_or(0);
        if len == 0 || len > 8192 {
            return response(stream, 413, b"{}", "application/json");
        }
        let mut body = vec![0; len];
        input.read_exact(&mut body)?;
        drop(input);
        let data: Value = serde_json::from_slice(&body).map_err(|_| error("Invalid MCP JSON"))?;
        if !budget.admit(&data, Instant::now()) {
            return response(stream, 429, b"{}", "application/json");
        }
        stream.response_deadline();
        let request = json!({"jsonrpc":"2.0","id":1,"method":"homewire/api","params":{"role":"agent","route":"/mcp","tools":agent.tools,"data":data}});
        let reply =
            ipc::request(socket, &request.to_string())?.ok_or_else(|| error("No owner reply"))?;
        if let Some(error) = reply.get("error") {
            let body = json!({"jsonrpc":"2.0","id":request.pointer("/params/data/id").cloned().unwrap_or(Value::Null),"error":error});
            return response(stream, 503, &serde_json::to_vec(&body)?, "application/json");
        }
        let result = &reply["result"];
        return if result.is_null() {
            response(stream, 202, b"", "application/json")
        } else {
            response(
                stream,
                200,
                &serde_json::to_vec(result)?,
                "application/json",
            )
        };
    }
    let role = if equal(token, &cfg.dashboard_token) {
        "dashboard".to_owned()
    } else if let Some(c) = cfg.clients.iter().find(|c| equal(token, &c.token)) {
        format!("mobile:{}", c.name)
    } else {
        return response(stream, 401, b"{}", "application/json");
    };
    if !matches!(method.as_str(), "GET" | "POST")
        || !matches!(
            path.as_str(),
            "/api/call"
                | "/api/mobile/report"
                | "/api/mobile/alerts"
                | "/api/mobile/ack"
                | "/api/assistant"
        )
        || ((path == "/api/mobile/alerts") != (method == "GET"))
    {
        return response(stream, 404, b"{}", "application/json");
    }
    let len = length.unwrap_or(0);
    if len > 8192 {
        return response(stream, 413, b"{}", "application/json");
    }
    let mut body = vec![0; len];
    input.read_exact(&mut body)?;
    drop(input);
    stream.response_deadline();
    let data = if body.is_empty() {
        json!({})
    } else {
        serde_json::from_slice(&body).map_err(|_| error("Invalid API JSON"))?
    };
    if path == "/api/assistant" {
        let permitted = role == "dashboard"
            || cfg
                .clients
                .iter()
                .any(|c| role == format!("mobile:{}", c.name) && c.assistant);
        if !permitted {
            return response(stream, 403, b"{}", "application/json");
        }
        let message = data["message"]
            .as_str()
            .filter(|s| !s.trim().is_empty() && s.len() <= 4096)
            .ok_or_else(|| error("Expected a message up to 4096 bytes"))?;
        let a = cfg
            .assistant
            .as_ref()
            .ok_or_else(|| error("Assistant is not configured"))?;
        // Outside the device owner. Never retry an agent request: it may already
        // have executed a device control when its reply is lost.
        let reply = crate::web::request(
            &a.url,
            "POST",
            &[
                ("Content-Type", "application/json"),
                ("Authorization", &format!("Bearer {}", a.bearer_token)),
            ],
            &serde_json::to_vec(&json!({"message":message}))?,
            Duration::from_secs(90),
        )?;
        let value: Value = serde_json::from_slice(&reply.body)?;
        let text = value["response"]
            .as_str()
            .filter(|s| s.len() <= 32768)
            .ok_or_else(|| error("Invalid assistant response"))?;
        return response(
            stream,
            200,
            &serde_json::to_vec(&json!({"result":{"response":text}}))?,
            "application/json",
        );
    }
    let request = json!({"jsonrpc":"2.0","id":1,"method":"homewire/api","params":{"role":role,"route":path,"data":data}});
    let reply = ipc::request(socket, &request.to_string())?
        .ok_or_else(|| error("No device owner reply"))?;
    response(
        stream,
        if reply.get("error").is_some() {
            400
        } else {
            200
        },
        &serde_json::to_vec(&reply)?,
        "application/json",
    )
}
fn response(stream: &mut impl Write, status: u16, body: &[u8], content_type: &str) -> Result<()> {
    write!(
        stream,
        "HTTP/1.1 {status} Response\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nReferrer-Policy: no-referrer\r\nContent-Security-Policy: default-src 'self'; script-src 'self' 'unsafe-inline'; style-src 'self' 'unsafe-inline'; base-uri 'none'; frame-ancestors 'none'\r\nConnection: close\r\n\r\n",
        body.len()
    )?;
    stream.write_all(body)?;
    stream.flush()?;
    Ok(())
}
/// Only stateless, scoped MCP metadata can bypass the device owner. Reports,
/// acknowledgements, device reads and controls still belong to that owner.
pub fn protocol_only(request: &Value) -> bool {
    request["method"] == "homewire/api"
        && request.pointer("/params/role").and_then(Value::as_str) == Some("agent")
        && request.pointer("/params/route").and_then(Value::as_str) == Some("/mcp")
        && request
            .pointer("/params/data/method")
            .and_then(Value::as_str)
            != Some("tools/call")
}
fn agent_mcp(
    selection: crate::device::Selection,
    params: &Value,
    allow_control: bool,
    call: impl FnMut(&str, &Value) -> Result<Value>,
) -> Result<Value> {
    let scope = params["tools"]
        .as_array()
        .filter(|t| !t.is_empty() && t.len() <= 32)
        .ok_or_else(|| error("Missing tool scope"))?;
    Ok(mcp::handle_tools(
        params["data"].clone(),
        entity::tools(selection).filter(|t| scope.iter().any(|n| n.as_str() == Some(t.name))),
        mcp::Options {
            allow_control,
            light_names: false,
        },
        false,
        call,
    )
    .unwrap_or(Value::Null))
}
pub fn dispatch_protocol(
    selection: crate::device::Selection,
    request: Value,
    allow_control: bool,
) -> Option<Value> {
    let result = if protocol_only(&request) {
        agent_mcp(selection, &request["params"], allow_control, |_, _| {
            Err(error("Device calls belong to the owner"))
        })
    } else {
        Err(error("This API request requires the device owner"))
    };
    Some(match result {
        Ok(result) => json!({"jsonrpc":"2.0","id":request["id"],"result":result}),
        Err(e) => mcp::rpc_error(request["id"].clone(), -32602, &e.to_string()),
    })
}
pub fn dispatch(devices: &mut Devices, request: Value, allow_control: bool) -> Option<Value> {
    let id = request["id"].clone();
    let params = &request["params"];
    let role = params["role"].as_str().unwrap_or_default();
    let route = params["route"].as_str().unwrap_or_default();
    let data = &params["data"];
    let result = (|| -> Result<Value> {
        if role == "agent" && route == "/mcp" {
            return agent_mcp(devices.selection(), params, allow_control, |name, args| {
                entity::call(devices, name, args)
            });
        }
        if role == "dashboard" && route == "/api/call" {
            let name = data["name"]
                .as_str()
                .ok_or_else(|| error("Expected tool name"))?;
            let tool = entity::tools(devices.selection())
                .find(|t| t.name == name)
                .ok_or_else(|| error("Unknown tool"))?;
            if !allow_control && !tool.hints.read_only {
                return Err(error("Device controls are disabled"));
            }
            return entity::call(devices, name, &data["arguments"]);
        }
        let name = role
            .strip_prefix("mobile:")
            .ok_or_else(|| error("Unauthorized API scope"))?;
        match route {
            "/api/mobile/report" => devices
                .mobile
                .as_mut()
                .ok_or_else(|| error("Mobile adapter is not configured"))?
                .report(name, data, chrono::Utc::now().timestamp()),
            "/api/mobile/alerts" => Ok(devices
                .alerts
                .as_ref()
                .ok_or_else(|| error("Alerts are not configured"))?
                .companion_list(name)),
            "/api/mobile/ack" => devices
                .alerts
                .as_mut()
                .ok_or_else(|| error("Alerts are not configured"))?
                .acknowledge(name, data),
            _ => Err(error("Unauthorized API route")),
        }
    })();
    Some(match result {
        Ok(result) => json!({"jsonrpc":"2.0","id":id,"result":result}),
        Err(e) => mcp::rpc_error(id, -32602, &e.to_string()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn agent_http_exposes_busy_error_with_original_request_id_without_retry() {
        use std::os::unix::{fs::PermissionsExt, net::UnixListener};
        let mut random = [0; 8];
        getrandom::getrandom(&mut random).expect("random");
        let dir = std::env::temp_dir().join(format!(
            "homewire-api-busy-{:x}",
            u64::from_ne_bytes(random)
        ));
        std::fs::create_dir(&dir).expect("directory");
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))
            .expect("permissions");
        let path = dir.join("owner.sock");
        let listener = UnixListener::bind(&path).expect("listener");
        let owner = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .expect("deadline");
            let mut line = String::new();
            BufReader::new(&mut stream)
                .read_line(&mut line)
                .expect("request");
            assert_eq!(
                serde_json::from_str::<Value>(&line).expect("JSON")["params"]["data"]["id"],
                "caller-71"
            );
            writeln!(
                stream,
                "{}",
                mcp::rpc_error(
                    json!(1),
                    -32000,
                    "Device owner is busy; request was not queued or executed"
                )
            )
            .expect("reply");
            listener.set_nonblocking(true).expect("nonblocking");
            thread::sleep(Duration::from_millis(50));
            assert_eq!(
                listener.accept().expect_err("no retry").kind(),
                std::io::ErrorKind::WouldBlock
            );
        });
        let mut cfg: Config = serde_json::from_value(json!({
            "listen":"127.0.0.1:0", "dashboard_token":"a".repeat(64),
            "agent":{"listen":"127.0.0.1:0","token":"b".repeat(64),"tools":["entity_list"]}
        }))
        .expect("config");
        cfg.agent_only = true;
        let server = Server::listen(cfg, &path, None).expect("server");
        let mut client = TcpStream::connect(server.local_addr()).expect("connect");
        client
            .set_read_timeout(Some(Duration::from_secs(3)))
            .expect("deadline");
        let body = json!({"jsonrpc":"2.0","id":"caller-71","method":"tools/call","params":{"name":"entity_list"}}).to_string();
        write!(client,"POST /mcp HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {}\r\nContent-Length: {}\r\n\r\n{body}","b".repeat(64),body.len()).expect("request");
        let mut reply = String::new();
        client.read_to_string(&mut reply).expect("reply");
        assert!(reply.starts_with("HTTP/1.1 503"));
        let body: Value =
            serde_json::from_str(reply.split_once("\r\n\r\n").expect("body").1).expect("JSON");
        assert_eq!(body["id"], "caller-71");
        assert_eq!(body["error"]["code"], -32000);
        owner.join().expect("owner");
        drop(server);
        std::fs::remove_dir_all(dir).expect("cleanup");
    }
    #[test]
    fn protocol_fast_path_preserves_agent_scope_and_rejects_device_calls() {
        let selection = crate::device::Selection::ALL;
        let mut request = json!({"jsonrpc":"2.0","id":1,"method":"homewire/api","params":{"role":"agent","route":"/mcp","tools":["entity_list","switch_set"],"data":{"jsonrpc":"2.0","id":99,"method":"tools/list"}}});
        assert!(protocol_only(&request));
        let reply = dispatch_protocol(selection, request.clone(), false).expect("reply");
        assert_eq!(reply["result"]["id"], 99);
        let tools = reply["result"]["result"]["tools"]
            .as_array()
            .expect("tools");
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["name"], "entity_list");
        request["params"]["data"]["method"] = json!("tools/call");
        assert!(!protocol_only(&request));
        assert!(
            dispatch_protocol(selection, request.clone(), true)
                .expect("reply")
                .get("error")
                .is_some()
        );
        request["params"]["data"]["method"] = json!("ping");
        request["params"]["role"] = json!("dashboard");
        assert!(!protocol_only(&request));
        request["params"]["role"] = json!("agent");
        request["params"]["tools"] = json!([]);
        assert!(
            dispatch_protocol(selection, request, true)
                .expect("reply")
                .get("error")
                .is_some()
        );
    }
    #[test]
    fn agent_listener_rejects_bad_credentials_and_other_api_routes() {
        let cfg: Config = serde_json::from_value(json!({
            "listen":"127.0.0.1:0", "dashboard_token":"a".repeat(64),
            "agent":{"listen":"127.0.0.1:0","token":"b".repeat(64),"tools":["shopping_list"]}
        }))
        .expect("config");
        let mut restricted = cfg;
        restricted.agent_only = true;
        let server = Server::listen(restricted, Path::new("/absent-homewire-owner.sock"), None)
            .expect("server");
        for (token, path, status) in [
            ("a", "/mcp", "401"),
            ("b", "/api/call", "404"),
            ("b", "/api/assistant", "404"),
        ] {
            let mut client = TcpStream::connect(server.local_addr()).expect("connect");
            client
                .set_read_timeout(Some(Duration::from_secs(2)))
                .expect("deadline");
            write!(client,"POST {path} HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {}\r\nContent-Length: 2\r\n\r\n{{}}",token.repeat(64)).expect("request");
            let mut reply = String::new();
            client.read_to_string(&mut reply).expect("reply");
            assert!(reply.starts_with(&format!("HTTP/1.1 {status}")));
        }
    }
    #[test]
    fn endpoint_budget_bounds_reads_and_controls_independently() {
        let start = Instant::now();
        let mut budget = AgentBudget::new(start);
        let read = json!({"method":"tools/call","params":{"name":"entity_status"}});
        for _ in 0..30 {
            assert!(budget.admit(&read, start));
        }
        assert!(!budget.admit(&read, start));
        let next = start + Duration::from_secs(60);
        let control = json!({"method":"tools/call","params":{"name":"switch_set"}});
        for _ in 0..20 {
            assert!(budget.admit(&control, next));
        }
        assert!(!budget.admit(&control, next));
        assert!(budget.admit(&read, next));
        assert!(!budget.admit(&control, next + Duration::from_secs(60)));
        assert!(budget.admit(&control, start + Duration::from_secs(3600)));
    }
}
