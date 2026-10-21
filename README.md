<p align="center">
  <img src="logo.png" alt="Homewire" width="280" /><br />
  <!-- repo-tagline:start -->
  <strong>🏠 Local home automation with verified device control 🔌</strong>
  <!-- repo-tagline:end -->
</p>

Homewire reads and controls home devices on your local network and runs daily
automations from a terminal. One Rust executable contains the adapters and
routines, with no Home Assistant, container or interpreter required at runtime.
Agents can use the same devices through MCP (Model Context Protocol); controls
and schedules are opt-in.

## Quickstart

Requires **Rust 1.89+**, Git, and network access to a supported device. This
example reads a Brother HL-L2350DW or HP DeskJet 3630 over IPP without credentials
or print jobs. Queries can wake a sleeping printer.

```sh
git clone https://github.com/tsilva/homewire.git
cd homewire
cargo build --release --locked
mkdir -p .local
chmod 700 .local
```

Create `.local/home.json`, replacing the documentation address with your printer's
LAN IP:

```json
{
  "version": 1,
  "adapters": {
    "printers": {"printers": [{"name": "printer", "host": "192.0.2.10"}]}
  }
}
```

Validate without contacting devices, then read live status:

```sh
chmod 600 .local/home.json
target/release/homewire --config .local/home.json config-check
target/release/homewire --config .local/home.json printer-status printer
```

Validation returns `{"device_io":false,"valid":true}`. Status returns JSON with
`printer`, `state`, `reasons` and `supplies`; an idle printer reports
`"state":"idle"`. Missing readings remain unknown. Failures go to stderr with a
nonzero exit code.

For other devices, copy only the sections you need from
[examples/home.json](examples/home.json) and follow the setup guides below.
Replace all placeholders, keep real settings in ignored `.local/` files, and use
[Infisical references](docs/configuration.md#infisical-configuration-references)
for credentials. There is no automatic discovery.

## Devices and routines

| Setup guide | Capabilities |
| --- | --- |
| [Meaco Arete Two](docs/configuration.md#configuration) | Status, purification/sleep controls and day/night/off routines |
| [Brother HL-L2350DW / HP DeskJet 3630](docs/devices.md#printers) | Status, supplies and confirmed fault warnings |
| [Awair Element](docs/devices.md#awair-element) | Fresh temperature, humidity, CO₂, TVOC, PM2.5 and score |
| [Tapo L430C / L900](docs/devices.md#tapo-l430c-bulbs) | Light controls, overheating monitoring and opt-in L900 schedules |
| [Tapo P100 / P110](docs/devices.md#tapo-p100p110-plugs) | Relay control, P110 metering and overload monitoring |
| [Samsung UE40H6200 / DIGI R2A](docs/devices.md#samsung-tv-and-digi-r2a) | Paired remotes and shared-outlet TV routines |
| [TCL Android / Google TV](docs/devices.md#tcl-android--google-tv) | Power, keys, app links, text and supplied voice audio; optional Cast playback |
| [Computers](docs/devices.md#computer-shutdown-service) | Wake, authenticated shutdown and optional metered outlet cutoff |

Controls verify identity where supported and check reported outcomes. Uncertain
results stop execution; packet or key delivery alone does not prove a device
changed state. Each guide describes its verification limits.

## Agents and automation

Start a read-only MCP server:

```sh
target/release/homewire --config .local/home.json serve
```

Only configured capabilities appear. Add `--allow-control` to expose write tools.
See [MCP and ZeroClaw](docs/automation.md#mcp-and-zeroclaw) for discovery, fresh
reads and routine previews, or [daemon and bridge](docs/automation.md#persistent-daemon-and-mcp-bridge)
for a persistent owner shared by clients.

[Meaco schedules](docs/automation.md#scheduling) and
[L900 curves/sunset rules](docs/devices.md#l900-scheduling) require explicit
runtime flags. **Disable corresponding Home Assistant rules before enabling
replacement controls.** Use one configuration path per device and a trusted LAN;
Homewire's locks cannot coordinate with manufacturer apps or other controllers.
Ownership locks are explicitly released when their owner is dropped, even
during concurrent child-process startup.

## Further reading

- [Configuration and CLI](docs/configuration.md): schemas, commands, credentials and migration.
- [Household services](docs/services.md): playback, alerts, history, dashboard and Android companion.
- [Android deployment](docs/android.md): installation, isolation, verification and rollback.
- [Development](docs/development.md): architecture, required checks and memory measurement.
- [Reference index](docs/reference.md): all guide sections.

[MIT license](LICENSE)
