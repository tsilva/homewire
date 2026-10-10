<p align="center">
  <img src="logo.png" alt="Homewire" width="280" /><br />
  <!-- repo-tagline:start -->
  <strong>🏠 Local home automation with verified device control 🔌</strong>
  <!-- repo-tagline:end -->
</p>

Homewire is a command-line tool for people who want to control home devices on
their local network and run daily automations. Read device status, preview
changes, and run control sequences from your terminal. Agents such as ZeroClaw
can use the same devices and routines through its MCP server.

One Rust executable contains all device adapters. It needs no Home Assistant,
Docker, Python, Node, or model runtime. Controls check device identity where
supported and verify reported results; uncertain outcomes stop execution.

## Install

Requires **Rust 1.89 or newer** to build and network access to your devices.

```sh
git clone https://github.com/tsilva/homewire.git
cd homewire
cargo build --release --locked
mkdir -p .local
chmod 700 .local
cp examples/home.json .local/home.json
chmod 600 .local/home.json
```

Edit `.local/home.json` with your device endpoints and credentials. Remove
adapter, routine, automation and service sections you do not use. Settings live
in one private file; each adapter still validates its own schema. Keep the
Meaco settling delay at 20 seconds. Validate without contacting devices, then
read its status:

```sh
target/release/homewire --config .local/home.json config-check
target/release/homewire --config .local/home.json status
```

The unified file contains `version: 1`, `adapters`, `routines`, `automations`,
`services` and optional host-only `deployment`. See the [schema and migration
procedure](docs/reference.md#unified-configuration). Certificates, runtime state
and history remain separate. Schedules still need explicit runtime flags.

Files in `examples/` are synthetic templates, not a deployed household configuration.
Keep real addresses, identities, keys and location in ignored `.local/` files.
Replace every credential placeholder and documentation address before use;
coordinates `0.0, 0.0` are placeholders for your chosen location.

Add a device by copying its example object into the matching section of the
unified file. The linked reference explains credentials, fields and limits.
Automatic discovery is not included. DIGI PIN pairing has an optional host-only
setup helper; Samsung requires an existing pairing. Separate example files and
per-family CLI flags remain available for legacy installations.

Private device credentials can be stored in Infisical. A string of the form
`${HOMEWIRE_SECRET_NAME}` in a private JSON config resolves from that environment
variable at startup; missing or invalid values abort loading. For PEM private-key
paths, use `env:HOMEWIRE_SECRET_NAME` to read the PEM from the environment without
a credential file. Certificates remain ordinary files and peer pins stay unchanged.
Install the Infisical CLI, run `infisical login` and `infisical init` to select your
workspace (the resulting `.infisical.json` is Git-ignored), then launch the native process with
`infisical run --env=prod -- target/release/homewire ...` from the configured checkout.
Shared credentials use one secret name across configs. Do not copy private-key files
or plaintext token exports into the repository. The phone installer resolves these references in memory and sends credential files directly
to protected device storage with mode `0600`. It does not create plaintext host copies.
Missing secrets abort validation before installation; existing device state and pins are preserved.

## Devices

The table lists supported adapter models. Device names and endpoints are supplied
by each installation through private configuration.

| Device | Unified section and schema example | Supported behavior |
| --- | --- | --- |
| [Meaco Arete Two](docs/reference.md#configuration) | `adapters.meaco`, [meaco.json](examples/meaco.json) | Status; day, night and off routines over Tuya 3.3 |
| [Brother HL-L2350DW / HP DeskJet 3630](docs/reference.md#printers) | `adapters.printers`, [printers.json](examples/printers.json) | Read-only IPP status, supplies and fault monitoring |
| [Awair Element](docs/reference.md#awair-element) | `adapters.awair`, [awair.json](examples/awair.json) | Local temperature, humidity, CO₂, TVOC, PM2.5 and score |
| [Tapo L430C / L900](docs/reference.md#l900-led-strip) | `adapters.lights`, [lights.json](examples/lights.json) | Verified power, brightness, colour and white temperature; overheating monitoring |
| [Tapo P100 / P110](docs/reference.md#tapo-p100p110-plugs) | `adapters.plugs`, [plugs.json](examples/plugs.json) | Verified relay control; P110 metering and overload monitoring |
| [Samsung UE40H6200](docs/reference.md#samsung-tv-and-digi-r2a) | `adapters.samsung`, [samsung.json](examples/samsung.json) | Existing encrypted pairing, wake, shutdown request and remote keys |
| [DIGI R2A](docs/reference.md#samsung-tv-and-digi-r2a) | `adapters.digi`, [digi.json](examples/digi.json) | Paired mutual TLS, verified wake/sleep and remote keys |
| [Shared TV outlet](docs/reference.md#shared-tv-outlet-workflow) | `routines.tv`, [tv.json](examples/tv.json) | Ordered outlet, Samsung and DIGI power routines |
| [Wake-on-LAN targets](docs/reference.md#wake-on-lan) | `adapters.wol`, [wol.json](examples/wol.json) | One-shot wake packets for explicitly configured devices; successful wake remains unverified |

Printer power uses the configured smart outlets through `switch_set`; no printer
shutdown command is sent. Printer state, queued jobs, job acceptance and hardware
fault status have individual read-only entities. Monitoring reports paper/jam,
cover and supply faults, plus separate confirmed low ink/toner warnings and
recovery. Missing readings never clear retained alerts. See
[printer monitoring](docs/reference.md#printers).

Enable Awair's local API in its app. L900 fixed brightness/colour settings
require effects and music mode to be off. P110 overload guards block changing
power to on while protection status is active or unknown. Samsung shutdown may
return `verified: false`; its API does not always report physical power state.
See the reference for each adapter's verification limits.

Router administration and router-based presence tracking have been removed.
Remove `adapters.router` from existing unified configurations before starting
this version; unsupported adapter sections fail configuration validation.
Android installs no longer import, copy or launch legacy `router.json` files.
Companion location and person entities remain available through `mobile`.

## Household services

Additional integrations use the same static adapter catalog and standard entity
interfaces. Configure hardware under `adapters` and household services under `services` in
the unified file:

| Family | Support |
| --- | --- |
| `brother` | Read-only SNMP page, duplex, toner and drum diagnostics |
| `cast`, `dlna` | Pinned Cast v2 and identity-checked DLNA playback, volume, mute and seek |
| `mobile` | Companion battery/location and person entities, preserving uncertain states |
| `alerts` | Bounded durable alerts, channel retries, companion receipts and recovery clearing |
| `history` | Fixed-size numeric recording and bounded trend/energy queries |
| `weather`, `shopping` | MET forecast and persistent household shopping list |
| `radio`, `speech` | Radio Browser search and Google Translate speech media URLs |

Meaco, plugs and lights expose typed settings through `entity_configure`. DIGI
settings support app links and text input. [Reference and example configs](docs/reference.md#household-services)
cover exact fields, storage limits and validation status.

The optional HTTPS frontend provides device controls, routines, alerts, shopping,
weather and history charts. The native Android companion reports telemetry and
receives alerts; its APK has no bundled model key, browser or framework. Optional
voice input uses Android's installed speech provider and forwards a reviewed
transcript to ZeroClaw. Whisper pipelines and Android TV voice streaming are
not implemented.
See [companion setup](docs/reference.md#native-android-companion).

## Commands

Run from the repository root with the corresponding private configs prepared:

```sh
target/release/homewire --help                                    # Full CLI reference
target/release/homewire --config .local/home.json plan night      # Preview without controls
target/release/homewire --config .local/home.json apply day       # Apply the configured daytime preset
target/release/homewire --config .local/home.json apply night     # Purify, then sleep
target/release/homewire --config .local/home.json apply off       # Switch off and verify
target/release/homewire --config .local/home.json printer-status brother
target/release/homewire --config .local/home.json awair-status
target/release/homewire --config .local/home.json light-status led_strip
target/release/homewire --config .local/home.json light-set led_strip '{"power":true,"brightness_percent":10}'
target/release/homewire --config .local/home.json plug-status metered_outlet
target/release/homewire --config .local/home.json plug-set relay off
target/release/homewire --config .local/home.json tv-status
```

CLI results are JSON on stdout; failures go to stderr with a nonzero exit code.
Meaco sleep is a toggle and is never automatically resent. Use `watch-printers`,
`watch-lights` or `watch-plugs` for continuous fault/recovery events. Monitoring
retains confirmed faults across restarts; unknown readings never prove recovery.
The optional alert ledger supports HTTPS delivery channels and native companion
notifications, including recovery updates and persisted receipts.

Alerts can also deliver directly to Telegram without an agent or Home Assistant.
Configure a channel with `delivery: "telegram"`, the official bot `sendMessage`
URL and `chat_id`; optional `keys` restrict it to exact alert keys. Protect the
URL as a credential. Tank-full messages request emptying the tank; confirmed
recovery sends a resolved message. Failed sends remain pending, and saved delivery
receipts suppress unchanged repeats across normal restarts. See
[alert delivery configuration](docs/reference.md#alerts-history-and-convenience-tools).

Companion battery reports can also feed those channels. Explicitly configure
`services.mobile.battery_monitor` with the paired devices to monitor. Defaults
warn below 15% and recover at 20%; missing or stale reports never clear a warning.
Add the exact `battery.NAME` key to filtered delivery channels. See
[companion battery monitoring](docs/reference.md#companion-battery-monitoring).

## MCP and ZeroClaw

Expose configured devices through one local stdio server:

```sh
target/release/homewire --config .local/home.json serve
```

The server is read-only by default. Add `--allow-control` to expose controls;
only tools for configured capabilities appear.

| Tools | Purpose |
| --- | --- |
| `entity_list`, `entity_status` | Paginated discovery and fresh batched reads |
| `entity_options`, `entity_configure` | Live choices / advertised device settings |
| `media_play`, `media_command` | Cast/DLNA URL playback and transport controls |
| `alert_list`, `history_series`, `history_query` | Durable alerts and bounded household data |
| `weather_status`, `shopping_list`, `shopping_update` | Forecast and household list |
| `radio_search`, `tts_media` | Find stations / create speech media URLs |
| `light_set`, `switch_set` | Apply absolute light settings and outlet power |
| `media_power`, `remote_key` | Control media power and send remote keys |
| `wake_list`, `wake_on_lan` | Discover configured wake targets / request Wake-on-LAN |
| `routine_list`, `routine_plan`, `routine_run` | Discover, preview and execute household routines |

For example, `entity_status` takes `{"entities":["humidifier.meaco"]}` and
`routine_plan` takes `{"routine":"meaco_night"}`. Available routines are
`meaco_day`, `meaco_night`, `meaco_off`, `tv_on` and `tv_off`, depending on config.
Meaco plans read live state; TV plans describe conditional steps without querying
devices. Add `serve --legacy-tools` for the original adapter-specific tool names.
See [tool arguments and ZeroClaw setup](docs/reference.md#mcp-and-zeroclaw).

Samsung is automatically available as `samsung.samsung` in `wake_list`; waking
it retains identity checks, on-state verification and TV ownership guards.
Add `adapters.wol` using [wol.json](examples/wol.json) to name up to eight other
Wake-on-LAN targets. `wake_on_lan` takes `{"device":"wol.workstation"}` and
requires `--allow-control`. Generic targets send once and return `verified:false`;
packet delivery does not prove that the device woke. See
[Wake-on-LAN configuration](docs/reference.md#wake-on-lan).

`--tv` owns its referenced Samsung, DIGI and plug configs; omit separate
`--samsung`, `--digi` and `--plugs` flags when using it. Use `tv-set on|off` or
`routine_run` for coordinated power. Direct power controls for its two media
devices and selected outlet require `--maintenance`. Other outlets and remote
keys remain available. Its explicit `fixed_delay` shutdown policy cuts outlet
power after verified DIGI sleep, a Samsung shutdown request and the configured
wait; it does not claim Samsung physically confirmed off. Earlier errors abort
cutoff. Full live power-cycle validation remains pending.

## Android deployment

Set the `deployment` section in `.local/home.json` using the
[deployment schema](examples/deployment.json), with the
DNS servers, service ports, agent name and provider hostname, and protect it with
`chmod 600`. Keep device credentials in the private unified file as
Infisical references. The profile generates firewall, gateway, MCP and
probe settings together; it adds no resident process. Android paths derive from
the configured package/user; optional root/config-path overrides are validated.
Set `HOMEWIRE_AGENT` to the profile’s agent name for the commands below.

Build an ARM64 Android package on macOS or Linux, then install it through ADB
into Termux. Requires Rust 1.95.0 with the `aarch64-linux-android` target, Android
NDK 30.0.16248370, Python 3.11+ on the host, and an existing Termux service
supervisor on the phone. The installer requires a Termux installation that permits
`run-as` for the configured package/user. Set `HOMEWIRE_ADB_SERIAL` to the target shown by `adb devices`;
keep device-specific installation records private.

```sh
# Set HOMEWIRE_NDK to your installed NDK directory first.
scripts/android/build.sh
infisical run --env=prod -- python3 scripts/android/phone.py --config .local/home.json --serial "$HOMEWIRE_ADB_SERIAL" install
python3 scripts/android/phone.py --config .local/home.json --serial "$HOMEWIRE_ADB_SERIAL" connect-zeroclaw --agent "$HOMEWIRE_AGENT"
python3 scripts/android/phone.py --config .local/home.json --serial "$HOMEWIRE_ADB_SERIAL" verify
# Config update with control tools enabled; state and saved schedules remain:
infisical run --env=prod -- python3 scripts/android/phone.py --config .local/home.json --serial "$HOMEWIRE_ADB_SERIAL" install --allow-control
# Restore a previously validated executable, when one exists:
python3 scripts/android/phone.py --config .local/home.json --serial "$HOMEWIRE_ADB_SERIAL" rollback
```

The supervised daemon owns devices independently of ZeroClaw sessions. MCP
clients use `homewire bridge SOCKET` to connect through a private Unix socket;
closing a client leaves the daemon running. A new installation starts read-only
with schedules/monitoring disabled; updates preserve existing grants and flags. See the
[deployment reference](docs/reference.md#android-deployment) for prerequisites,
boot setup, rollback limits and phone validation.

For a rooted Android deployment, isolate ZeroClaw after configuring its local assistant gateway.
Build the small native guard and permission-only Android identity, then run
`python3 scripts/android/isolate.py --config .local/home.json --serial SERIAL install`. The installed agent
has its own UID, read-only policy, no program execution or host filesystem access,
and a scoped HTTP MCP connection. Telegram gets read-only Homewire tools; the
trusted companion/dashboard gateway can use normal controls. Device configuration
is withheld from the agent. See [sandbox setup and limits](docs/reference.md#zeroclaw-isolation).
Legacy `connect-zeroclaw` and `connect_assistant.py` refuse changes after isolation.
Homewire's schedules and monitors continue independently.

## Automations

**Disable corresponding Home Assistant rules before enabling replacement controls.**

Android updates preserve saved schedule, monitor and control flags. New installs
default to read-only; enabling a schedule and transferring ownership are explicit
installation choices.

The optional Meaco schedule reads its timezone and day/night times from the
private Meaco JSON. Its day/night presets are also explicit configuration, with
no built-in household humidity target. Missing settings fail before controls:

```sh
target/release/homewire --config .local/home.json schedule
# Or combine scheduling and MCP:
target/release/homewire --config .local/home.json serve --allow-control --schedule
```

[L900 scheduling](docs/reference.md#l900-scheduling) adjusts an on strip along a
daily brightness/temperature curve and turns it on at locally calculated sunset.
Configure its timezone and actual location in the lights file, then use
`light-plan` to preview or `light-schedule` to run. For MCP, add
`--allow-control --light-schedule`. Missed timed runs are not caught up;
failed or interrupted attempts are not blindly replayed. See
[scheduler supervision and failure behavior](docs/reference.md#scheduling).

For an existing Android owner, validate the private curve and preview it with
`light-plan` before transferring ownership. Disable both the previous controller's
curve and sunset rules persistently, then add `--light-schedule` to `service.args`
and restart Homewire, preserving its other flags and attempt state. Keep matched
configuration and executable backups for rollback; older binaries may not accept
the newer explicit curve, Meaco presets/schedule and TV timings. Verify an actual
curve adjustment with the previous rules off, and check that flags survive an
owner restart. A future sunset and a phone reboot need their own live checks.

## Notes

- Keep credentials in ignored private files, including media certificates and
  keys. Never put secrets in tool arguments or tracked files.
- Use one config path per device across clients. Stateful adapters hold locks
  and save bounded state alongside configs; access a running server through MCP.
  These locks cannot coordinate with Home Assistant or manufacturer apps.
- MCP operations run sequentially. Allow time for device settling and TV boot.
  Watch flags enable background monitoring, with events on stderr. The server
  exits on stdin EOF, including schedules running inside it.
- Use a trusted LAN: Tuya 3.3's CRC does not authenticate the peer. Optional weather, radio and TTS use public services. Agent inference runs in
  ZeroClaw. The HTTPS frontend serves the dashboard and companion; MCP uses IPC.
- Build separately for the target platform. See [Android deployment](docs/reference.md#android-deployment)
  for installation prerequisites and target validation. Measure memory, background
  operation and recovery on the device where you deploy.

## Development

```sh
cargo fmt --check
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s scripts/android -p 'test_*.py'
```

GitHub Actions runs these checks and a release build with Rust 1.95.0 on Linux.
Tests use synthetic credentials and simulated devices. Protocol adapters,
capability entities and household routines compile into one program; batched
reads share physical snapshots, then release them. Background jobs wait for
the next deadline. See [adding device support](docs/reference.md#static-device-adapters)
and [memory measurements](docs/reference.md#memory-and-allocation-checks).

## License

[MIT](LICENSE)
