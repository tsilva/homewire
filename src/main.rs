use homewire::{
    Result,
    config::{Config, Memory, Store},
    error, mcp,
    meaco::{self, Device, NetworkDevice, Operation},
    schedule,
    tuya::Client,
};
use serde_json::{Value, json};
use std::{
    io::{self, BufReader},
    net::SocketAddr,
    path::PathBuf,
    sync::mpsc,
    thread,
    time::Duration,
};

const HELP: &str = "Homewire — lightweight home automation\n\nUsage: homewire --config PATH [--connect IP:PORT] COMMAND\n\nCommands:\n  status                   Read Meaco status\n  plan day|night|off       Preview a sequence without controls\n  apply day|night|off      Apply and verify a sequence\n  serve [--allow-control] [--schedule]\n                           MCP stdio; read-only by default\n  schedule                 Run the daily 09:00/22:00 London rules\n\nSchedule mode sends controls. Enable it only after disabling the two\ncorresponding Home Assistant rules. No late catch-up runs are performed.\n";
struct App {
    device: NetworkDevice,
    memory: Memory,
    store: Store,
}
impl App {
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
    let mut connect = None;
    let mut control = false;
    let mut scheduling = false;
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
            "--connect" => {
                connect = Some(
                    iter.next()
                        .ok_or_else(|| error("--connect requires IP:PORT"))?
                        .parse::<SocketAddr>()?,
                )
            }
            "--allow-control" => control = true,
            "--schedule" => scheduling = true,
            a if a.starts_with('-') => return Err(error("Unknown option")),
            _ => positional.push(arg.as_str()),
        }
    }
    let command = positional
        .first()
        .copied()
        .ok_or_else(|| error("Missing command"))?;
    if !matches!(command, "status" | "plan" | "apply" | "serve" | "schedule") {
        return Err(error("Unknown command"));
    }
    let expected = if matches!(command, "plan" | "apply") {
        2
    } else {
        1
    };
    if positional.len() != expected {
        return Err(error("Unexpected or missing command arguments"));
    }
    if (control || scheduling) && command != "serve" {
        return Err(error("--allow-control and --schedule belong to serve"));
    }
    if scheduling && !control {
        return Err(error("serve --schedule requires --allow-control"));
    }
    let config_path = config_path.ok_or_else(|| error("Specify --config PATH"))?;
    let config = Config::load(&config_path)?;
    let store = Store::open(&config_path, &config.device_id)?;
    let memory = store.read()?;
    let device = NetworkDevice {
        client: Client::new(&config, connect)?,
        settle_seconds: config.settle_seconds,
    };
    let mut app = App {
        device,
        memory,
        store,
    };
    match command {
        "status" => println!("{}", app.call("meaco_status", None)?),
        "plan" | "apply" => println!(
            "{}",
            app.call(
                if command == "plan" {
                    "meaco_plan"
                } else {
                    "meaco_apply"
                },
                Some(positional[1].parse()?)
            )?
        ),
        "schedule" => loop {
            app.tick()?;
            thread::sleep(Duration::from_secs(1));
        },
        "serve" => {
            // One bounded reader thread; the main thread owns all state and controls.
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
            loop {
                if scheduling {
                    app.tick()?;
                }
                match receiver.recv_timeout(Duration::from_secs(1)) {
                    Ok(Ok(Some(line))) => {
                        let response = match serde_json::from_str(&line) {
                            Ok(request) => {
                                mcp::handle(request, control, |name, op| app.call(name, op))
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
