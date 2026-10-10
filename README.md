<p align="center">
  <img src="logo.png" alt="Homewire" width="280" /><br />
  <!-- repo-tagline:start -->
  <strong>🏠 Local home automation with verified device control 🔌</strong>
  <!-- repo-tagline:end -->
</p>

Homewire is for people who want to read and control home devices on their local
network, preview changes, and run daily automations from a terminal. One Rust
executable contains the device adapters and routines; it needs no Home Assistant,
Docker, Python, Node, or model runtime. Agents such as ZeroClaw can use the same
devices through MCP (Model Context Protocol), with controls enabled explicitly.

## Quickstart

Requires **Rust 1.89 or newer**, Git, and network access to a supported device.
This first example reads a Brother HL-L2350DW or HP DeskJet 3630 over IPP without
credentials or print jobs. Queries can wake a sleeping printer.

```sh
git clone https://github.com/tsilva/homewire.git
cd homewire
cargo build --release --locked
mkdir -p .local
chmod 700 .local
```

Create `.local/home.json` with the following content, replacing `192.0.2.10` with
your printer's LAN IP address. The address below is a documentation placeholder.

```json
{
  "version": 1,
  "adapters": {
    "printers": {"printers": [{"name": "printer", "host": "192.0.2.10"}]}
  }
}
```

Protect the file, validate it without contacting devices, then read live status:

```sh
chmod 600 .local/home.json
target/release/homewire --config .local/home.json config-check
target/release/homewire --config .local/home.json printer-status printer
```

Validation returns `{"device_io":false,"valid":true}`. The status command returns
JSON with `printer`, `state`, `reasons` and `supplies`; an idle printer reports
`"state":"idle"`. Missing supply readings remain unknown. Failures go to stderr
with a nonzero exit code.

For another device, follow its setup guide below. Add only the sections you need
from [the unified configuration example](examples/home.json); replace all
credential, address and location placeholders. There is no automatic discovery.
Keep real settings in ignored `.local/` files and credentials in
[Infisical references](docs/configuration.md#infisical-configuration-references).
See [configuration and migration](docs/configuration.md#unified-configuration) for
schemas, state files and legacy installations.

## Devices and routines

| Device / setup guide | What you can do |
| --- | --- |
| [Meaco Arete Two](docs/configuration.md#configuration) | Read status; preview and run configured day, night and off routines |
| [Brother HL-L2350DW / HP DeskJet 3630](docs/devices.md#printers) | Read status and supplies; monitor confirmed faults and low-supply warnings |
| [Awair Element](docs/devices.md#awair-element) | Read fresh temperature, humidity, CO₂, TVOC, PM2.5 and score |
| [Tapo L430C / L900](docs/devices.md#l900-led-strip) | Set power, brightness, colour and white temperature; monitor overheating |
| [Tapo P100 / P110](docs/devices.md#tapo-p100p110-plugs) | Control relays; read P110 energy and monitor overloads |
| [Samsung UE40H6200 / DIGI R2A](docs/devices.md#samsung-tv-and-digi-r2a) | Use paired remotes and coordinated shared-outlet TV routines |
| [Computers](docs/devices.md#computer-shutdown-service) | Wake configured targets; use authenticated shutdown and optional metered outlet cutoff |

Controls check identity where supported and verify reported outcomes; uncertain
results stop execution. Generic [Wake-on-LAN](docs/devices.md#wake-on-lan)
cannot confirm a successful wake, and Samsung shutdown may remain unverified.
Each adapter guide explains its prerequisites and limits.

## Agents and automation

Start a read-only MCP server for your client:

```sh
target/release/homewire --config .local/home.json serve
```

Only configured capabilities appear. Add `--allow-control` to expose write tools.
[MCP and ZeroClaw setup](docs/automation.md#mcp-and-zeroclaw) covers discovery,
fresh reads, routine previews and execution. For persistent ownership across
client sessions, use [daemon and bridge](docs/automation.md#persistent-daemon-and-mcp-bridge).

[Schedules](docs/automation.md#scheduling) and
[L900 curves / sunset rules](docs/devices.md#l900-scheduling) are opt-in.
**Disable corresponding Home Assistant rules before enabling replacement controls.**
Keep one configuration path per device; Homewire's locks cannot coordinate with
manufacturer apps or other controllers. Use a trusted LAN.

## Further reading

- [CLI usage](docs/configuration.md#command-line-usage): status, plans and explicit controls.
- [Household services](docs/services.md#household-services): Cast/DLNA playback, alerts, history, weather and shopping.
- [Android deployment](docs/android.md#android-deployment): Termux installation, supervision, verification and rollback.
- [Dashboard and native companion](docs/services.md#native-android-companion): HTTPS controls, telemetry and notifications.
- [Implementation and development](docs/development.md#implementation-and-validation): adapter design, checks and measured memory limits.

[MIT license](LICENSE)
