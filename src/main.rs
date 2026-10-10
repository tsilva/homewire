use homewire::{
    Result, androidtv, awair, bulb,
    device::{Adapter, Devices, Monitored},
    error, fault, mcp,
    meaco::{self, Operation},
    naming, plug, printer, samsung, tv,
};
use serde_json::{Value, json};
use std::{
    io::{self, BufReader, Write},
    net::SocketAddr,
    path::PathBuf,
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

const HELP: &str = "Homewire — lightweight home automation\n\nUsage: homewire [--config HOME_JSON] [--printers PRINTERS_JSON] [--awair AWAIR_JSON] [--lights LIGHTS_JSON] [--plugs PLUGS_JSON] [--samsung SAMSUNG_JSON] [--digi DIGI_JSON] [--tv TV_JSON] COMMAND\n\nCommands:\n  config-check            Validate adapter schemas/state without device I/O\n  call TOOL JSON        Use a standard capability tool from CLI\n  certificate-sha256 IP:PORT  Inspect a new TLS pin; confirm device identity\n  daemon SOCKET          Persistent device owner; accepts local MCP clients\n  bridge SOCKET          MCP stdio client for a running daemon\n  tv-status               Read shared outlet and both media devices\n  tv-set on|off           Run configured shared-outlet sequence\n  samsung-status          Read Samsung identity and remote state\n  samsung-set on|off      Wake or request Samsung shutdown\n  samsung-key KEY         Send one Samsung remote key\n  digi-status             Read DIGI identity and power state\n  digi-set on|off         Apply verified DIGI wake/sleep\n  digi-key KEY            Send one DIGI remote key\n  plugs                   List configured Tapo P100/P110 plugs\n  plug-status NAME        Read relay, overload and P110 energy\n  plug-set NAME on|off    Apply and verify absolute relay power\n  plug-check              Poll plugs and retained overload faults\n  watch-plugs             Monitor reported overload events\n  status                  Read Meaco status\n  plan day|night|off       Preview a Meaco sequence\n  apply day|night|off      Apply and verify a Meaco sequence\n  awair-status            Read fresh air-quality measurements\n  printers                List configured printers\n  printer-status NAME     Read printer status and supplies\n  printer-check           Poll printers and retained faults\n  watch-printers          Monitor debounced printer fault events\n  lights                  List configured Tapo bulbs and strips\n  light-status NAME       Read light state and overheating\n  light-set NAME JSON     Apply and verify absolute settings\n  light-check             Poll lights and retained faults\n  light-plan              Preview optional L900 curve and sunset\n  light-schedule          Run optional L900 curve and sunset rules\n  watch-lights            Monitor overheating events\n  bulbs                   Legacy bulb commands (also --bulbs)\n  bulb-status NAME        Read bulb state and overheating status\n  bulb-set NAME JSON      Apply and verify absolute bulb settings\n  bulb-check              Poll bulbs and retained overheating faults\n  watch-bulbs             Monitor debounced overheating events\n  serve [--allow-control] [--legacy-tools] [--maintenance] [--schedule] [--light-schedule] [--watch-printers] [--watch-lights] [--watch-plugs]\n                          MCP entities/routines; read-only by default\n--legacy-tools exposes original adapter tools.\n--maintenance permits direct power controls owned by the TV routine.\n  schedule                Run the explicitly configured daily Meaco rules\n\n--config HOME_JSON loads a version 1 unified configuration.\nLegacy Meaco JSON and per-family flags remain supported; do not mix formats.\n--adapter FAMILY=PATH loads a statically compiled adapter; may be repeated.\n--api API_JSON adds a bounded HTTPS dashboard/companion frontend to daemon.\n--watch-meaco enables independent Meaco fault monitoring.\n--connect IP:PORT overrides only the Meaco TCP destination.\nPrinter, Awair, light, plug and media configs may have connect overrides for testing.\nSchedule mode sends controls. Enable it only after disabling the two\ncorresponding Home Assistant rules. Fault monitoring sends no controls.\n";

struct App {
    devices: Devices,
    next_meaco_schedule: Instant,
    next_plug_poll: Instant,
    next_bulb_poll: Instant,
    next_light_schedule: Instant,
    next_printer_poll: Instant,
    watch_meaco: bool,
    next_meaco_poll: Instant,
    next_history: Instant,
}
impl App {
    fn wait_duration(
        &self,
        scheduling: bool,
        watching: bool,
        watching_bulbs: bool,
        light_scheduling: bool,
        watching_plugs: bool,
    ) -> Duration {
        let now = Instant::now();
        [
            self.devices.next_background(),
            scheduling.then_some(self.next_meaco_schedule),
            watching.then_some(self.next_printer_poll),
            watching_bulbs.then_some(self.next_bulb_poll),
            light_scheduling.then_some(self.next_light_schedule),
            watching_plugs.then_some(self.next_plug_poll),
            self.watch_meaco.then_some(self.next_meaco_poll),
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
    fn tick(
        &mut self,
        scheduling: bool,
        watching: bool,
        watching_bulbs: bool,
        light_scheduling: bool,
        watching_plugs: bool,
    ) -> Result<()> {
        self.devices.background_tick();
        let mut alerts = self.devices.alerts.take();
        let result = self.tick_with_alerts(
            scheduling,
            watching,
            watching_bulbs,
            light_scheduling,
            watching_plugs,
            &mut alerts,
        );
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
    fn tick_with_alerts(
        &mut self,
        scheduling: bool,
        watching: bool,
        watching_bulbs: bool,
        light_scheduling: bool,
        watching_plugs: bool,
        alerts: &mut Option<homewire::alerts::Manager>,
    ) -> Result<()> {
        if self.watch_meaco
            && Instant::now() >= self.next_meaco_poll
            && let Some(meaco) = self.devices.meaco.as_mut()
        {
            poll_watch(meaco, &mut self.next_meaco_poll, "meaco", alerts.as_mut())?;
        }
        if scheduling && Instant::now() >= self.next_meaco_schedule {
            let meaco = self
                .devices
                .meaco
                .as_mut()
                .ok_or_else(|| error("No Meaco configured"))?;
            if let Some(operation) = meaco.scheduled_operation(chrono::Utc::now())? {
                match meaco.execute("meaco_apply", &json!({"operation":operation})) {
                    Ok(result) => {
                        eprintln!("Schedule completed: {result}");
                        if let Some(alerts) = alerts.as_mut()
                            && let Err(e) = alerts.record(
                                "schedule.meaco",
                                false,
                                "Meaco schedule",
                                "",
                                chrono::Utc::now().timestamp(),
                            )
                        {
                            eprintln!("Alert ledger write failed: {e}");
                        }
                    }
                    Err(e) => {
                        eprintln!("Schedule failed (no automatic retry): {e}");
                        if let Some(alerts) = alerts.as_mut()
                            && let Err(e) = alerts.record(
                                "schedule.meaco",
                                true,
                                "Meaco schedule failed",
                                &e.to_string(),
                                chrono::Utc::now().timestamp(),
                            )
                        {
                            eprintln!("Alert ledger write failed: {e}");
                        }
                    }
                }
            }
            self.next_meaco_schedule =
                Instant::now() + homewire::schedule::until_next_minute(chrono::Utc::now());
        }
        if light_scheduling && Instant::now() >= self.next_light_schedule {
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
                    if let Some(alerts) = alerts.as_mut()
                        && let Err(e) = alerts.record(
                            "schedule.lights",
                            false,
                            "Light schedule",
                            "",
                            chrono::Utc::now().timestamp(),
                        )
                    {
                        eprintln!("Alert ledger write failed: {e}");
                    }
                }
                Ok(_) => {}
                Err(e) => {
                    eprintln!("Light schedule failed (no immediate retry): {e}");
                    if let Some(alerts) = alerts.as_mut()
                        && let Err(e) = alerts.record(
                            "schedule.lights",
                            true,
                            "Light schedule failed",
                            &e.to_string(),
                            chrono::Utc::now().timestamp(),
                        )
                    {
                        eprintln!("Alert ledger write failed: {e}");
                    }
                }
            }
            self.next_light_schedule =
                Instant::now() + Duration::from_secs(bulbs.config.poll_seconds);
        }
        if watching_plugs && Instant::now() >= self.next_plug_poll {
            let plugs = self
                .devices
                .plugs()
                .ok_or_else(|| error("No plugs configured"))?;
            poll_watch(plugs, &mut self.next_plug_poll, "plugs", alerts.as_mut())?;
        }
        if watching && Instant::now() >= self.next_printer_poll {
            let printers = self
                .devices
                .printers
                .as_mut()
                .ok_or_else(|| error("No printers configured"))?;
            poll_watch(
                printers,
                &mut self.next_printer_poll,
                "printers",
                alerts.as_mut(),
            )?;
        }
        if watching_bulbs && Instant::now() >= self.next_bulb_poll {
            let bulbs = self
                .devices
                .bulbs
                .as_mut()
                .ok_or_else(|| error("No bulbs configured"))?;
            poll_watch(bulbs, &mut self.next_bulb_poll, "lights", alerts.as_mut())?;
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
enum WatchOutput {
    Diagnostics,
    Events { light_names: bool },
}
fn report_watch<U: serde::Serialize, E: serde::Serialize>(
    check: fault::Watch<U, E>,
    label: &str,
    output: WatchOutput,
) -> Result<()> {
    for event in check.events {
        match output {
            WatchOutput::Diagnostics => diagnostic(format_args!("{label} event"), &event)?,
            WatchOutput::Events { .. } => mcp::write_message(&mut io::stdout().lock(), &event)?,
        }
    }
    for unavailable in check.unavailable {
        let prefix = format_args!("{label} unavailable");
        if matches!(output, WatchOutput::Events { light_names: true }) {
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
fn poll_watch(
    device: &mut impl Monitored,
    next: &mut Instant,
    label: &str,
    mut alerts: Option<&mut homewire::alerts::Manager>,
) -> Result<()> {
    *next = Instant::now() + Duration::from_secs(device.poll_seconds());
    let display = match label {
        "printers" => "Printer",
        "lights" => "Light",
        "plugs" => "Plug",
        "meaco" => "Meaco",
        s => s,
    };
    let check = match device.watch() {
        Ok(check) => check,
        Err(e) => {
            eprintln!("{display} monitor failed: {e}");
            return Ok(());
        }
    };
    let mut failed = None;
    device.active_faults(&mut |name, faults| {
        if failed.is_none()
            && let Some(alerts) = alerts.as_mut()
        {
            failed = alerts
                .reconcile(label, name, faults, chrono::Utc::now().timestamp())
                .err();
        }
    });
    if let Some(e) = failed {
        eprintln!("Alert ledger write failed: {e}");
    }
    report_watch(check, display, WatchOutput::Diagnostics)
}
fn watch_loop(device: &mut impl Monitored, label: &str, light_names: bool) -> Result<()> {
    loop {
        report_watch(device.watch()?, label, WatchOutput::Events { light_names })?;
        thread::sleep(Duration::from_secs(device.poll_seconds()));
    }
}
fn diagnostic(label: impl std::fmt::Display, value: &impl serde::Serialize) -> Result<()> {
    let mut stderr = io::stderr().lock();
    write!(stderr, "{label}: ")?;
    mcp::write_message(&mut stderr, value)
}

enum Input {
    Direct(BufReader<io::Stdin>),
    Background(mpsc::Receiver<Result<Option<String>>>),
}

fn run() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() || args.iter().any(|a| a == "--help" || a == "-h") {
        print!("{HELP}");
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
    let mut printers_path = None;
    let mut awair_path = None;
    let mut bulbs_path = None;
    let mut plugs_path = None;
    let mut samsung_path = None;
    let mut digi_path = None;
    let mut tv_path = None;
    let mut api_path = None;
    let mut adapters = Vec::new();
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
            "--plugs" => {
                plugs_path = Some(PathBuf::from(
                    iter.next()
                        .ok_or_else(|| error("--plugs requires a path"))?,
                ))
            }
            "--tv" => {
                tv_path = Some(PathBuf::from(
                    iter.next().ok_or_else(|| error("--tv requires a path"))?,
                ))
            }
            "--samsung" => {
                samsung_path = Some(PathBuf::from(
                    iter.next()
                        .ok_or_else(|| error("--samsung requires a path"))?,
                ))
            }
            "--digi" => {
                digi_path = Some(PathBuf::from(
                    iter.next().ok_or_else(|| error("--digi requires a path"))?,
                ))
            }
            "--watch-plugs" => watching_plugs = true,
            "--printers" => {
                printers_path = Some(PathBuf::from(
                    iter.next()
                        .ok_or_else(|| error("--printers requires a path"))?,
                ))
            }
            "--awair" => {
                awair_path = Some(PathBuf::from(
                    iter.next()
                        .ok_or_else(|| error("--awair requires a path"))?,
                ))
            }
            "--lights" | "--bulbs" => {
                if bulbs_path.is_some() {
                    return Err(error("Use one --lights or --bulbs configuration path"));
                }
                light_names = arg == "--lights";
                bulbs_path = Some(PathBuf::from(
                    iter.next()
                        .ok_or_else(|| error("--lights/--bulbs requires a path"))?,
                ))
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
    let command = match raw_command {
        "lights" => "bulbs",
        "light-status" => "bulb-status",
        "light-set" => "bulb-set",
        "light-check" => "bulb-check",
        "watch-lights" => "watch-bulbs",
        other => other,
    };
    let light_command = raw_command.starts_with("light") || raw_command == "watch-lights";
    if !matches!(
        command,
        "config-check"
            | "tv-status"
            | "tv-set"
            | "samsung-status"
            | "samsung-set"
            | "samsung-key"
            | "digi-status"
            | "digi-set"
            | "digi-key"
            | "status"
            | "plan"
            | "apply"
            | "serve"
            | "daemon"
            | "schedule"
            | "light-plan"
            | "light-schedule"
            | "awair-status"
            | "plugs"
            | "plug-status"
            | "plug-set"
            | "plug-check"
            | "watch-plugs"
            | "printers"
            | "printer-status"
            | "call"
            | "printer-check"
            | "watch-printers"
            | "bulbs"
            | "bulb-status"
            | "bulb-set"
            | "bulb-check"
            | "watch-bulbs"
    ) {
        return Err(error("Unknown command"));
    }
    if api_path.is_some() && command != "daemon" {
        return Err(error("--api requires daemon"));
    }
    let expected = if matches!(command, "bulb-set" | "plug-set" | "call") {
        3
    } else if matches!(
        command,
        "daemon"
            | "tv-set"
            | "samsung-set"
            | "samsung-key"
            | "digi-set"
            | "digi-key"
            | "plan"
            | "apply"
            | "printer-status"
            | "bulb-status"
            | "plug-status"
    ) {
        2
    } else {
        1
    };
    if positional.len() != expected {
        return Err(error("Unexpected or missing command arguments"));
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
    let needs_meaco =
        matches!(command, "status" | "plan" | "apply" | "schedule") || scheduling || watch_meaco;
    let needs_printers = matches!(
        command,
        "printers" | "printer-status" | "printer-check" | "watch-printers"
    ) || watching;
    let bundle = config_path
        .as_ref()
        .map(|p| homewire::bundle::Bundle::load(p))
        .transpose()?
        .flatten();
    if bundle.is_some()
        && (printers_path.is_some()
            || awair_path.is_some()
            || bulbs_path.is_some()
            || plugs_path.is_some()
            || samsung_path.is_some()
            || digi_path.is_some()
            || tv_path.is_some()
            || api_path.is_some()
            || !adapters.is_empty())
    {
        return Err(error(
            "Unified --config cannot be combined with per-family configuration flags",
        ));
    }
    if bundle.is_none() {
        if needs_meaco && config_path.is_none() || connect.is_some() && config_path.is_none() {
            return Err(error("Specify --config PATH for Meaco"));
        }
        if needs_printers && printers_path.is_none() {
            return Err(error("Specify --printers PATH"));
        }
        if (matches!(
            command,
            "bulbs"
                | "bulb-status"
                | "bulb-set"
                | "bulb-check"
                | "watch-bulbs"
                | "light-plan"
                | "light-schedule"
        ) || watching_bulbs
            || light_scheduling)
            && bulbs_path.is_none()
        {
            return Err(error("Specify --lights or --bulbs PATH"));
        }
        if (matches!(
            command,
            "plugs" | "plug-status" | "plug-set" | "plug-check" | "watch-plugs"
        ) || watching_plugs)
            && plugs_path.is_none()
            && tv_path.is_none()
            && adapters.is_empty()
        {
            return Err(error("Specify --plugs or --tv PATH"));
        }
        if command.starts_with("tv-") && tv_path.is_none() {
            return Err(error("Specify --tv PATH"));
        }
        if command.starts_with("samsung-") && samsung_path.is_none() && tv_path.is_none() {
            return Err(error("Specify --samsung PATH"));
        }
        if command.starts_with("digi-") && digi_path.is_none() && tv_path.is_none() {
            return Err(error("Specify --digi PATH"));
        }
        if command == "awair-status" && awair_path.is_none() {
            return Err(error("Specify --awair PATH"));
        }
        if matches!(command, "serve" | "daemon")
            && config_path.is_none()
            && printers_path.is_none()
            && awair_path.is_none()
            && bulbs_path.is_none()
            && plugs_path.is_none()
            && samsung_path.is_none()
            && digi_path.is_none()
            && tv_path.is_none()
            && adapters.is_empty()
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
            printers: printers_path
                .as_ref()
                .map(|p| printer::Manager::open(p))
                .transpose()?,
            awair: awair_path
                .as_ref()
                .map(|p| awair::Config::open(p))
                .transpose()?,
            bulbs: bulbs_path
                .as_ref()
                .map(|p| bulb::Manager::open(p))
                .transpose()?,
            plugs: plugs_path
                .as_ref()
                .map(|p| plug::Manager::open(p))
                .transpose()?,
            samsung: samsung_path
                .as_ref()
                .map(|p| samsung::Manager::open(p))
                .transpose()?,
            digi: digi_path
                .as_ref()
                .map(|p| androidtv::Manager::open(p))
                .transpose()?,
            tv: tv_path.as_ref().map(|p| tv::Manager::open(p)).transpose()?,
            ..Devices::default()
        }
    };
    // The root JSON and resolved temporary secrets leave memory before serving.
    let _configuration_lock = bundle.map(homewire::bundle::Bundle::into_lock);
    devices.maintenance = maintenance;
    if needs_meaco && devices.meaco.is_none() {
        return Err(error("Meaco is not configured"));
    }
    if needs_printers && devices.printers.is_none() {
        return Err(error("Printers are not configured"));
    }
    if watching_bulbs && devices.bulbs.is_none() {
        return Err(error("Lights are not configured"));
    }
    if watching_plugs && devices.plugs().is_none() {
        return Err(error("Plugs are not configured"));
    }
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
    let first_printer_poll = Instant::now()
        + Duration::from_secs(
            devices
                .printers
                .as_ref()
                .map(|p| p.config.poll_seconds)
                .unwrap_or(30),
        );
    let first_bulb_poll = Instant::now()
        + Duration::from_secs(
            devices
                .bulbs
                .as_ref()
                .map(|p| p.config.poll_seconds)
                .unwrap_or(30),
        );
    let first_plug_poll = Instant::now()
        + Duration::from_secs(
            devices
                .plugs
                .as_ref()
                .map(|p| p.config.poll_seconds)
                .unwrap_or(30),
        );
    let mut app = App {
        devices,
        next_meaco_schedule: Instant::now(),
        next_plug_poll: first_plug_poll,
        next_bulb_poll: first_bulb_poll,
        next_light_schedule: Instant::now(),
        next_printer_poll: first_printer_poll,
        watch_meaco,
        next_meaco_poll: Instant::now() + Duration::from_secs(30),
        next_history: Instant::now(),
    };
    for (family, path) in adapters {
        app.devices.open_adapter(family, &path)?;
    }
    if let Some(plugs) = app.devices.plugs() {
        app.next_plug_poll = Instant::now() + Duration::from_secs(plugs.config.poll_seconds);
    }
    app.devices.validate()?;
    let selection = app.devices.selection();
    let options = mcp::Options {
        light_names,
        allow_control: control,
    };
    match command {
        "config-check" => println!("{}", json!({"valid":true,"device_io":false})),
        "tv-status" => println!("{}", app.devices.call("tv_status", &json!({}))?),
        "tv-set" => {
            let power = match positional[1] {
                "on" => true,
                "off" => false,
                _ => return Err(error("TV power must be on or off")),
            };
            println!("{}", app.devices.call("tv_apply", &json!({"power":power}))?);
        }
        "samsung-status" | "digi-status" => println!(
            "{}",
            app.devices.call(
                if command == "samsung-status" {
                    "samsung_status"
                } else {
                    "digi_status"
                },
                &json!({})
            )?
        ),
        "samsung-set" | "digi-set" => {
            let power = match positional[1] {
                "on" => true,
                "off" => false,
                _ => return Err(error("Media power must be on or off")),
            };
            println!(
                "{}",
                app.devices.call(
                    if command == "samsung-set" {
                        "samsung_apply"
                    } else {
                        "digi_apply"
                    },
                    &json!({"power":power})
                )?
            );
        }
        "samsung-key" | "digi-key" => println!(
            "{}",
            app.devices.call(
                if command == "samsung-key" {
                    "samsung_key"
                } else {
                    "digi_key"
                },
                &json!({"key":positional[1]})
            )?
        ),
        "call" => println!(
            "{}",
            homewire::entity::call(
                &mut app.devices,
                positional[1],
                &serde_json::from_str::<Value>(positional[2])?
            )?
        ),
        "plugs" => println!("{}", app.devices.call("plug_list", &json!({}))?),
        "plug-status" => println!(
            "{}",
            app.devices
                .call("plug_status", &json!({"plug":positional[1]}))?
        ),
        "plug-check" => println!("{}", app.devices.call("plug_check", &json!({}))?),
        "plug-set" => {
            let power = match positional[2] {
                "on" => true,
                "off" => false,
                _ => return Err(error("Plug power must be on or off")),
            };
            println!(
                "{}",
                app.devices
                    .call("plug_apply", &json!({"plug":positional[1],"power":power}))?
            );
        }
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
            app.tick(false, false, false, true, false)?;
            thread::sleep(app.wait_duration(false, false, false, true, false));
        },
        "status" => println!("{}", app.devices.call("meaco_status", &json!({}))?),
        "plan" | "apply" => {
            let operation: Operation = positional[1].parse()?;
            println!(
                "{}",
                app.devices.call(
                    if command == "plan" {
                        "meaco_plan"
                    } else {
                        "meaco_apply"
                    },
                    &json!({"operation":operation})
                )?
            );
        }
        "awair-status" => println!("{}", app.devices.call("awair_status", &json!({}))?),
        "bulbs" | "bulb-status" | "bulb-set" | "bulb-check" => {
            let (tool, args) = match command {
                "bulbs" => ("bulb_list", json!({})),
                "bulb-status" => ("bulb_status", json!({"bulb":positional[1]})),
                "bulb-set" => {
                    let change: bulb::Change = serde_json::from_str(positional[2])
                        .map_err(|_| error("Invalid light settings JSON"))?;
                    change.validate()?;
                    (
                        "bulb_apply",
                        json!({"bulb":positional[1],"settings":change}),
                    )
                }
                _ => ("bulb_check", json!({})),
            };
            let value = app.devices.call(tool, &args)?;
            println!(
                "{}",
                if light_command {
                    naming::light_output(value)
                } else {
                    value
                }
            );
        }
        "watch-bulbs" => {
            let device = app
                .devices
                .bulbs
                .as_mut()
                .ok_or_else(|| error("No bulbs configured"))?;
            watch_loop(device, "Light", light_command)?;
        }

        "printers" => println!("{}", app.devices.call("printer_list", &json!({}))?),
        "printer-status" => println!(
            "{}",
            app.devices
                .call("printer_status", &json!({"printer":positional[1]}))?
        ),
        "printer-check" => println!("{}", app.devices.call("printer_check", &json!({}))?),
        "watch-printers" => {
            let device = app
                .devices
                .printers
                .as_mut()
                .ok_or_else(|| error("No printers configured"))?;
            watch_loop(device, "Printer", false)?;
        }

        "schedule" => loop {
            app.tick(true, false, false, false, false)?;
            thread::sleep(app.wait_duration(true, false, false, false, false));
        },
        "daemon" => {
            let server = homewire::ipc::Server::bind(std::path::Path::new(positional[1]))?;
            let background = scheduling
                || watching
                || watching_bulbs
                || light_scheduling
                || watching_plugs
                || watch_meaco
                || app.devices.alerts.is_some()
                || app.devices.history.is_some()
                || app.devices.next_background().is_some();
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
                    app.tick(
                        scheduling,
                        watching,
                        watching_bulbs,
                        light_scheduling,
                        watching_plugs,
                    )?;
                }
                let stream = if background {
                    match server.incoming.recv_timeout(app.wait_duration(
                        scheduling,
                        watching,
                        watching_bulbs,
                        light_scheduling,
                        watching_plugs,
                    )) {
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
            let background = scheduling
                || watching
                || watching_bulbs
                || light_scheduling
                || watching_plugs
                || watch_meaco
                || app.devices.alerts.is_some()
                || app.devices.history.is_some()
                || app.devices.next_background().is_some();
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
                    app.tick(
                        scheduling && initialized,
                        watching && initialized,
                        watching_bulbs && initialized,
                        light_scheduling && initialized,
                        watching_plugs && initialized,
                    )?;
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
                            match receiver.recv_timeout(app.wait_duration(
                                scheduling,
                                watching,
                                watching_bulbs,
                                light_scheduling,
                                watching_plugs,
                            )) {
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
            next_meaco_schedule: now + Duration::from_secs(50),
            next_printer_poll: now + Duration::from_secs(30),
            next_bulb_poll: now + Duration::from_secs(20),
            next_plug_poll: now + Duration::from_secs(10),
            next_light_schedule: now + Duration::from_secs(5),
            watch_meaco: false,
            next_meaco_poll: now,
            next_history: now,
        };
        let delay = app.wait_duration(true, true, false, false, false);
        assert!(delay > Duration::from_secs(29) && delay <= Duration::from_secs(30));
        let delay = app.wait_duration(true, true, true, true, true);
        assert!(delay > Duration::from_secs(4) && delay <= Duration::from_secs(5));
        app.next_plug_poll = now - Duration::from_secs(1);
        assert_eq!(
            app.wait_duration(false, false, false, false, true),
            Duration::ZERO
        );
    }
}
