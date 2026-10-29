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

Rooted Android hosts can add an independent [battery guard](docs/android.md#battery-guard)
with a charge ceiling, temperature cutoff, persistent serious-fault lockout and
optional approved Telegram alerts. It runs separately from the automation owner.

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

[Explicit household routines](docs/configuration.md#explicit-household-shutdown-lists)
compose shutdown lists with device-before-outlet ordering and fresh idle printer
checks. `leave_house` includes PC and kitchen only on explicit request;
`sleep` uses its complete configured list, including explicit light/group-off steps.
Wake, sleep and leave-house lists run independent device chains concurrently,
including TV/PC readiness and shutdown verification alongside other devices.
Each chain preserves device-before-outlet shutdown and outlet-first startup.
Results stay in configuration order; timing is best effort and partial failures
never trigger automatic replay.
[Device groups](docs/configuration.md#device-groups) switch their configured lights
concurrently with per-member verification.
`group_plan` previews this without device I/O.
`group_set` uses absolute power and preserves brightness and colour.
Preview shutdown routines without device I/O.
`wake_up` uses an [explicit startup list](docs/configuration.md#explicit-startup-list)
with protected device exclusions and outlet-first TV/PC sequencing. Keep printers
and their supply outlets out of its steps and explicitly excluded, so waking up
does not turn either on. Preview it
without controls; its power-only steps preserve other settings.

| Setup guide | Capabilities |
| --- | --- |
| [Meaco Arete Two](docs/configuration.md#configuration) | Status, purification/sleep controls and day/night/off routines |
| [Brother HL-L2350DW / HP DeskJet 3630](docs/devices.md#printers) | Status, supplies and confirmed fault warnings |
| [Awair Element](docs/devices.md#awair-element) | Fresh temperature, humidity, CO₂, TVOC, PM2.5 and score |
| [Tapo L430C / L900](docs/devices.md#tapo-l430c-bulbs) | Light controls, overheating monitoring and opt-in L900 schedules |
| [Tapo P100 / P110](docs/devices.md#tapo-p100p110-plugs) | Relay control, P110 metering and overload monitoring |
| [Samsung UE40H6200 / DIGI R2A](docs/devices.md#samsung-tv-and-digi-r2a) | Paired remotes and shared-outlet TV routines |
| [TCL Android / Google TV](docs/devices.md#tcl-android--google-tv) | Power, keys, app links, text and supplied voice audio; optional Cast playback and authenticated backlight companion with shared light-curve scheduling |
| [Computers](docs/devices.md#computer-shutdown-service) | Wake, authenticated shutdown and optional metered outlet cutoff |

Controls verify identity where supported and check reported outcomes. Uncertain
results stop execution; packet or key delivery alone does not prove a device
changed state. Each guide describes its verification limits.
[TCL TV routines](docs/configuration.md#unified-configuration) turn the outlet on
before the TV, and verify TV shutdown before turning the outlet off.
Waking a powered TCL from standby also requires the TV's
[network standby mode](docs/devices.md#tcl-power-and-network-standby);
firmware that still suspends its network can opt in to the companion's
[standby connection](companion/tv/README.md#keep-the-connection-available-in-standby).
Microphone access can remain blocked.
Computer startup requires verified outlet state, without depending on energy
statistics; metered shutdown still requires fresh consumption readings.
P110 live metering remains available when an uninitialized outlet clock causes
calendar statistics to be rejected; missing daily/monthly totals stay unknown.

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

Light and TCL brightness followers share one curve target per scheduling cycle.
Scheduled TCL backlight has a minimum of 30, including at the end of the curve
and outside it; light brightness keeps the configured range.
Independent bounded device I/O keeps an unreachable target from delaying the
others; existing follower settings and attempt guards remain supported.
Manual brightness adjustments pause only that device for two hours, restarting
the timer on further changes and surviving restarts. Use
`brightness_schedule_status` or `brightness_schedule_resume` with its entity ID
to inspect the pause or resume the current target now.

## Android assistant input

The [native assistant input](companion/betty/README.md) takes Android's digital
assistant shortcut, records on-device dictation, and shows an editable transcript.
Tap Send to deliver text into the existing private Telegram bot conversation;
after Telegram confirms delivery, the app opens that conversation in Telegram.
The capture screen contains no chat history or replies. Recording stops when the
app leaves the foreground, and opening the app never sends a message.
See the guide for sign-in, Pixel power-button setup, privacy boundaries and builds.

## TCL companion releases

The companion also has an opt-in [visual navigation prototype](companion/tv/README.md#visual-navigation-prototype)
for authenticated screenshots and accessibility navigation on the paired TV.
Agents use `media_screenshot`, `media_screen_status` and `media_navigate` through
MCP. Optional playback readback adds Android media-session state and available
title metadata to `media_screen_status`.
Enable the companion's accessibility service explicitly; navigation also
requires `--allow-control`. [Visual control guide](docs/devices.md#tcl-screenshots-and-navigation).

After the private toolchain and Infisical bindings are configured, build, sign,
install and verify an update from the repository root:

```sh
python3 companion/tv/release.py --version 0.1.5
```

Choose an unused version greater than the installed version. Add `--verify-reboot`
to deliberately reboot once and verify automatic startup.
The command preserves pairing, recovers bounded transient ADB connection failures
before installation, and saves versioned artifacts and previous-APK backups. See the [one-time setup, verification and recovery guide](companion/tv/README.md#one-command-releases).

## Further reading

- [Configuration and CLI](docs/configuration.md): schemas, commands, credentials and migration.
- [Household services](docs/services.md): playback, alerts, history, dashboard and Android companion.
- [Android deployment](docs/android.md): installation, isolation, verification and rollback. Includes optional [offline ultrasleep](docs/android.md#offline-ultrasleep) with a private schedule, local wake alarm, checked service transitions and a retained recovery journal.
- [Development](docs/development.md): architecture, required checks, memory measurement and [privacy hooks](docs/development.md#public-repository-privacy).
  Contributors should install the hooks before committing or pushing.
- [Reference index](docs/reference.md): all guide sections.

The isolated Android agent serves its installed ZeroClaw dashboard from read-only
assets. Its path compatibility layer keeps procfs hidden and rejects symlinks and
path traversal; see [Android isolation](docs/android.md#zeroclaw-isolation).
Fresh isolated installations prepare the pinned ZeroClaw release with
`scripts/android/zeroclaw_package.py` and pass its dashboard through
`isolate.py --web-assets`; existing native/policy updates preserve dashboard files.

[MIT license](LICENSE)
