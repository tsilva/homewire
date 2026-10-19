//! Static CLI metadata and compatibility translations. No protocol behavior.
use homewire::{Result, device::Devices, error, naming};
use serde_json::{Value, json};

use Action::*;
use Arguments::*;
enum Arguments {
    Empty,
    Named(&'static str),
    Power(Option<&'static str>),
    Settings,
    Operation,
}
enum Action {
    Runtime,
    Tool(&'static str, Arguments),
    Standard,
}
pub struct Command {
    pub name: &'static str,
    pub usage: &'static str,
    pub family: &'static str,
    action: Action,
    description: &'static str,
}
macro_rules! commands {
    ($(($name:literal, $usage:literal, $family:literal, $action:expr, $description:literal)),+ $(,)?) => {
        const COMMANDS: &[Command] = &[$(Command { name:$name, usage:$usage, family:$family, action:$action, description:$description },)+];
    };
}
commands! {
    ("config-check", "", "", Runtime, "Validate adapter schemas/state without device I/O"),
    ("call", "TOOL JSON", "", Standard, "Use a standard capability tool from CLI"),
    ("certificate-sha256", "IP:PORT", "", Runtime, "Inspect a new TLS pin; confirm device identity"),
    ("daemon", "SOCKET", "", Runtime, "Persistent device owner; accepts local MCP clients"),
    ("bridge", "SOCKET", "", Runtime, "MCP stdio client for a running daemon"),
    ("tv-status", "", "tv", Tool("tv_status", Empty), "Read shared outlet and both media devices"),
    ("tv-set", "on|off", "tv", Tool("tv_apply", Power(None)), "Run configured shared-outlet sequence"),
    ("samsung-status", "", "samsung", Tool("samsung_status", Empty), "Read Samsung identity and remote state"),
    ("samsung-set", "on|off", "samsung", Tool("samsung_apply", Power(None)), "Wake or request Samsung shutdown"),
    ("samsung-key", "KEY", "samsung", Tool("samsung_key", Named("key")), "Send one Samsung remote key"),
    ("digi-status", "", "digi", Tool("digi_status", Empty), "Read DIGI identity and power state"),
    ("digi-set", "on|off", "digi", Tool("digi_apply", Power(None)), "Apply verified DIGI wake/sleep"),
    ("digi-key", "KEY", "digi", Tool("digi_key", Named("key")), "Send one DIGI remote key"),
    ("plugs", "", "plugs", Tool("plug_list", Empty), "List configured Tapo P100/P110 plugs"),
    ("plug-status", "NAME", "plugs", Tool("plug_status", Named("plug")), "Read relay, overload and P110 energy"),
    ("plug-set", "NAME on|off", "plugs", Tool("plug_apply", Power(Some("plug"))), "Apply and verify absolute relay power"),
    ("plug-check", "", "plugs", Tool("plug_check", Empty), "Poll plugs and retained overload faults"),
    ("watch-plugs", "", "plugs", Runtime, "Monitor reported overload events"),
    ("status", "", "meaco", Tool("meaco_status", Empty), "Read Meaco status"),
    ("plan", "day|night|off", "meaco", Tool("meaco_plan", Operation), "Preview a Meaco sequence"),
    ("apply", "day|night|off", "meaco", Tool("meaco_apply", Operation), "Apply and verify a Meaco sequence"),
    ("awair-status", "", "awair", Tool("awair_status", Empty), "Read fresh air-quality measurements"),
    ("printers", "", "printers", Tool("printer_list", Empty), "List configured printers"),
    ("printer-status", "NAME", "printers", Tool("printer_status", Named("printer")), "Read printer status and supplies"),
    ("printer-check", "", "printers", Tool("printer_check", Empty), "Poll printers and retained faults"),
    ("watch-printers", "", "printers", Runtime, "Monitor debounced printer fault events"),
    ("bulbs", "", "bulbs", Tool("bulb_list", Empty), "List configured lights (legacy bulb names)"),
    ("bulb-status", "NAME", "bulbs", Tool("bulb_status", Named("bulb")), "Read light state and overheating status"),
    ("bulb-set", "NAME JSON", "bulbs", Tool("bulb_apply", Settings), "Apply and verify absolute light settings"),
    ("bulb-check", "", "bulbs", Tool("bulb_check", Empty), "Poll lights and retained overheating faults"),
    ("watch-bulbs", "", "bulbs", Runtime, "Monitor debounced overheating events"),
    ("light-plan", "", "bulbs", Runtime, "Preview optional L900 curve and sunset"),
    ("light-schedule", "", "bulbs", Runtime, "Run optional L900 curve and sunset rules"),
    ("serve", "", "", Runtime, "MCP entities/routines; read-only by default"),
    ("schedule", "", "meaco", Runtime, "Run the explicitly configured daily Meaco rules"),
}
pub fn command(name: &str) -> Result<&'static Command> {
    let name = naming::canonical_cli(name);
    COMMANDS
        .iter()
        .find(|c| c.name == name)
        .ok_or_else(|| error("Unknown command"))
}
impl Command {
    pub fn validate(&self, args: &[&str]) -> Result<()> {
        if args.len() != 1 + self.usage.split_ascii_whitespace().count() {
            return Err(error("Unexpected or missing command arguments"));
        }
        Ok(())
    }
    pub fn execute(&self, devices: &mut Devices, args: &[&str], light_names: bool) -> Result<bool> {
        let value = match &self.action {
            Runtime => return Ok(false),
            Standard => {
                homewire::entity::call(devices, args[1], &serde_json::from_str::<Value>(args[2])?)?
            }
            Tool(tool, input) => {
                let value = match input {
                    Empty => json!({}),
                    Named(key) => json!({*key:args[1]}),
                    Power(key) => {
                        let index = 1 + usize::from(key.is_some());
                        let power = match args[index] {
                            "on" => true,
                            "off" => false,
                            _ => return Err(error("Power must be on or off")),
                        };
                        let mut value = json!({"power":power});
                        if let Some(key) = key {
                            value[*key] = json!(args[1]);
                        }
                        value
                    }
                    Settings => {
                        let change: homewire::bulb::Change = serde_json::from_str(args[2])
                            .map_err(|_| error("Invalid light settings JSON"))?;
                        change.validate()?;
                        json!({"bulb":args[1],"settings":change})
                    }
                    Operation => {
                        json!({"operation":args[1].parse::<homewire::meaco::Operation>()?})
                    }
                };
                devices.call(tool, &value)?
            }
        };
        println!(
            "{}",
            if light_names {
                naming::light_output(value)
            } else {
                value
            }
        );
        Ok(true)
    }
}
pub fn help() {
    println!(
        "Homewire — lightweight home automation\n\nUsage: homewire [--config HOME_JSON] [OPTIONS] COMMAND\n\nCommands:"
    );
    for c in COMMANDS {
        println!(
            "  {:24} {}",
            format!("{} {}", c.name, c.usage),
            c.description
        );
        for &(alias, canonical) in naming::CLI_ALIASES {
            if canonical == c.name {
                println!("  {:24} {}", format!("{alias} {}", c.usage), c.description);
            }
        }
    }
    print!("{OPTIONS_HELP}");
}

const OPTIONS_HELP: &str = "\nServer flags: --allow-control --legacy-tools --maintenance --schedule --light-schedule\n              --watch-meaco --watch-printers --watch-lights --watch-plugs\n--legacy-tools exposes original adapter tools.\n--maintenance permits direct power controls owned by the TV routine.\n\n--config HOME_JSON loads a version 1 unified configuration.\nLegacy Meaco JSON and per-family flags remain supported; do not mix formats.\n--adapter FAMILY=PATH loads a statically compiled adapter; may be repeated.\n--api API_JSON adds a bounded HTTPS dashboard/companion frontend to daemon.\n--watch-meaco enables independent Meaco fault monitoring.\n--connect IP:PORT overrides only the Meaco TCP destination.\nPrinter, Awair, light, plug and media configs may have connect overrides for testing.\nSchedule mode sends controls. Enable it only after disabling the two\ncorresponding Home Assistant rules. Fault monitoring sends no controls.\n";

pub fn config_family(option: &str) -> Option<&str> {
    let family = option.strip_prefix("--")?;
    [
        "printers", "awair", "lights", "bulbs", "plugs", "samsung", "digi", "tcl", "tv",
    ]
    .contains(&family)
    .then(|| naming::canonical_family(family))
}
