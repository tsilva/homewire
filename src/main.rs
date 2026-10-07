use homewire::{
    Result, awair, bulb,
    config::{Config, Memory, Store},
    error, mcp,
    meaco::{self, Device, NetworkDevice, Operation},
    plug, printer, schedule,
    tuya::Client,
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

const HELP: &str = "Homewire — lightweight home automation\n\nUsage: homewire [--config MEACO_JSON] [--printers PRINTERS_JSON] [--awair AWAIR_JSON] [--lights LIGHTS_JSON] [--plugs PLUGS_JSON] COMMAND\n\nCommands:\n  plugs                   List configured Tapo P100/P110 plugs\n  plug-status NAME        Read relay, overload and P110 energy\n  plug-set NAME on|off    Apply and verify absolute relay power\n  plug-check              Poll plugs and retained overload faults\n  watch-plugs             Monitor reported overload events\n  status                  Read Meaco status\n  plan day|night|off       Preview a Meaco sequence\n  apply day|night|off      Apply and verify a Meaco sequence\n  awair-status            Read fresh air-quality measurements\n  printers                List configured printers\n  printer-status NAME     Read printer status and supplies\n  printer-check           Poll printers and retained faults\n  watch-printers          Monitor debounced printer fault events\n  lights                  List configured Tapo bulbs and strips\n  light-status NAME       Read light state and overheating\n  light-set NAME JSON     Apply and verify absolute settings\n  light-check             Poll lights and retained faults\n  light-plan              Preview optional L900 curve and sunset\n  light-schedule          Run optional L900 curve and sunset rules\n  watch-lights            Monitor overheating events\n  bulbs                   Legacy bulb commands (also --bulbs)\n  bulb-status NAME        Read bulb state and overheating status\n  bulb-set NAME JSON      Apply and verify absolute bulb settings\n  bulb-check              Poll bulbs and retained overheating faults\n  watch-bulbs             Monitor debounced overheating events\n  serve [--allow-control] [--schedule] [--light-schedule] [--watch-printers] [--watch-lights] [--watch-plugs]\n                          MCP stdio; read-only by default\n  schedule                Run the daily 09:00/22:00 London Meaco rules\n\n--connect IP:PORT overrides only the Meaco TCP destination.\nPrinter, Awair, light and plug configs may have connect overrides for testing.\nSchedule mode sends controls. Enable it only after disabling the two\ncorresponding Home Assistant rules. Fault monitoring sends no controls.\n";

struct Meaco {
    device: NetworkDevice,
    memory: Memory,
    store: Store,
}
impl Meaco {
    fn call(&mut self, name: &str, operation: Option<Operation>) -> Result<Value> {
        match name {
            "meaco_status" => Ok(serde_json::to_value(self.device.read()?)?),
            "meaco_plan" => {
                let op = operation.ok_or_else(|| error("Missing operation"))?;
                let state = self.device.read()?;
                Ok(
                    json!({"operation":op,"state":state,"steps":meaco::plan(&state,op,&self.memory),"settle_seconds":self.device.settle_seconds,
                    "note":"Provisional plan; execution rechecks state after every command."}),
                )
            }
            "meaco_apply" => {
                let op = operation.ok_or_else(|| error("Missing operation"))?;
                Ok(serde_json::to_value(meaco::apply(
                    &mut self.device,
                    op,
                    &mut self.memory,
                    |memory| self.store.save(memory),
                )?)?)
            }
            _ => Err(error("Unknown operation")),
        }
    }
    fn tick(&mut self) -> Result<()> {
        if let Some((operation, name, date)) =
            schedule::due(chrono::Utc::now(), &self.memory.schedule_attempts)
        {
            // Record the attempt first: restarts or failures cannot replay a toggle.
            self.memory.schedule_attempts.insert(name, date);
            self.store.save(&self.memory)?;
            match self.call("meaco_apply", Some(operation)) {
                Ok(result) => eprintln!("Schedule completed: {result}"),
                Err(e) => eprintln!("Schedule failed (no automatic retry): {e}"),
            }
        }
        Ok(())
    }
}
struct App {
    meaco: Option<Meaco>,
    printers: Option<printer::Manager>,
    awair: Option<awair::Config>,
    bulbs: Option<bulb::Manager>,
    plugs: Option<plug::Manager>,
    next_plug_poll: Instant,
    next_bulb_poll: Instant,
    next_light_schedule: Instant,
    next_printer_poll: Instant,
}
impl App {
    fn call(&mut self, name: &str, args: &Value) -> Result<Value> {
        if name.starts_with("meaco_") {
            return self
                .meaco
                .as_mut()
                .ok_or_else(|| error("No Meaco configured"))?
                .call(
                    name,
                    args.get("operation")
                        .and_then(Value::as_str)
                        .and_then(|s| s.parse().ok()),
                );
        }
        if name == "awair_status" {
            return Ok(serde_json::to_value(
                self.awair
                    .as_ref()
                    .ok_or_else(|| error("No Awair configured"))?
                    .read()?,
            )?);
        }
        if name.starts_with("plug_") {
            let plugs = self
                .plugs
                .as_mut()
                .ok_or_else(|| error("No plugs configured"))?;
            return match name {
                "plug_list" => Ok(plugs.list()),
                "plug_status" => Ok(serde_json::to_value(
                    plugs.status(
                        args.get("plug")
                            .and_then(Value::as_str)
                            .ok_or_else(|| error("Missing plug name"))?,
                    )?,
                )?),
                "plug_apply" => Ok(serde_json::to_value(
                    plugs.apply(
                        args.get("plug")
                            .and_then(Value::as_str)
                            .ok_or_else(|| error("Missing plug name"))?,
                        args.get("power")
                            .and_then(Value::as_bool)
                            .ok_or_else(|| error("Specify absolute plug power"))?,
                    )?,
                )?),
                "plug_check" => Ok(serde_json::to_value(plugs.check()?)?),
                _ => Err(error("Unknown plug operation")),
            };
        }
        if name.starts_with("bulb_") {
            let bulbs = self
                .bulbs
                .as_mut()
                .ok_or_else(|| error("No bulbs configured"))?;
            let bulb_name = args.get("bulb").and_then(Value::as_str).unwrap_or_default();
            return match name {
                "bulb_list" => Ok(bulbs.list()),
                "bulb_status" => Ok(serde_json::to_value(bulbs.status(bulb_name)?)?),
                "bulb_apply" => {
                    let change: bulb::Change = serde_json::from_value(
                        args.get("settings").cloned().unwrap_or(Value::Null),
                    )
                    .map_err(|_| error("Invalid bulb settings"))?;
                    Ok(serde_json::to_value(bulbs.apply(bulb_name, &change)?)?)
                }
                "bulb_check" => Ok(serde_json::to_value(bulbs.check()?)?),
                _ => Err(error("Unknown bulb operation")),
            };
        }
        let printers = self
            .printers
            .as_mut()
            .ok_or_else(|| error("No printers configured"))?;
        match name {
            "printer_list" => Ok(json!({"printers":printers.config.printers})),
            "printer_status" => Ok(serde_json::to_value(
                printers.status(
                    args.get("printer")
                        .and_then(Value::as_str)
                        .ok_or_else(|| error("Missing printer name"))?,
                )?,
            )?),
            "printer_check" => Ok(serde_json::to_value(printers.check()?)?),
            _ => Err(error("Unknown operation")),
        }
    }
    fn tick(
        &mut self,
        scheduling: bool,
        watching: bool,
        watching_bulbs: bool,
        light_scheduling: bool,
        watching_plugs: bool,
    ) -> Result<()> {
        if scheduling {
            self.meaco
                .as_mut()
                .ok_or_else(|| error("No Meaco configured"))?
                .tick()?;
        }
        if light_scheduling && Instant::now() >= self.next_light_schedule {
            let bulbs = self
                .bulbs
                .as_mut()
                .ok_or_else(|| error("No lights configured"))?;
            match bulbs.schedule_tick() {
                Ok(Some(result)) => eprintln!(
                    "Light schedule completed: {}",
                    bulb::light_output(serde_json::to_value(result)?)
                ),
                Ok(None) => {}
                Err(e) => eprintln!("Light schedule failed (no immediate retry): {e}"),
            }
            self.next_light_schedule =
                Instant::now() + Duration::from_secs(bulbs.config.poll_seconds);
        }
        if watching_plugs && Instant::now() >= self.next_plug_poll {
            let plugs = self
                .plugs
                .as_mut()
                .ok_or_else(|| error("No plugs configured"))?;
            let check = plugs.check()?;
            self.next_plug_poll = Instant::now() + Duration::from_secs(plugs.config.poll_seconds);
            for event in check.events {
                eprintln!("Plug event: {}", serde_json::to_string(&event)?);
            }
            for unavailable in check.unavailable {
                eprintln!("Plug unavailable: {}", serde_json::to_string(&unavailable)?);
            }
        }
        if watching && Instant::now() >= self.next_printer_poll {
            let printers = self
                .printers
                .as_mut()
                .ok_or_else(|| error("No printers configured"))?;
            let check = printers.check()?;
            self.next_printer_poll =
                Instant::now() + Duration::from_secs(printers.config.poll_seconds);
            // Stderr preserves the MCP stdout transport; no outbound notifications.
            for event in check.events {
                eprintln!("Printer event: {}", serde_json::to_string(&event)?);
            }
            for unavailable in check.unavailable {
                eprintln!(
                    "Printer unavailable: {}",
                    serde_json::to_string(&unavailable)?
                );
            }
        }
        if watching_bulbs && Instant::now() >= self.next_bulb_poll {
            let bulbs = self
                .bulbs
                .as_mut()
                .ok_or_else(|| error("No bulbs configured"))?;
            let check = bulbs.check()?;
            self.next_bulb_poll = Instant::now() + Duration::from_secs(bulbs.config.poll_seconds);
            for event in check.events {
                eprintln!("Bulb event: {}", serde_json::to_string(&event)?);
            }
            for unavailable in check.unavailable {
                eprintln!("Bulb unavailable: {}", serde_json::to_string(&unavailable)?);
            }
        }
        Ok(())
    }
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
    let meaco = if let Some(path) = config_path {
        let config = Config::load(&path)?;
        let store = Store::open(&path, &config.device_id)?;
        let memory = store.read()?;
        Some(Meaco {
            device: NetworkDevice {
                client: Client::new(&config, connect)?,
                settle_seconds: config.settle_seconds,
            },
            memory,
            store,
        })
    } else {
        None
    };
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
        .map(|path| awair::Config::load(&path))
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
    let tools = mcp::ToolSet {
        meaco: meaco.is_some(),
        printers: printers.is_some(),
        awair: awair.is_some(),
        bulbs: bulbs.is_some(),
        plugs: plugs.is_some(),
        light_names,
        allow_control: control,
    };
    let mut app = App {
        meaco,
        printers,
        awair,
        bulbs,
        plugs,
        next_plug_poll: first_plug_poll,
        next_bulb_poll: first_bulb_poll,
        next_light_schedule: Instant::now(),
        next_printer_poll: first_printer_poll,
    };
    match command {
        "plugs" => println!("{}", app.call("plug_list", &json!({}))?),
        "plug-status" => println!(
            "{}",
            app.call("plug_status", &json!({"plug":positional[1]}))?
        ),
        "plug-check" => println!("{}", app.call("plug_check", &json!({}))?),
        "plug-set" => {
            let power = match positional[2] {
                "on" => true,
                "off" => false,
                _ => return Err(error("Plug power must be on or off")),
            };
            println!(
                "{}",
                app.call("plug_apply", &json!({"plug":positional[1],"power":power}))?
            );
        }
        "watch-plugs" => loop {
            let plugs = app
                .plugs
                .as_mut()
                .ok_or_else(|| error("No plugs configured"))?;
            let check = plugs.check()?;
            for event in check.events {
                println!("{}", serde_json::to_string(&event)?);
            }
            for unavailable in check.unavailable {
                eprintln!("Plug unavailable: {}", serde_json::to_string(&unavailable)?);
            }
            thread::sleep(Duration::from_secs(plugs.config.poll_seconds));
        },
        "light-plan" => println!(
            "{}",
            app.bulbs
                .as_ref()
                .and_then(|b| b.config.automation.as_ref())
                .ok_or_else(|| error("No light automation configured"))?
                .preview(chrono::Utc::now())?
        ),
        "light-schedule" => loop {
            app.tick(false, false, false, true, false)?;
            thread::sleep(Duration::from_secs(1));
        },
        "status" => println!("{}", app.call("meaco_status", &json!({}))?),
        "plan" | "apply" => {
            let operation: Operation = positional[1].parse()?;
            println!(
                "{}",
                app.call(
                    if command == "plan" {
                        "meaco_plan"
                    } else {
                        "meaco_apply"
                    },
                    &json!({"operation":operation})
                )?
            );
        }
        "awair-status" => println!("{}", app.call("awair_status", &json!({}))?),
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
            let value = app.call(tool, &args)?;
            println!(
                "{}",
                if light_command {
                    bulb::light_output(value)
                } else {
                    value
                }
            );
        }
        "watch-bulbs" => loop {
            let bulbs = app
                .bulbs
                .as_mut()
                .ok_or_else(|| error("No bulbs configured"))?;
            let check = bulbs.check()?;
            for event in check.events {
                println!("{}", serde_json::to_string(&event)?);
            }
            for unavailable in check.unavailable {
                let value = serde_json::to_value(unavailable)?;
                let value = if light_command {
                    bulb::light_output(value)
                } else {
                    value
                };
                eprintln!("Light unavailable: {value}");
            }
            thread::sleep(Duration::from_secs(bulbs.config.poll_seconds));
        },
        "printers" => println!("{}", app.call("printer_list", &json!({}))?),
        "printer-status" => println!(
            "{}",
            app.call("printer_status", &json!({"printer":positional[1]}))?
        ),
        "printer-check" => println!("{}", app.call("printer_check", &json!({}))?),
        "watch-printers" => loop {
            let printers = app
                .printers
                .as_mut()
                .ok_or_else(|| error("No printers configured"))?;
            let check = printers.check()?;
            for event in check.events {
                println!("{}", serde_json::to_string(&event)?);
            }
            for unavailable in check.unavailable {
                eprintln!(
                    "Printer unavailable: {}",
                    serde_json::to_string(&unavailable)?
                );
            }
            thread::sleep(Duration::from_secs(printers.config.poll_seconds));
        },
        "schedule" => loop {
            app.tick(true, false, false, false, false)?;
            thread::sleep(Duration::from_secs(1));
        },
        "serve" => {
            let (sender, receiver) = mpsc::sync_channel(1);
            thread::Builder::new()
                .name("mcp-stdin".into())
                .stack_size(128 * 1024)
                .spawn(move || {
                    let mut reader = BufReader::new(io::stdin());
                    loop {
                        let item = mcp::read_message(&mut reader);
                        let finished = !matches!(&item, Ok(Some(_)));
                        if sender.send(item).is_err() || finished {
                            break;
                        }
                    }
                })?;
            let mut stdout = io::stdout().lock();
            let mut initialized = false;
            loop {
                // Handshake must not wait behind printer network timeouts or controls.
                app.tick(
                    scheduling && initialized,
                    watching && initialized,
                    watching_bulbs && initialized,
                    light_scheduling && initialized,
                    watching_plugs && initialized,
                )?;
                match receiver.recv_timeout(Duration::from_secs(1)) {
                    Ok(Ok(Some(line))) => {
                        let response = match serde_json::from_str::<Value>(&line) {
                            Ok(request) => {
                                if request.get("jsonrpc") == Some(&json!("2.0"))
                                    && request.get("method")
                                        == Some(&json!("notifications/initialized"))
                                    && request.get("id").is_none()
                                {
                                    initialized = true;
                                }
                                mcp::handle_configured(request, tools, |name, args| {
                                    app.call(name, args)
                                })
                            }
                            Err(_) => Some(mcp::rpc_error(Value::Null, -32700, "Invalid JSON")),
                        };
                        if let Some(response) = response {
                            mcp::write_message(&mut stdout, &response)?;
                        }
                    }
                    Ok(Err(e)) => return Err(e),
                    Ok(Ok(None)) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
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
