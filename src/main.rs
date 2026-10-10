use homewire::{
    Result,
    device::{Adapter, Devices, Monitored},
    error,
    fault::{self, diagnostic},
    mcp, meaco, naming,
};
use serde_json::{Value, json};
use std::{
    io::{self, BufReader},
    net::SocketAddr,
    path::PathBuf,
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

mod cli;

struct App {
    devices: Devices,
    scheduling: bool,
    light_scheduling: bool,
    next_meaco_schedule: Instant,
    next_light_schedule: Instant,
    next_history: Instant,
}
impl App {
    fn has_background(&self) -> bool {
        self.scheduling
            || self.light_scheduling
            || self.devices.alerts.is_some()
            || self.devices.history.is_some()
            || self.devices.next_background().is_some()
    }
    fn wait_duration(&self) -> Duration {
        let now = Instant::now();
        [
            self.devices.next_background(),
            self.scheduling.then_some(self.next_meaco_schedule),
            self.light_scheduling.then_some(self.next_light_schedule),
            self.devices
                .alerts
                .as_ref()
                .and_then(|a| a.next_delay(chrono::Utc::now().timestamp()))
                .map(|seconds| now + Duration::from_secs(seconds)),
            self.devices.history.is_some().then_some(self.next_history),
        ]
        .into_iter()
        .flatten()
        .map(|due| due.saturating_duration_since(now))
        .min()
        .unwrap_or(Duration::from_secs(60))
    }
    fn tick(&mut self) -> Result<()> {
        self.devices.background_tick();
        let mut alerts = self.devices.alerts.take();
        let result = self.tick_with_alerts(&mut alerts);
        self.devices.alerts = alerts;
        if result.is_ok()
            && Instant::now() >= self.next_history
            && let Some(mut history) = self.devices.history.take()
        {
            let samples = homewire::entity::call(
                &mut self.devices,
                "entity_status",
                &json!({"entities":history.config.entities}),
            );
            let mut error = None;
            if let Ok(samples) = samples {
                for state in samples["states"].as_array().into_iter().flatten() {
                    if state["available"] == true
                        && let (Some(entity), Some(value)) =
                            (state["entity"].as_str(), state["state"].as_f64())
                        && let Err(e) =
                            history.record(entity, chrono::Utc::now().timestamp(), value)
                    {
                        error = Some(e);
                        break;
                    }
                }
            }
            self.next_history =
                Instant::now() + Duration::from_secs(history.config.interval_seconds);
            self.devices.history = Some(history);
            if let Some(e) = error {
                eprintln!("History write failed: {e}");
            }
        }
        result
    }
    fn tick_with_alerts(&mut self, alerts: &mut Option<homewire::alerts::Manager>) -> Result<()> {
        if self.scheduling && Instant::now() >= self.next_meaco_schedule {
            let meaco = self
                .devices
                .meaco
                .as_mut()
                .ok_or_else(|| error("No Meaco configured"))?;
            if let Some(operation) = meaco.scheduled_operation(chrono::Utc::now())? {
                match meaco.execute("meaco_apply", &json!({"operation":operation})) {
                    Ok(result) => {
                        eprintln!("Schedule completed: {result}");
                        record_schedule(alerts, "schedule.meaco", false, "Meaco schedule", "");
                    }
                    Err(e) => {
                        eprintln!("Schedule failed (no automatic retry): {e}");
                        record_schedule(
                            alerts,
                            "schedule.meaco",
                            true,
                            "Meaco schedule failed",
                            &e.to_string(),
                        );
                    }
                }
            }
            self.next_meaco_schedule =
                Instant::now() + homewire::schedule::until_next_minute(chrono::Utc::now());
        }
        if self.light_scheduling && Instant::now() >= self.next_light_schedule {
            let bulbs = self
                .devices
                .bulbs
                .as_mut()
                .ok_or_else(|| error("No lights configured"))?;
            match bulbs.schedule_tick() {
                Ok(results) if !results.is_empty() => {
                    for result in results {
                        eprintln!(
                            "Light schedule completed: {}",
                            naming::light_output(serde_json::to_value(result)?)
                        );
                    }
                    record_schedule(alerts, "schedule.lights", false, "Light schedule", "");
                }
                Ok(_) => {}
                Err(e) => {
                    eprintln!("Light schedule failed (no immediate retry): {e}");
                    record_schedule(
                        alerts,
                        "schedule.lights",
                        true,
                        "Light schedule failed",
                        &e.to_string(),
                    );
                }
            }
            self.next_light_schedule =
                Instant::now() + Duration::from_secs(bulbs.config.poll_seconds);
        }
        if alerts
            .as_ref()
            .and_then(|a| a.next_delay(chrono::Utc::now().timestamp()))
            == Some(0)
        {
            let sent = alerts
                .as_mut()
                .map(|a| a.deliver(chrono::Utc::now().timestamp()));
            if let Some(Err(e)) = sent {
                eprintln!("{e}");
            }
        }
        Ok(())
    }
}
fn record_schedule(
    alerts: &mut Option<homewire::alerts::Manager>,
    key: &str,
    failed: bool,
    title: &str,
    message: &str,
) {
    if let Some(alerts) = alerts.as_mut()
        && let Err(e) = alerts.record(key, failed, title, message, chrono::Utc::now().timestamp())
    {
        eprintln!("Alert ledger write failed: {e}");
    }
}
fn report_watch<U: serde::Serialize, E: serde::Serialize>(
    check: fault::Watch<U, E>,
    label: &str,
    light_names: bool,
) -> Result<()> {
    for event in check.events {
        mcp::write_message(&mut io::stdout().lock(), &event)?;
    }
    for unavailable in check.unavailable {
        let prefix = format_args!("{label} unavailable");
        if light_names {
            diagnostic(
                prefix,
                &naming::light_output(serde_json::to_value(unavailable)?),
            )?;
        } else {
            diagnostic(prefix, &unavailable)?;
        }
    }
    Ok(())
}
fn watch_loop(device: &mut impl Monitored, label: &str, light_names: bool) -> Result<()> {
    loop {
        report_watch(device.watch()?, label, light_names)?;
        thread::sleep(Duration::from_secs(device.poll_seconds()));
    }
}

enum Input {
    Direct(BufReader<io::Stdin>),
    Background(mpsc::Receiver<Result<Option<String>>>),
}

fn run() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() || args.iter().any(|a| a == "--help" || a == "-h") {
        cli::help();
        return Ok(());
    }
    if args.first().is_some_and(|a| a == "bridge") {
        if args.len() != 2 {
            return Err(error("Usage: homewire bridge SOCKET"));
        }
        return homewire::ipc::bridge(std::path::Path::new(&args[1]));
    }
    if args.first().is_some_and(|a| a == "certificate-sha256") {
        if args.len() != 2 {
            return Err(error("Expected certificate-sha256 IP:PORT"));
        }
        let fingerprint = homewire::setup::certificate_sha256(args[1].parse()?)?;
        println!(
            "{}",
            json!({"sha256":fingerprint,"identity_verified":false,"note":"Explicit first-use inspection. Confirm the endpoint identity before saving this pin."})
        );
        return Ok(());
    }
    if args == ["--version"] {
        println!("homewire {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    let mut config_path = None;
    let mut api_path = None;
    let mut adapters = Vec::new();
    let mut legacy_paths: Vec<(&str, PathBuf)> = Vec::new();
    let mut light_names = false;
    let mut connect = None;
    let mut control = false;
    let mut scheduling = false;
    let mut light_scheduling = false;
    let mut watching = false;
    let mut watching_bulbs = false;
    let mut watching_plugs = false;
    let mut watch_meaco = false;
    let mut legacy_tools = false;
    let mut maintenance = false;
    let mut positional = vec![];
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--watch-meaco" => watch_meaco = true,
            "--api" => {
                api_path =
                    Some(PathBuf::from(iter.next().ok_or_else(|| {
                        error("--api needs a private JSON configuration")
                    })?))
            }
            "--adapter" => {
                let value = iter
                    .next()
                    .ok_or_else(|| error("--adapter requires FAMILY=PATH"))?;
                let (family, path) = value
                    .split_once('=')
                    .filter(|(f, p)| !f.is_empty() && !p.is_empty())
                    .ok_or_else(|| error("--adapter requires FAMILY=PATH"))?;
                light_names |= family == "lights";
                adapters.push((family, PathBuf::from(path)));
                if adapters.len() > 16 {
                    return Err(error("At most 16 adapter declarations"));
                }
            }
            "--legacy-tools" => legacy_tools = true,
            "--maintenance" => maintenance = true,
            "--config" => {
                config_path = Some(PathBuf::from(
                    iter.next()
                        .ok_or_else(|| error("--config requires a path"))?,
                ))
            }
            "--watch-plugs" => watching_plugs = true,
            flag if cli::config_family(flag).is_some() => {
                let family = cli::config_family(flag).expect("matched configuration flag");
                if family == "bulbs" {
                    if legacy_paths.iter().any(|(name, _)| *name == family) {
                        return Err(error("Use one --lights or --bulbs configuration path"));
                    }
                    light_names = flag == "--lights";
                }
                let path = iter
                    .next()
                    .ok_or_else(|| error(format!("{flag} requires a path")))?;
                if let Some((_, existing)) =
                    legacy_paths.iter_mut().find(|(name, _)| *name == family)
                {
                    *existing = PathBuf::from(path);
                } else {
                    legacy_paths.push((family, PathBuf::from(path)));
                }
            }
            "--connect" => {
                connect = Some(
                    iter.next()
                        .ok_or_else(|| error("--connect requires IP:PORT"))?
                        .parse::<SocketAddr>()?,
                )
            }
            "--allow-control" => control = true,
            "--schedule" => scheduling = true,
            "--light-schedule" => light_scheduling = true,
            "--watch-printers" => watching = true,
            "--watch-lights" | "--watch-bulbs" => watching_bulbs = true,
            a if a.starts_with('-') => return Err(error(format!("Unknown option: {a}"))),
            _ => positional.push(arg.as_str()),
        }
    }
    let raw_command = positional
        .first()
        .copied()
        .ok_or_else(|| error("Missing command"))?;
    let specification = cli::command(raw_command)?;
    let command = specification.name;
    let light_command = naming::CLI_ALIASES
        .iter()
        .any(|(alias, _)| *alias == raw_command);
    specification.validate(&positional)?;
    if api_path.is_some() && command != "daemon" {
        return Err(error("--api requires daemon"));
    }
    if (control
        || scheduling
        || light_scheduling
        || watching
        || watching_bulbs
        || watching_plugs
        || watch_meaco
        || legacy_tools)
        && !matches!(command, "serve" | "daemon")
    {
        return Err(error("Server flags belong to serve or daemon"));
    }
    if (scheduling || light_scheduling) && !control {
        return Err(error("Server schedules require --allow-control"));
    }
    let bundle = config_path
        .as_ref()
        .map(|p| homewire::bundle::Bundle::load(p))
        .transpose()?
        .flatten();
    if bundle.is_some() && (api_path.is_some() || !adapters.is_empty() || !legacy_paths.is_empty())
    {
        return Err(error(
            "Unified --config cannot be combined with per-family configuration flags",
        ));
    }
    if bundle.is_none() {
        if connect.is_some() && config_path.is_none() {
            return Err(error("Specify --config PATH for Meaco"));
        }
        if matches!(command, "serve" | "daemon")
            && config_path.is_none()
            && adapters.is_empty()
            && legacy_paths.is_empty()
        {
            return Err(error("Configure at least one device adapter for serve"));
        }
    }
    light_names |= bundle.is_some();
    let api_settings = match &bundle {
        Some(b) => b.api_settings()?,
        None => api_path
            .as_ref()
            .map(|p| homewire::api::Settings::from_source(&homewire::config::Source::file(p)))
            .transpose()?,
    };
    // API settings are validated for every command, but retained only by daemon.
    let api_settings = if command == "daemon" {
        api_settings
    } else {
        None
    };
    let mut devices = if let Some(b) = &bundle {
        b.open_devices(connect)?
    } else {
        Devices {
            meaco: config_path
                .as_ref()
                .map(|p| meaco::Manager::open_with_connect(p, connect))
                .transpose()?,
            ..Devices::default()
        }
    };
    for (family, path) in legacy_paths.into_iter().chain(adapters) {
        devices.open_adapter(naming::canonical_family(family), &path)?;
    }
    if !specification.family.is_empty() && !devices.configured(specification.family) {
        return Err(error(format!("{} is not configured", specification.family)));
    }
    // The root JSON and resolved temporary secrets leave memory before serving.
    let _configuration_lock = bundle.map(homewire::bundle::Bundle::into_lock);
    devices.maintenance = maintenance;
    if (scheduling || command == "schedule")
        && !devices
            .meaco
            .as_ref()
            .is_some_and(meaco::Manager::has_schedule)
    {
        return Err(error("Configure Meaco schedule and presets"));
    }
    if (light_scheduling || matches!(command, "light-plan" | "light-schedule"))
        && devices
            .bulbs
            .as_ref()
            .and_then(|b| b.config.automation.as_ref())
            .is_none()
    {
        return Err(error("Configure lights automation"));
    }
    for (enabled, family) in [
        (watch_meaco, "meaco"),
        (watching, "printers"),
        (watching_bulbs, "bulbs"),
        (watching_plugs, "plugs"),
    ] {
        if enabled {
            devices.enable_monitoring(family)?;
        }
    }
    let mut app = App {
        devices,
        scheduling: scheduling || command == "schedule",
        light_scheduling: light_scheduling || command == "light-schedule",
        next_meaco_schedule: Instant::now(),
        next_light_schedule: Instant::now(),
        next_history: Instant::now(),
    };
    app.devices.validate()?;
    let selection = app.devices.selection();
    let options = mcp::Options {
        light_names,
        allow_control: control,
    };
    if specification.execute(&mut app.devices, &positional, light_command)? {
        return Ok(());
    }
    match command {
        "config-check" => println!("{}", json!({"valid":true,"device_io":false})),
        "watch-plugs" => {
            let device = app
                .devices
                .plugs()
                .ok_or_else(|| error("No plugs configured"))?;
            watch_loop(device, "Plug", false)?;
        }

        "light-plan" => println!(
            "{}",
            app.devices
                .bulbs
                .as_ref()
                .and_then(|b| b.config.automation.as_ref())
                .ok_or_else(|| error("No light automation configured"))?
                .preview(chrono::Utc::now())?
        ),
        "light-schedule" => loop {
            app.tick()?;
            thread::sleep(app.wait_duration());
        },
        "watch-bulbs" => {
            let device = app
                .devices
                .bulbs
                .as_mut()
                .ok_or_else(|| error("No bulbs configured"))?;
            watch_loop(device, "Light", light_command)?;
        }

        "watch-printers" => {
            let device = app
                .devices
                .printers
                .as_mut()
                .ok_or_else(|| error("No printers configured"))?;
            watch_loop(device, "Printer", false)?;
        }

        "schedule" => loop {
            app.tick()?;
            thread::sleep(app.wait_duration());
        },
        "daemon" => {
            let server = homewire::ipc::Server::bind(std::path::Path::new(positional[1]))?;
            let background = app.has_background();
            let _api = api_settings
                .map(|settings| {
                    homewire::api::Server::start_settings(
                        settings,
                        std::path::Path::new(positional[1]),
                    )
                })
                .transpose()?;
            eprintln!("Homewire daemon ready");
            loop {
                if background {
                    app.tick()?;
                }
                let stream = if background {
                    match server.incoming.recv_timeout(app.wait_duration()) {
                        Ok(stream) => stream,
                        Err(mpsc::RecvTimeoutError::Timeout) => continue,
                        Err(mpsc::RecvTimeoutError::Disconnected) => {
                            return Err(error("IPC listener stopped"));
                        }
                    }
                } else {
                    server
                        .incoming
                        .recv()
                        .map_err(|_| error("IPC listener stopped"))?
                };
                let mut stream = stream;
                if let Err(e) = homewire::ipc::exchange(&mut stream, |request| {
                    if request["method"] == "homewire/api" {
                        return homewire::api::dispatch(&mut app.devices, request, control);
                    }
                    if legacy_tools {
                        mcp::handle_selected(request, selection, options, |name, args| {
                            app.devices.call(name, args)
                        })
                    } else {
                        mcp::handle_entities(request, &mut app.devices, control)
                    }
                }) {
                    eprintln!("IPC request failed (no retry): {e}");
                }
            }
        }
        "serve" => {
            let background = app.has_background();
            // Idle MCP needs no worker, queue or periodic wakeup.
            let mut input = if background {
                let (sender, receiver) = mpsc::sync_channel(1);
                thread::Builder::new()
                    .name("mcp-stdin".into())
                    .stack_size(128 * 1024)
                    .spawn(move || {
                        let mut reader = BufReader::with_capacity(1024, io::stdin());
                        loop {
                            let item = mcp::read_message(&mut reader);
                            let finished = !matches!(&item, Ok(Some(_)));
                            if sender.send(item).is_err() || finished {
                                break;
                            }
                        }
                    })?;
                Input::Background(receiver)
            } else {
                Input::Direct(BufReader::with_capacity(1024, io::stdin()))
            };
            let mut stdout = io::stdout().lock();
            let mut initialized = false;
            loop {
                // Handshake must not wait behind printer network timeouts or controls.
                if background && initialized {
                    app.tick()?;
                }
                let line = match &mut input {
                    Input::Direct(reader) => mcp::read_message(reader)?,
                    Input::Background(receiver) => {
                        if !initialized {
                            match receiver.recv() {
                                Ok(item) => item?,
                                Err(_) => break,
                            }
                        } else {
                            match receiver.recv_timeout(app.wait_duration()) {
                                Ok(item) => item?,
                                Err(mpsc::RecvTimeoutError::Disconnected) => break,
                                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                            }
                        }
                    }
                };
                let Some(line) = line else {
                    break;
                };
                let parsed = serde_json::from_str::<Value>(&line);
                drop(line);
                let response = match parsed {
                    Ok(request) => {
                        if request.get("jsonrpc") == Some(&json!("2.0"))
                            && request.get("method") == Some(&json!("notifications/initialized"))
                            && request.get("id").is_none()
                        {
                            initialized = true;
                        }
                        if legacy_tools {
                            mcp::handle_selected(request, selection, options, |name, args| {
                                app.devices.call(name, args)
                            })
                        } else {
                            mcp::handle_entities(request, &mut app.devices, control)
                        }
                    }
                    Err(_) => Some(mcp::rpc_error(Value::Null, -32700, "Invalid JSON")),
                };
                if let Some(response) = response {
                    mcp::write_message(&mut stdout, &response)?;
                }
            }
        }
        _ => return Err(error("Unknown command")),
    }
    Ok(())
}
fn main() {
    if let Err(e) = run() {
        eprintln!("homewire: {e}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn receiver_waits_for_earliest_enabled_job_and_overdue_jobs_do_not_sleep() {
        let now = Instant::now();
        let mut app = App {
            devices: Devices::default(),
            scheduling: true,
            light_scheduling: false,
            next_meaco_schedule: now + Duration::from_secs(50),
            next_light_schedule: now + Duration::from_secs(5),
            next_history: now,
        };
        let delay = app.wait_duration();
        assert!(delay > Duration::from_secs(49) && delay <= Duration::from_secs(50));
        app.light_scheduling = true;
        let delay = app.wait_duration();
        assert!(delay > Duration::from_secs(4) && delay <= Duration::from_secs(5));
        app.next_light_schedule = now - Duration::from_secs(1);
        assert_eq!(app.wait_duration(), Duration::ZERO);
    }
}
