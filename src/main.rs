use homewire::{
    Result, awair, bulb,
    device::{Adapter, Devices, Monitored},
    error, fault, mcp,
    meaco::{self, Operation},
    naming, plug, printer,
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

const HELP: &str = "Homewire — lightweight home automation\n\nUsage: homewire [--config MEACO_JSON] [--printers PRINTERS_JSON] [--awair AWAIR_JSON] [--lights LIGHTS_JSON] [--plugs PLUGS_JSON] COMMAND\n\nCommands:\n  plugs                   List configured Tapo P100/P110 plugs\n  plug-status NAME        Read relay, overload and P110 energy\n  plug-set NAME on|off    Apply and verify absolute relay power\n  plug-check              Poll plugs and retained overload faults\n  watch-plugs             Monitor reported overload events\n  status                  Read Meaco status\n  plan day|night|off       Preview a Meaco sequence\n  apply day|night|off      Apply and verify a Meaco sequence\n  awair-status            Read fresh air-quality measurements\n  printers                List configured printers\n  printer-status NAME     Read printer status and supplies\n  printer-check           Poll printers and retained faults\n  watch-printers          Monitor debounced printer fault events\n  lights                  List configured Tapo bulbs and strips\n  light-status NAME       Read light state and overheating\n  light-set NAME JSON     Apply and verify absolute settings\n  light-check             Poll lights and retained faults\n  light-plan              Preview optional L900 curve and sunset\n  light-schedule          Run optional L900 curve and sunset rules\n  watch-lights            Monitor overheating events\n  bulbs                   Legacy bulb commands (also --bulbs)\n  bulb-status NAME        Read bulb state and overheating status\n  bulb-set NAME JSON      Apply and verify absolute bulb settings\n  bulb-check              Poll bulbs and retained overheating faults\n  watch-bulbs             Monitor debounced overheating events\n  serve [--allow-control] [--schedule] [--light-schedule] [--watch-printers] [--watch-lights] [--watch-plugs]\n                          MCP stdio; read-only by default\n  schedule                Run the daily 09:00/22:00 London Meaco rules\n\n--connect IP:PORT overrides only the Meaco TCP destination.\nPrinter, Awair, light and plug configs may have connect overrides for testing.\nSchedule mode sends controls. Enable it only after disabling the two\ncorresponding Home Assistant rules. Fault monitoring sends no controls.\n";

struct App {
    devices: Devices,
    next_plug_poll: Instant,
    next_bulb_poll: Instant,
    next_light_schedule: Instant,
    next_printer_poll: Instant,
}
impl App {
    fn tick(
        &mut self,
        scheduling: bool,
        watching: bool,
        watching_bulbs: bool,
        light_scheduling: bool,
        watching_plugs: bool,
    ) -> Result<()> {
        if scheduling {
            let meaco = self
                .devices
                .meaco
                .as_mut()
                .ok_or_else(|| error("No Meaco configured"))?;
            if let Some(operation) = meaco.scheduled_operation(chrono::Utc::now())? {
                match meaco.execute("meaco_apply", &json!({"operation":operation})) {
                    Ok(result) => eprintln!("Schedule completed: {result}"),
                    Err(e) => eprintln!("Schedule failed (no automatic retry): {e}"),
                }
            }
        }
        if light_scheduling && Instant::now() >= self.next_light_schedule {
            let bulbs = self
                .devices
                .bulbs
                .as_mut()
                .ok_or_else(|| error("No lights configured"))?;
            match bulbs.schedule_tick() {
                Ok(Some(result)) => eprintln!(
                    "Light schedule completed: {}",
                    naming::light_output(serde_json::to_value(result)?)
                ),
                Ok(None) => {}
                Err(e) => eprintln!("Light schedule failed (no immediate retry): {e}"),
            }
            self.next_light_schedule =
                Instant::now() + Duration::from_secs(bulbs.config.poll_seconds);
        }
        if watching_plugs && Instant::now() >= self.next_plug_poll {
            let plugs = self
                .devices
                .plugs
                .as_mut()
                .ok_or_else(|| error("No plugs configured"))?;
            poll_watch(plugs, &mut self.next_plug_poll, "Plug")?;
        }
        if watching && Instant::now() >= self.next_printer_poll {
            let printers = self
                .devices
                .printers
                .as_mut()
                .ok_or_else(|| error("No printers configured"))?;
            poll_watch(printers, &mut self.next_printer_poll, "Printer")?;
        }
        if watching_bulbs && Instant::now() >= self.next_bulb_poll {
            let bulbs = self
                .devices
                .bulbs
                .as_mut()
                .ok_or_else(|| error("No bulbs configured"))?;
            poll_watch(bulbs, &mut self.next_bulb_poll, "Bulb")?;
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
fn poll_watch(device: &mut impl Monitored, next: &mut Instant, label: &str) -> Result<()> {
    let check = device.watch()?;
    *next = Instant::now() + Duration::from_secs(device.poll_seconds());
    report_watch(check, label, WatchOutput::Diagnostics)
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
    if args == ["--version"] {
        println!("homewire {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    let mut config_path = None;
    let mut printers_path = None;
    let mut awair_path = None;
    let mut bulbs_path = None;
    let mut plugs_path = None;
    let mut light_names = false;
    let mut connect = None;
    let mut control = false;
    let mut scheduling = false;
    let mut light_scheduling = false;
    let mut watching = false;
    let mut watching_bulbs = false;
    let mut watching_plugs = false;
    let mut positional = vec![];
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
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
        "status"
            | "plan"
            | "apply"
            | "serve"
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
    let expected = if matches!(command, "bulb-set" | "plug-set") {
        3
    } else if matches!(
        command,
        "plan" | "apply" | "printer-status" | "bulb-status" | "plug-status"
    ) {
        2
    } else {
        1
    };
    if positional.len() != expected {
        return Err(error("Unexpected or missing command arguments"));
    }
    if (control || scheduling || light_scheduling || watching || watching_bulbs || watching_plugs)
        && command != "serve"
    {
        return Err(error("Server flags belong to serve"));
    }
    if (scheduling || light_scheduling) && !control {
        return Err(error("serve schedules require --allow-control"));
    }
    let needs_meaco = matches!(command, "status" | "plan" | "apply" | "schedule") || scheduling;
    let needs_printers = matches!(
        command,
        "printers" | "printer-status" | "printer-check" | "watch-printers"
    ) || watching;
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
    {
        return Err(error("Specify --plugs PATH"));
    }
    if command == "awair-status" && awair_path.is_none() {
        return Err(error("Specify --awair PATH"));
    }
    if command == "serve"
        && config_path.is_none()
        && printers_path.is_none()
        && awair_path.is_none()
        && bulbs_path.is_none()
        && plugs_path.is_none()
    {
        return Err(error(
            "Configure a Meaco, printers, Awair, lights, or plugs for serve",
        ));
    }
    let meaco = config_path
        .map(|path| meaco::Manager::open_with_connect(&path, connect))
        .transpose()?;
    let printers = printers_path
        .map(|path| printer::Manager::open(&path))
        .transpose()?;
    let first_printer_poll = Instant::now()
        + Duration::from_secs(
            printers
                .as_ref()
                .map(|p| p.config.poll_seconds)
                .unwrap_or(30),
        );
    let awair = awair_path
        .map(|path| awair::Config::open(&path))
        .transpose()?;
    let bulbs = bulbs_path
        .map(|path| bulb::Manager::open(&path))
        .transpose()?;
    let first_bulb_poll = Instant::now()
        + Duration::from_secs(bulbs.as_ref().map(|b| b.config.poll_seconds).unwrap_or(30));
    if (light_scheduling || matches!(command, "light-plan" | "light-schedule"))
        && bulbs
            .as_ref()
            .and_then(|b| b.config.automation.as_ref())
            .is_none()
    {
        return Err(error("Configure automation in the lights JSON"));
    }
    let plugs = plugs_path
        .map(|path| plug::Manager::open(&path))
        .transpose()?;
    let first_plug_poll = Instant::now()
        + Duration::from_secs(plugs.as_ref().map(|p| p.config.poll_seconds).unwrap_or(30));
    let mut app = App {
        devices: Devices {
            meaco,
            printers,
            awair,
            bulbs,
            plugs,
        },
        next_plug_poll: first_plug_poll,
        next_bulb_poll: first_bulb_poll,
        next_light_schedule: Instant::now(),
        next_printer_poll: first_printer_poll,
    };
    let selection = app.devices.selection();
    let options = mcp::Options {
        light_names,
        allow_control: control,
    };
    match command {
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
                .plugs
                .as_mut()
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
            thread::sleep(Duration::from_secs(1));
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
            thread::sleep(Duration::from_secs(1));
        },
        "serve" => {
            let background =
                scheduling || watching || watching_bulbs || light_scheduling || watching_plugs;
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
                if background {
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
                        match receiver.recv_timeout(Duration::from_secs(1)) {
                            Ok(item) => item?,
                            Err(mpsc::RecvTimeoutError::Disconnected) => break,
                            Err(mpsc::RecvTimeoutError::Timeout) => continue,
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
                        mcp::handle_selected(request, selection, options, |name, args| {
                            app.devices.call(name, args)
                        })
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
