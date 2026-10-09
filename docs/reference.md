# Homewire reference

See the [README](../README.md) for installation and first use. This reference
covers configuration, verified control behavior, agent integration, scheduling,
printer monitoring, Awair measurements, bulb controls, and implementation limits.

## Unified configuration

The primary input is `homewire --config .local/home.json COMMAND`.
Copy [examples/home.json](../examples/home.json) into ignored `.local/`,
protect it with `chmod 600`, replace the synthetic values and remove unused
families. The file is bounded to 64 KiB and requires `version: 1`.

| Section | Contents |
| --- | --- |
| `adapters` | `meaco`, `printers`, `awair`, `lights`, `plugs`, `samsung`, `digi`, `router`, `brother`, `cast`, `dlna`; objects use the existing adapter schemas below |
| `routines.meaco` | Day/night presets, previously `meaco.json`'s `presets` |
| `routines.tv` | `plug`, `shutdown_policy`, `timings`; its children are the configured Samsung, DIGI and plugs adapters, without file references |
| `automations.meaco` | `timezone`, `day`, `night`, previously Meaco `schedule` |
| `automations.lights` | L900 curve/location/timezone settings, previously lights `automation` |
| `services` | `api`, `alerts`, `history`, `mobile`, `weather`, `shopping`, `radio`, `speech`; objects use the service schemas below |
| `deployment` | Host-only Android installation profile described under Android deployment |

Sections are optional, but enabled routines and automations require their
adapters. Unknown root keys, misplaced families, unsupported versions and
conflicting routine/automation declarations fail loading. Each statically
compiled adapter decodes and validates its own typed settings through
`config::Source`; the catalog declares whether it belongs to `adapters` or
`services`. A new adapter needs no additional loader switch. The TV routine
composes typed child managers and retains their existing ownership safeguards.

The file is read once at startup. Temporary JSON, including the host-only
`deployment` block, is discarded before serving; there is no configuration
service, dynamic module loader or JSON cache. `${HOMEWIRE_SECRET_NAME}` and
PEM `env:HOMEWIRE_SECRET_NAME` references work as before. Relative certificate
and history paths resolve beside `home.json`. Runtime state and locks keep
canonical filenames such as `meaco.state.json`, `lights.state.json` and
`tv.state.json`, including `FAMILY.json.lock`, beside the unified file. The root
also has an exclusive `home.json.lock`. Keep one config directory per
installation and use the daemon bridge for additional clients.

```sh
infisical run --env=prod -- target/release/homewire --config .local/home.json config-check
infisical run --env=prod -- target/release/homewire --config .local/home.json serve
```

`config-check` validates adapter/service schemas, API credentials/TLS and stored
state without device requests or listener binding. It does not verify online
availability. Configuration presence alone does not enable controls or
schedules: `--allow-control`, `--schedule`, `--light-schedule` and watch flags
remain explicit runtime grants. Android updates preserve saved grants.

To migrate existing private files in place:

```sh
python3 scripts/android/configuration.py --configs .local --output .local/home.json
infisical run --env=prod -- target/release/homewire --config .local/home.json config-check
```

The helper imports known adapter/service JSON and `deployment.json`, moves
presets/schedules into the unified sections, and removes TV child file
references. Imported originals are copied to a mode-700 `.local/backups/`
directory before being retired. State, history, certificates, keys and unrelated
setup/test records remain untouched. It refuses an existing output, a changed
config directory or mismatching TV child paths. Verify with the new executable
before installation; backups provide recovery if adapter validation fails.

Legacy separate-file flags and legacy Meaco `--config` remain supported.
Do not mix unified `--config` with `--adapter`, `--api` or per-family config
flags. Adapter examples and the older command snippets below also document
those compatibility interfaces. A previous executable that predates unified
configuration requires restoration of the backed-up separate configs and
launcher; binary-only rollback cannot reverse a configuration format migration.

## Configuration

Tracked JSON files under `examples/` are synthetic templates. They contain
placeholder credentials, sample names and documentation-only `192.0.2.x` addresses.
They do not represent any installation. Copy the templates into ignored `.local/`
files, replace all placeholders, and protect real configurations with mode 600.
Weather/mobile coordinates of `0.0, 0.0` must be replaced with your chosen location.
Keep device serials, household inventories, pairing material, status captures and
deployment/migration records in private storage. Public documentation describes
capabilities and procedures rather than a particular household.

The local key and device ID come from your existing Tuya-local setup. Only
protocol `3.3` is supported in this first module. Pairing, discovery, cloud API
access, other Tuya versions, and additional Tuya device families are not implemented.

Configuration fields: `device_id`, literal-IP `host`, `local_key`,
`protocol_version`, optional `port` (6668), `timeout_seconds` (8; range 1–30),
and `settle_seconds` (20; range 1–60). Optional `presets` and `schedule` contain
installation behavior; neither supplies an implicit household default.
Do not put device credentials in tracked files or tool arguments.

Homewire stores confirmed base mode and schedule attempts alongside the config
as `meaco.state.json`; `meaco.json.lock` serializes ownership. Use the same config
path for all clients of one device. A running server holds the lock; access it
through MCP instead of starting a concurrent CLI instance. This lock cannot
coordinate with Home Assistant or the manufacturer's app.

## Device controls

```sh
target/release/homewire --config .local/meaco.json status
target/release/homewire --config .local/meaco.json plan night
target/release/homewire --config .local/meaco.json apply day
target/release/homewire --config .local/meaco.json apply night
target/release/homewire --config .local/meaco.json apply off
```

- **Day/night:** apply the explicitly configured preset: power on if needed,
  clear sleep once if necessary, verify the optional humidity and base mode,
  then enable sleep only when requested. Confirmed matching settings are skipped.
- **Off:** switch off and verify that the device remains off; no preset required.

```json
"presets": {
  "day": {"mode": "manual", "target_humidity": 50, "sleep": false},
  "night": {"mode": "purify", "sleep": true}
},
"schedule": {"timezone": "Etc/UTC", "day": "09:00", "night": "22:00"}
```

These are sample choices, not defaults. Each preset accepts `mode` (`manual` or
`purify`), optional `target_humidity` (35–70%, steps of 5) and optional `sleep`
(false). Omitted humidity is preserved. Either preset may be omitted; its routine
is then absent from `routine_list`, and direct execution fails before device I/O.
A configured schedule requires both presets, a valid IANA timezone and distinct
strict `HH:MM` times. Schedule enablement still requires an explicit command/flag.

Each sent command is followed by the settling delay and a fresh state query.
Already-correct settings are skipped. A missing reading, rejected command,
failed verification, or uncertain result stops execution. Sleep is a toggle and
is never automatically resent. Confirmed mode assumptions are invalidated before
controls and restored only after the entire sequence succeeds.

`status` and `plan` send status queries but no device controls. Plans are
provisional: the runner rechecks state after each step. CLI commands print JSON
results on stdout; failures go to stderr with a nonzero exit code.

For testing through a TCP tunnel, `--connect 127.0.0.1:PORT` overrides only the
connection address. The device identity and key remain in the private config.

## Printers

The printer adapter sends only IPP `Get-Printer-Attributes` over local HTTP. It
reports idle/printing/stopped state, reasons, job acceptance/count, and ink/toner
levels when provided. No printing, cancellation, power control, discovery,
authenticated IPP or TLS is implemented in the IPP adapter. A separate
`brother` adapter provides read-only SNMP diagnostics.

```sh
target/release/homewire --printers .local/printers.json printers
target/release/homewire --printers .local/printers.json printer-status brother
target/release/homewire --printers .local/printers.json printer-status hp
target/release/homewire --printers .local/printers.json printer-check
target/release/homewire --printers .local/printers.json watch-printers
```

Copy `examples/printers.json` to an ignored file, replace the example addresses,
and use `chmod 600`. The schema has top-level `printers` (1–8 endpoints),
`timeout_seconds` (8; range 1–30), and `poll_seconds` (30; range 5–300).
Each endpoint requires `name` (unique, 1–32 letters/digits/hyphens/underscores)
and literal-IP `host`; optional `port` defaults to 631 and `path` to `/ipp/print`.
Optional `connect` (`IP:PORT`) overrides just the TCP connection for tunnels.
Use a separate config file for simulated monitoring. Unknown fields are rejected.
The supported unauthenticated IPP endpoints require no credentials.

Fault classification uses reported IPP reasons: paper,
jam, ink/toner, covers/trays, fuser and other specified supply/hardware reasons,
plus unknown reasons explicitly ending in `-error`. Idle/printing and routine
sleep/pause/offline reasons do not become fault alerts. Missing data, failed
queries, unclassified reasons, or stopped without a specific fault cannot prove
recovery. Percentages alone never generate faults. Negative or out-of-range
supply levels remain unknown; raw values are retained.

`watch-printers` emits JSON fault/recovery events on stdout; connection
diagnostics go to stderr. A changed fault must appear in observations at least
3 seconds apart; recovery needs clear observations at least 30 seconds apart.
Unknown readings interrupt confirmation. With 30-second polling, confirmation
normally takes one more poll. Monitoring is opt-in; querying IPP can wake a
sleeping printer.

Confirmed transitions are atomically saved before output in
`printers.state.json`; `printers.json.lock` excludes concurrent owners using the
same config. Repeated faults are suppressed across reconnects/restarts. Changes
to name, host, port or path invalidate saved assumptions for that printer.
The one-shot `printer-check` reports live status and retained faults; candidates
live in the running process, so separate CLI invocations do not form a continuous
monitor. This is a local event source; the optional alerts ledger and native
companion provide notification delivery and acknowledgements. `active_faults`
returns retained confirmed
faults even if a consumer missed an event.

For desktop testing through an SSH host that can reach the printers, keep TCP
forwards open and use `connect` overrides in a separate private test config.
For example, forward local port 62350 to the Brother's port 631, then set its
`connect` to `127.0.0.1:62350`. The printer URI still uses its real LAN host/path.

## Awair Element

This adapter sends only `GET /air-data/latest` to the Element's local HTTP API.
Enable that API in the Awair app if needed; no account token is required at
runtime. The device refreshes its measurements approximately every 10 seconds.
See [Awair's local API documentation](https://support.getawair.com/hc/en-us/articles/360049985053-Awair-2nd-Edition-Local-API-Feature).

Copy `examples/awair.json` to an ignored file and use `chmod 600`. Required
`host` is a literal LAN IP. Optional fields are `port` (80; nonzero),
`timeout_seconds` (8; range 1–30), `max_age_seconds` (120; range 10–3600), and
`connect` (`IP:PORT`, nonzero port; overrides the TCP destination for testing).
Unknown config fields and files larger than 8 KiB are rejected.

```sh
target/release/homewire --awair .local/awair.json awair-status
# One MCP process for all three adapters:
target/release/homewire --config .local/meaco.json --printers .local/printers.json --awair .local/awair.json serve
```

| JSON field | Measurement/unit |
| --- | --- |
| `temperature_c` | Temperature, °C |
| `humidity_percent` | Relative humidity, % |
| `co2_ppm` | Measured CO₂, ppm |
| `tvoc_ppb` | Total volatile organic compounds, ppb |
| `pm25_ug_m3` | Measured PM2.5, µg/m³ |
| `awair_score` | Device's score, 0–100 |

`measured_at` is the device timestamp normalized to UTC; `observed_at` is the
host's UTC time after the response. `age_seconds` is signed, exposing small
clock skew. Readings older than `max_age_seconds`, or more than 30 seconds in
the future, fail. Keep both clocks accurate. Missing/null measurements, invalid
timestamps, negative pollutants, humidity outside 0–100%, and scores outside
0–100 fail rather than becoming zero. Estimated CO₂/PM10 and calibration fields
are ignored. No cached reading is substituted after a failed request.

Standard MCP exposes these measurements through `entity_status`. The legacy
`awair_status` tool (`serve --legacy-tools`) takes no arguments and reads on demand. There is no persistent
socket, background polling thread, cloud access, display control, threshold
alerting or sensor-triggered Meaco automation. Existing Meaco schedules do not
depend on this module.

## Tapo L430C bulbs

The shared light adapter uses KLAP v2 over local HTTP. It supports named L430C bulbs, power,
brightness, colour via hue/saturation, white temperature, and reported
overheating. Typed settings also expose presets, stored effects, transitions and
auto-update settings. Pairing, discovery, Bluetooth, cloud access, firmware
installation and other Tapo device types are not implemented.
The configured device ID and model must match a live reading before controls.

Copy `examples/bulbs.json` to an ignored private file and use `chmod 600`.
Top-level fields are `bulbs` (1–8 endpoints), `timeout_seconds` (8; range 1–30),
`settle_seconds` (2; range 0–10, strictly less than timeout), and `poll_seconds`
(30; range 5–300). Each bulb needs a unique `name` (1–32 letters, digits,
hyphens or underscores), literal-IP `host`, unique `device_id` (1–64 letters
or digits), and `auth_hash` (64 hexadecimal digits). Optional `model` defaults
to `L430C`; only `L430C` and `L900` are accepted. Optional `port` defaults
to 80; `connect` (`IP:PORT`, nonzero port) overrides just TCP for tunnels.
Unknown config fields and files over 8 KiB are rejected.

The secret hash is the KLAP v2 digest
`SHA256(SHA1(username_utf8) || SHA1(password_utf8))`, encoded as hexadecimal.
Username/password are case-sensitive. For an existing Home Assistant KLAP v2
entry, `credentials_hash` is Base64 of this same 32-byte digest: decode it and
encode those bytes as hex. Treat the hash as a credential. Homewire stores no
account password and never contacts the Tapo cloud.
Protocol reference: [python-kasa's KLAP implementation](https://github.com/python-kasa/python-kasa/blob/master/kasa/transports/klaptransport.py).

```sh
target/release/homewire --bulbs .local/bulbs.json bulbs
target/release/homewire --bulbs .local/bulbs.json bulb-status bulb_1
target/release/homewire --bulbs .local/bulbs.json bulb-set bulb_1 '{"power":false}'
target/release/homewire --bulbs .local/bulbs.json bulb-set bulb_1 '{"power":true,"brightness_percent":10}'
target/release/homewire --bulbs .local/bulbs.json bulb-set bulb_1 '{"hue":210,"saturation_percent":70}'
target/release/homewire --bulbs .local/bulbs.json bulb-set bulb_1 '{"color_temperature_k":2700}'
target/release/homewire --bulbs .local/bulbs.json bulb-check
target/release/homewire --bulbs .local/bulbs.json watch-bulbs
```

Settings are absolute: `power` is boolean, `brightness_percent` is 1–100,
`hue` is 0–360 and must be paired with `saturation_percent` (0–100), and
`color_temperature_k` is 2500–6500, also checked against the bulb's live range.
Colour and white temperature cannot appear in the same change. Colour sets
the device's white-temperature field to zero. Brightness/colour changes do not
implicitly turn an off bulb on; include `power:true` when desired.

The runner reads status first, skips already-matching settings, sends at most
one write, waits the settling delay and verifies every requested field with
fresh status. Failed/rejected writes, missing reads or mismatches stop without
retrying. If overheating is true or unknown, only a power-off change is allowed.
Dynamic effects may prevent a fixed setting from verifying.

KLAP uses a random seed, validates the handshake proof and response signatures,
and rejects invalid padding or frames over 16 KiB plus encryption overhead.
Each operation gets a fresh short-lived session, with one total timeout covering
authentication, requests and the settling delay. No per-bulb worker is needed.
HTTP framing is bounded by the shared transport. These bulbs label their binary
responses `text/html`; KLAP authentication and decoding validate the payload.

`overheated` can be true, false, or null (unknown). Only reported true generates
a fault; unavailable or missing readings do not prove recovery. The shared
fault monitor requires matching observations at least 3 seconds apart for a
fault and 30 seconds apart for recovery. Confirmed transitions are saved before
output in `bulbs.state.json`; `bulbs.json.lock` excludes concurrent Homewire
owners using that config path. Endpoint/identity changes invalidate saved faults.
Candidates are held in the running process, so use `watch-bulbs`, automatic MCP
monitoring or repeated `bulb_check` calls in one server. `active_faults` returns
retained faults. Optional alert delivery is handled separately by the alerts ledger.
The ownership lock cannot coordinate with Home Assistant or the Tapo app.

## L900 LED strip

The L900 reuses the bulb adapter's KLAP v2 transport, verified controls,
ownership lock and overheating monitor. It adds no dependencies or worker
threads. Use `examples/lights.json` with `--lights PATH`; its top-level `lights`
array follows the bulb endpoint schema. A strip endpoint must explicitly set
`"model":"L900"`. Bulb endpoints can use `"model":"L430C"` or omit it.
Live model/device identity must match before controls. The L900 has a separate
2500–9000 K allowed range, also constrained by its live reported range; L430C
controls remain limited to their live range within 2500–6500 K.

```sh
target/release/homewire --lights .local/lights.json lights
target/release/homewire --lights .local/lights.json light-status led_strip
target/release/homewire --lights .local/lights.json light-set led_strip '{"power":false}'
target/release/homewire --lights .local/lights.json light-set led_strip '{"power":true,"brightness_percent":10}'
target/release/homewire --lights .local/lights.json light-set led_strip '{"hue":210,"saturation_percent":70}'
target/release/homewire --lights .local/lights.json light-set led_strip '{"color_temperature_k":7000}'
target/release/homewire --lights .local/lights.json light-check
target/release/homewire --lights .local/lights.json watch-lights
```

The shared KLAP transport retries an HTTP 400/403-rejected authentication
handshake once before application requests, within the original deadline.

Settings and fault confirmation follow the bulb rules. If a status or verification
read gets HTTP 403 after a valid handshake, Homewire refreshes the session and
retries that read once within the original total timeout. This handles observed
session invalidation while another client is present. It never retries a control
command, including one rejected with HTTP 403. A second read failure remains an
explicit error; a failed verification reports an uncertain outcome.
 `light-status` also
reports `model` and `effect_active`: true if an effect/music mode is active,
false if both are confirmed off, or null if unknown. L900 fixed brightness,
colour and white-temperature changes fail before controls if effects are
active/unknown, and fail verification if an effect becomes active/unknown during
the change. Disable effects/music mode in the Tapo app first. Power-only changes
can preserve the existing effect. Use typed settings for presets and restoring or
disabling the stored effect; arbitrary effects/music programming is not implemented.

`--lights` and `--bulbs` select the same manager and are mutually exclusive.
Either config accepts `lights` or legacy `bulbs`, but cannot contain both.
Use one config path for all clients of each light; separate overlapping configs
cannot share an ownership lock. With `--lights PATH serve --legacy-tools`, MCP advertises `light_list`,
`light_status`, `light_check`, and (with `--allow-control`) `light_apply`.
Status/apply arguments use `light` instead of `bulb`; responses use `light` and
`lights`. Existing `--bulbs`, `bulb-*`, `watch-bulbs` and MCP `bulb_*` calls remain
supported. Only one tool naming scheme is advertised, avoiding duplicate tools.

State paths follow the config name: `lights.state.json` and `lights.json.lock`
for `lights.json`. The expected L900 model forms part of the retained fault
identity. `light_check` reports retained `active_faults`, live statuses, unavailable
readings and debounced events. Optional alerts/companion adapters deliver
reported overheating notifications independently of the light protocol.

## L900 scheduling

The optional `automation` object selects one configured L900 and supplies its
local timezone and location:

```json
"automation": {
  "light": "led_strip",
  "timezone": "Etc/UTC",
  "latitude": 0.0,
  "longitude": 0.0,
  "curve": [[360, 20, 3000], [1080, 80, 5000]],
  "outside_curve": [20, 2700]
}
```

Replace the sample timezone and zero coordinates with your desired timezone and
location in the private config. The tracked lights template omits `automation`.
A valid IANA timezone, latitude within ±89°, longitude within ±180° and
`poll_seconds` of 5–30 are required. Omit `automation` to disable its configuration.
Having it configured does **not** enable controls: `light-plan` previews, while
`light-schedule` explicitly runs the rules. MCP uses `serve --allow-control
--light-schedule`; the scheduler starts only after the client's initialized
notification. It shares the existing event loop and config ownership lock.

The persistent Android daemon enables the same rules with `--light-schedule` in
its saved `service.args`, alongside `--allow-control`. Preserve other saved flags;
`--schedule` selects Meaco, so it does not substitute for the light flag. Validate
staged private schemas using `config-check` without server flags or `--api`, then
preview the staged light config using `light-plan` before cutover. API settings are
checked when the actual daemon starts. Rollback across a schema change requires
the matching old configs as well as the old executable; preserve attempt state.

Disable any competing light-curve and sunset automations in your existing
automation system before starting native scheduling. Use the single shared
lights config, rather than running separate bulb and strip processes.

`curve` is required: 2–16 strictly increasing `[minute_of_day, brightness_percent,
white_temperature_k]` points, minutes 0–1439, brightness 1–100%, temperature
2500–9000 K. `outside_curve` explicitly supplies `[brightness, temperature]`
before the first point and from the final point onward. Interpolation includes
local seconds and rounds ties to even. There is no compiled household curve.
At startup, on observed off→on transitions, and on each new local minute, it
adjusts an on strip if its settings differ. It never turns on an off strip for
curve adjustment.
External power-on detection is polled rather than pushed; the default interval
is 30 seconds. Changes are absolute and verified, with the usual settle delay;
no transition/fade duration is sent. Effects/music must be confirmed off.

Sunset uses local-date NOAA/Meeus solar equations with 90.833° zenith and two
iterations, without network calls or new runtime dependencies. It is an
approximation (NOAA describes roughly one-minute accuracy below ±72° latitude),
so this is not second-exact parity with HA's Astral calculation. A date with no
sunset has no sunset action. The process must observe a crossing within 60
seconds; startup after sunset does not catch up. At sunset it requests power-on
and the current curve settings together.

`lights.schedule.state.json` retains the device identity, last sunset attempt
date and last curve write minute. State is bounded, private and atomically
saved **before** querying a due sunset or issuing settings. Sunset query/write
failures do not replay that date, and curve attempts cannot repeat in the same
UTC minute, including after restart. Each subsequent minute is a fresh curve
assessment against live state; it does not blindly retransmit a failed command.
Persistence failures block controls. Connection/handshake failures before a
sunset attempt is recorded can be checked again inside the 60-second window;
no device write has occurred at that stage. Scheduler errors are logged to
stderr so MCP protocol output remains valid. Device identity changes discard
stale schedule memory. The reported-overheating monitor is independent of the
scheduler; use the alerts/companion adapters for notification delivery.

The sunset equations are documented by
[NOAA](https://gml.noaa.gov/grad/solcalc/calcdetails.html).

## Tapo P100/P110 plugs

The plug adapter shares the KLAP v2 HTTP transport, bounded messages and
read-only session refresh with the light adapter. No dependencies, background
threads, interpreter runtimes or extra servers are added. These configured P100
and P110 devices use HTTP port 80 and KLAP login version 2. Other models,
AES/login variants, discovery and pairing are not supported by this adapter.

Copy `examples/plugs.json` into a private config and replace the sample values.
The top-level `plugs` array contains 1–8 endpoints with:

- `name`: unique 1–32 ASCII letters, digits, hyphens or underscores.
- `model`: required `P100` or `P110`.
- `host`: the plug's IP; `port` defaults to 80.
- `device_id`: the exact reported identity, 1–64 alphanumeric characters.
- `auth_hash`: private 64-digit hexadecimal KLAP v2 hash, equivalent to decoded
  Home Assistant `credentials_hash`; do not use an account password here.
- `connect`: optional `IP:PORT` test/tunnel destination, preserving the actual
  Host header, expected model and device identity.

The file must have no group/other permissions (use `chmod 600`). Maximum size is
8 KiB. `timeout_seconds` defaults to 8 (1–30), `settle_seconds` to 2 (0–10, less
than timeout) and `poll_seconds` to 30 (5–300). The timeout covers the whole
handshake/read/write/settle/verification operation for each endpoint. Only
configured names can be accessed; tool arguments cannot change hosts or keys.

CLI commands are `plugs`, `plug-status NAME`, `plug-set NAME on|off`,
`plug-check`, and `watch-plugs`, all selected with `--plugs PATH`.
Legacy MCP (`serve --legacy-tools`) exposes `plug_list`, `plug_status {"plug":"metered_outlet"}`, and `plug_check`.
`serve --legacy-tools --allow-control` additionally advertises
`plug_apply {"plug":"relay","power":true}`. No settings/toggle strings or
extra arguments are accepted. Tools can share one process with all other
adapters. `serve --watch-plugs` polls automatically after MCP initialization
and sends diagnostics/events to stderr. Standalone `watch-plugs` emits events
on stdout. Monitoring never sends relay controls or outbound notifications.

`plug_status` reads fresh device information. P110 status additionally reads
`get_energy_usage` and `get_emeter_data` in the same bounded operation and
returns nullable `energy` fields: `power_w`, `voltage_v`, `current_a`,
`today_kwh`, `month_kwh` and `total_kwh`. Raw milliwatts/millivolts/milliamps
convert by 1000. Device milli-watt-hour counters convert to kWh by 1,000,000
and are preferred over coarser Wh counters (divide by 1000). Total is the
device's reported energy counter, whose reset/scope depends on firmware;
The plug adapter does not accumulate history or infer measurements. Optional
`history` records selected numeric entities in bounded disk rings. Missing fields
remain null; a reported zero stays zero; malformed, negative or wholly absent
measurements fail explicitly. Values are sequential reads rather than an
atomic metering snapshot. P100 energy is null because these devices provide
no metering. Auto-off output describes the active timer and raw remaining
seconds, not whether the saved auto-off feature is enabled.

`plug_apply` reads and verifies the configured model/identity, skips a matching
relay state, then sends one `set_device_info {"device_on":BOOL}` command,
waits for settling and verifies fresh relay state. A rejected/unverified
command is never replayed. A handshake rejected with HTTP 400/403 can be attempted again once before any
application request is sent, using the same total deadline. Only status/metering
reads rejected with HTTP 403 can refresh the session and repeat that read once within the original timeout.
P110 power-on requires a confirmed `normal` protection status, and verification
must still show normal. Reported `overloaded`, missing or unrecognized status
blocks changing power to on; power-off remains available. P100 has no reported
overload feature, so `overloaded` is null. Relay commands preserve protection,
auto-off, LED and firmware settings. Explicit typed settings can change the
advertised configuration; firmware installation and factory reset are unavailable.

`plug-check` reads only device information, without energy queries, and returns
statuses, unavailable readings, `active_faults` and debounced events. P110
`power_protection_status` maps `overloaded` to an `overloaded` fault and `normal`
to a known clear reading. Unknown/missing/unavailable readings never prove
recovery. The shared monitor confirms faults after 3 seconds and recovery
after 30 seconds; both require another confirming poll, so the default 30-second
poll cadence can delay alerts beyond those debounce durations. These are
reported device faults, independent of watts/amps or connection availability.
State is bounded, atomically saved before emitting, and retained across
restarts in `plugs.state.json`. Device/model changes discard stale state.
`plugs.json.lock` excludes another process using the same config path;
separate overlapping configs and HA/Tapo clients cannot share that lock.

The example contains a P100 relay, a P110 metered outlet and a P110 TV outlet.
Choose your own names, endpoints and credentials; it is not a household inventory.
The relay adapter does not coordinate media-device shutdown. Use the explicit TV
workflow when sharing outlet power with media devices, and validate the sequence
before transferring ownership from another automation system.

Protocol semantics follow python-kasa's
[energy module](https://github.com/python-kasa/python-kasa/blob/master/kasa/smart/modules/energy.py)
and [power-protection module](https://github.com/python-kasa/python-kasa/blob/master/kasa/smart/modules/powerprotection.py).

## Samsung TV and DIGI R2A

The Samsung adapter targets the **UE40H6200** with encrypted Socket.IO on port
8000, using an existing PIN pairing token/session. It verifies `modelName` and
`device.id` from a fresh HTTP `/api/v2/` response on port 8001 before remote
controls. H/J-series pairing is distinct from the legacy port-55000 protocol.
Other Samsung models/protocols are not implemented.

Samsung configuration (`examples/samsung.json`):

- `host`: literal LAN IP; `port` defaults to 8000; `info_port` defaults to 8001.
- `device_id`: the exact `device.id` from `/api/v2/`, not the UPnP UDN or MAC.
- `mac`: six hexadecimal octets; `wake_address`: UDP destination, normally the
  device LAN's broadcast address with port 9. WOL must originate on a network
  that can reach that destination; TCP SSH forwards do not forward WOL.
- `token`: the 32-hex-character AES-128 pairing token from the existing Samsung
  HA config entry; `session_id`: its unsigned numeric pairing session identifier.
- `off_key`: `KEY_POWEROFF` by default; `KEY_POWER` is an explicit compatibility
  alternative. It is sent only after a fresh on reading, never as a blind toggle.
  The actual H6200 off-key behavior still needs a deliberate live shutdown test.
- `timeout_seconds`: 1–30, default 8; `verify_seconds`: 1–60, default 20.
- Optional `connect` and `info_connect`: `IP:PORT` TCP destinations for tests or
  tunnels. The original configured host remains the HTTP/WebSocket authority.

`samsung-status` / `samsung_status` takes no arguments. If `PowerState` is present,
its on/standby/off value is used. Otherwise a verified identity plus an active
companion channel yields `power: true` with explicit evidence. Connection failures
are errors, not off readings. The channel join exchanges no remote key.

`samsung-set on|off` / `samsung_apply {"power":BOOL}` wakes with one magic packet
and polls within the verification interval, or sends one encrypted shutdown key.
Controls use fresh state; already-correct states skip writes. A shutdown attempt
is saved before sending and prevents another shutdown/key for 120 seconds,
including after restart. Remote/channel failure after sending cannot prove physical
off: the response then reports `verified: false`. Check `verified` before assuming
an outcome. This adapter does not send the old HA fallback of two power keys.

`samsung-key KEY` / `samsung_key {"key":"hdmi"}` accepts `home` (Samsung menu),
`back`, `up`, `down`, `left`, `right`, `enter`, `hdmi`, `volume_up`, `volume_down`,
`mute`, `play`, `pause`. Keys are sent once over a bounded masked WebSocket with
AES-128 ECB/PKCS#7 command payloads. Generic power keys are not exposed. These
navigation/media keys return `sent: true, verified: false`; that is transport
completion rather than proof of a resulting screen, source or volume.

The DIGI adapter targets **ZTE DIGI R2A**, using paired Android TV Remote v2 on
TLS port 6466. Its config (`examples/digi.json`) contains:

- `host`: literal IP; `port` defaults to 6466; optional `connect` for TCP tunneling.
- `client_cert`, `client_key`: private PEM paths, resolved relative to the config.
  Reuse HA's `.storage/androidtv_remote_cert.pem` and `androidtv_remote_key.pem`.
  Both must be private (`chmod 600`) and at most 8 KiB. No keys are generated or
  pairing prompts initiated during normal commands.
- `server_cert_sha256`: 64 hex characters, SHA-256 of the server's leaf DER
  certificate obtained from the paired device over a trusted connection.
  The certificate is pinned and TLS handshake signatures are verified. There is
  no accept-all verifier or silent trust-on-first-use. Certificate changes require
  explicit configuration update after identity verification.
- `timeout_seconds`: 1–30, default 8; `verify_seconds`: 1–60, default 20.

One way to obtain the pin during setup, on a trusted LAN, is:

```sh
openssl s_client -connect DIGI_IP:6466 -cert .local/digi-cert.pem -key .local/digi-key.pem </dev/null 2>/dev/null | openssl x509 -outform DER | openssl dgst -sha256
```

The adapter verifies the pinned certificate plus live model/vendor, negotiates
ping/key/power plus supported app-link/text features, and waits for a `remote_start` power reading. An
absent proto3 `started` field means false only inside a present `remote_start`;
identity/connection alone never defaults to off. Messages are limited to 8 KiB,
128 fields and 128 frames per state wait, with a socket deadline. Unknown valid
fields are skipped; malformed/truncated/overflowing fields fail.

`digi-status` / `digi_status` takes no arguments. `digi-set on|off` /
`digi_apply {"power":BOOL}` sends absolute `KEYCODE_WAKEUP` (224) or `KEYCODE_SLEEP`
(223) once, then verifies a fresh reported state on that same TLS session. Already
correct settings send no key. Errors/disconnects after controls are not reconnected
or replayed. `digi-key KEY` / `digi_key {"key":"home"}` supports `home`, `back`,
`up`, `down`, `left`, `right`, `enter`, `volume_up`, `volume_down`, `mute`,
`play_pause`, `play`, `pause`, `channel_up`, `channel_down`. It sends one short key
and returns an unverified delivery result. `entity_configure` supports HTTP(S)
`app_link` launch and `text` (up to 512 bytes) in a live focused input field.
Requests are sent once and remain unverified. TV voice streaming and ADB are
not implemented. Host-only `scripts/android/pair_digi.py` performs PIN pairing
without HA; existing credentials remain the normal runtime path.

Both managers hold exclusive config locks. Config/credential parser errors omit
secret values; credentials are not tool arguments or listed by MCP. They build
TLS/session state on demand and discard it after each operation. There are no
persistent connections, background threads, session caches or polling tasks. The
shared synchronous deadline socket now serves HTTP, Samsung WebSocket and DIGI
TLS. Rustls uses only its Ring provider, std and TLS 1.2/1.3; no async runtime,
OpenSSL runtime linkage, full protobuf runtime or interpreter is required.

Simulated-device tests exercise encrypted Samsung keys/WOL, shutdown intent
persistence, DIGI mutual TLS/pin rejection, wake/sleep verification, malformed
frames and workflow failure ordering. Validate physical shutdown and the complete
power sequence against the firmware and wiring in each installation.

### Shared TV outlet workflow

`examples/tv.json` references `samsung`, `digi`, and `plugs` config paths (relative
to its own directory), plus `plug` (normally `tv`) and the required
`shutdown_policy: "fixed_delay"`, plus required
`timings: {"startup_seconds": 5, "shutdown_seconds": 5, "ready_seconds": 120}`.
Startup/shutdown delays must be 1–60 seconds and readiness 1–120 seconds; the
example values are explicit settings, not implicit defaults. This remains a
statically compiled Samsung + Android TV + outlet workflow. `--tv PATH` opens/locks those managers and the
workflow config. Do not also load the same underlying configs separately into
that process; another config owner fails at startup. The catalog borrows those
child managers for their normal Samsung, DIGI and plug tools; `--tv` can also be
used with `plugs`, `samsung-status`, `digi-status`, and `serve --watch-plugs`. There
are no duplicated instances, reference-counted handles or wrapper transports.
Use standard MCP `routine_run` (`tv_on` / `tv_off`), CLI `tv-set`, or legacy
`tv_apply` for coordinated power. Direct child power requires explicit
`--maintenance`; navigation keys and reads remain available.
Other unrelated adapters
can still share its MCP process.

`tv-status` / `tv_status` returns the outlet and both media readings, with explicit
errors for unavailable devices. `tv-set on|off` / `tv_apply {"power":BOOL}` runs
serialized deterministic steps:

1. **On:** verify the outlet; switch on if needed and wait `startup_seconds`. Request
   one Samsung wake and verify on, then wait up to `ready_seconds` for DIGI to become
   readable. Read attempts can finish within their own configured deadline after
   the boot wait expires. Apply/verify DIGI wake. Errors leave the restored outlet on.
2. **Off:** skip if the outlet is already off. Apply/verify DIGI sleep first; unknown
   DIGI state aborts. Request Samsung shutdown once, wait `shutdown_seconds`, then switch
   off and verify the shared outlet. Any earlier failure leaves the outlet on.

The fixed-delay shutdown policy follows the existing outlet cutoff intent; it
explicitly allows cutoff without proof of physical Samsung off. This is stricter
than HA about DIGI sleep, executes sequentially rather than in parallel, and sends
one WOL packet rather than HA's six attempts. Successful responses describe the
workflow completion, not Samsung physical-off confirmation.

An attempt is durably saved before any step. A failed/interrupted attempt blocks
workflow replay for 180 seconds across restart; success clears it. Individual
Samsung shutdown protection still applies. No failed controls are rolled back,
automatically retried, or caught up. Long workflows occupy the single request
thread, so clients need sufficient timeouts; queued requests execute afterwards.
There is no listener for HA service events. Keep both HA TV rules disabled only
when deliberately cutting over ownership to explicit Homewire/agent commands.

Protocol references: [Samsung encrypted remote](https://github.com/xchwarze/samsung-tv-ws-api/tree/master/samsungtvws/encrypted),
[HA Samsung bridge](https://github.com/home-assistant/core/blob/dev/homeassistant/components/samsungtv/bridge.py),
[Android TV remote protocol](https://github.com/tronikos/androidtvremote2/blob/main/src/androidtvremote2/remotemessage.proto).

## MCP and ZeroClaw

Start a local stdio MCP server:

```sh
target/release/homewire --config .local/meaco.json serve
```

### Standard entities and routines

The default MCP interface follows Home Assistant's separation of protocol
integrations, capability entities and household scripts. It uses a static Rust
catalog and typed adapters; there is no retained entity-state database or
allocated entity registry. Category tools are selected from adapters' static
`Support` flags. New adapters can reuse an existing category without adding MCP
routing branches.

| Tool | Arguments | Availability |
| --- | --- | --- |
| `entity_list` | Optional `domain`, `device`, `offset`, `limit`, `details` | Paginated discovery; no network I/O |
| `entity_options` | `{"entity":"light.bulb_1"}` | Live choices where implemented |
| `entity_configure` | `{"entity":"humidifier.meaco","settings":{"target_humidity":55}}` | Advertised typed settings + control flag |
| `media_play` | `{"entity":"media_player.cast_player","media":{"url":"https://example.org/music.mp3","content_type":"audio/mpeg"}}` | Cast/DLNA + control flag |
| `media_command` | `{"entity":"media_player.dlna_player","command":{"action":"volume","percent":20}}` | Playback + control flag |
| `entity_status` | `{"entities":["light.bulb_1","sensor.awair_co2"]}` | Fresh batched reads |
| `light_set` | `{"entity":"light.led_strip","settings":{"power":true,"brightness_percent":30}}` | Lights + control flag |
| `switch_set` | `{"entity":"switch.tv","power":false}` | Plugs + control flag; TV outlet requires maintenance |
| `media_power` | `{"entity":"media_player.digi","power":true}` | Media + control flag; routine-owned power requires maintenance |
| `remote_key` | `{"entity":"media_player.samsung","key":"hdmi"}` | Media + control flag |
| `routine_list` | `{}` | Configured Meaco/TV routines |
| `routine_plan` | `{"routine":"meaco_night"}` | Configured routines; no controls |
| `routine_run` | `{"routine":"tv_on"}` | Configured routines + control flag |

IDs use `<domain>.<configured-label>` for lights, switches and printers. Singleton
IDs are `humidifier.meaco`, `media_player.samsung`, and `media_player.digi`.
Awair contributes six sensor entities: `sensor.awair_temperature`,
`awair_humidity`, `awair_co2`, `awair_tvoc`, `awair_pm25`, `awair_score` (each with
`sensor.` prefix). P110 adds `<label>_power`, `_voltage`, `_current`,
`_today_energy`, `_month_energy`, `_total_energy` under `sensor.`. Their units are
explicit. A physical `device` groups all its entities; adding entities does not
create connections, threads, copies of credentials or separate managers.

Discovery reports model capability envelopes, remote-key choices and power
ownership. Light temperature ranges are model envelopes; actual live ranges
are checked before controls. Light settings can disable or restore the device's stored effect; L900 fixed
settings still require effects/music mode confirmed off. Meaco also exposes typed
humidity, modes, child lock and timer settings. `meaco_day`, `meaco_night` and
`meaco_off` retain the deterministic household sequences.
TV routines are `tv_on` and `tv_off`.

Status accepts 1–32 unique IDs, resolves the entire batch before I/O, and reads
each physical device once per request. Results preserve input order and include
`available`, `observed_at`, `state` and sensor `measured_at` when provided by the
device. Errors return `available:false`, `state:null` and an explicit error;
missing optional measurements remain null. Readable media power may still be
unknown. Snapshots exist only for the current batch. Explicit status requests
and background fault monitors each request fresh readings.

Meaco plans query live state. TV plans describe the configured conditional
sequence without querying devices and report `live_state_checked:false`;
neither plan guarantees the subsequent outcome. Runs preserve existing durable
attempt guards and control verification.

When `--tv` is configured, its Samsung, DIGI and entire plug adapter are borrowed
from one owner. Loading separate adapters for those families is rejected, even
with different config paths. Direct power commands for the two media devices
and the TV's selected outlet are blocked in CLI and both MCP interfaces.
`--maintenance` explicitly permits those direct controls, with device validation
and verification still enforced. Other outlets, reads and navigation keys remain
available. MCP writes also require `--allow-control`.

Existing CLI commands are preserved. Existing MCP consumers should add
`--legacy-tools` to their server arguments until migrated. Standard mode
advertises only capabilities supported by loaded adapters; the complete household
deployment exposes 21 tools with control enabled. It rejects legacy tool names.
Legacy mode retains family lists/status/check/apply/key commands.
Fault monitoring remains available through the existing watch flags; it reports
events to stderr instead of adding monitoring checks to the normal agent tool set.

### Legacy adapter tools

Add `serve --legacy-tools` for the original interface described below.

With `--config` it exposes `meaco_status` and `meaco_plan`. Add `--allow-control`
to expose `meaco_apply`. Plan/apply take `{"operation":"day"}`, `night`, or `off`.
With `--printers` it exposes `printer_list`, `printer_status` (takes
`{"printer":"brother"}` or `hp`), and `printer_check` (takes no arguments).
With `--awair` it exposes `awair_status` (takes no arguments).
With `--bulbs` it exposes `bulb_list` and `bulb_check` (no arguments), plus
`bulb_status` (`{"bulb":"bulb_1"}`). Add `--allow-control` for `bulb_apply`,
which takes `{"bulb":"bulb_1","settings":{"power":true,"brightness_percent":10}}`.
With `--lights`, the same tools are advertised as `light_list`, `light_status`,
`light_check` and `light_apply` (control flag required). For example,
`{"light":"led_strip","settings":{"brightness_percent":10}}` applies to the strip.
With `--samsung` or `--digi`, the corresponding `_status` tool takes no arguments;
`--allow-control` adds `_apply` (`{"power":BOOL}`) and `_key` (`{"key":"home"}`).
With `--tv`, `tv_status` is read-only and `tv_apply` requires the control flag;
the underlying Samsung, DIGI and plug tools are also advertised. Their direct
power commands remain subject to routine ownership; `--legacy-tools` does not
bypass it.
Only configured device tools are advertised. Printer and Awair tools remain read-only.

```sh
# Printers only; no Meaco key needed:
target/release/homewire --printers .local/printers.json serve
# Both families, with automatic printer monitoring:
target/release/homewire --config .local/meaco.json --printers .local/printers.json serve --watch-printers
# All adapters in one process, read-only by default:
target/release/homewire --config .local/meaco.json --printers .local/printers.json --awair .local/awair.json --lights .local/lights.json serve --watch-printers --watch-lights
```

Repeated `printer_check` calls in the same server confirm and deduplicate events.
`--watch-printers` polls automatically and writes events/diagnostics to stderr,
keeping stdout clean. Automatic polling/scheduling begins after the initialized
notification; the first automatic printer poll occurs within the configured
interval. Device queries are sequential: allow client timeouts for multiple
printer requests and Meaco's 20-second settling delay per step. Cancellation
notifications do not abort an in-flight sequence.
`--watch-bulbs` follows the same initialization and stderr behavior for
overheating events. Its first poll occurs within the bulb poll interval.
`--watch-lights` is the shared alias for bulbs and the L900 strip.

The server implements initialization, ping, tool listing and tool calls for
MCP 2024-11-05, 2025-03-26, 2025-06-18, and 2025-11-25. Messages are newline-delimited JSON-RPC,
limited to 64 KiB. Device operations run sequentially; stdout contains only
protocol messages. It does not implement HTTP transport or agent inference.

For ZeroClaw 0.8.x, add a server to an MCP bundle, then reference that bundle
from the appropriate agent. Use absolute executable and config paths; start
without `--allow-control` during development. See
[ZeroClaw's configuration reference](https://docs.zeroclaw.com/v0.8.3/en/reference/config.html#mcp_bundles)
for the exact schema supported by your installed version.

## Scheduling

The native scheduler reads the Meaco JSON's explicit `schedule` and `presets`.
The timezone is parsed once at startup; day/night times have minute resolution
and follow that timezone's daylight-saving rules. Missing schedule settings cause
an enabling command to fail at startup, before any device controls.

```sh
# Standalone scheduler:
target/release/homewire --config .local/meaco.json schedule

# Or one process combining MCP and scheduling:
target/release/homewire --config .local/meaco.json serve --allow-control --schedule
```

Disable the corresponding Home Assistant rules before enabling this scheduler.
Starting it outside a scheduled minute does not change the device or catch up
missed runs. It attempts each rule once per local date, persists that attempt
before control, and does not replay failed or interrupted runs after restart.
Failures are logged to stderr; independent push notification delivery is not
implemented yet. The MCP server exits on stdin EOF; its MCP client must keep
stdin open. Supervise the standalone scheduler when running without an MCP client.

## Implementation and validation

- `device`: standard adapter/monitoring interfaces, command definitions and one
  compile-time catalog generating concrete storage, discovery and dispatch.
- `config` / `bundle`: private typed adapter inputs, bounded unified startup
  loading, durable state and exclusive device ownership.
- `tuya`: bounded TCP frames, AES/CRC handling; no interpreter subprocesses.
- `transport`: one deadline-bound socket for HTTP, WebSocket and TLS.
- `http`: bounded HTTP framing; 1 KiB reader buffer, 8 KiB header budget, up to
  64 KiB bodies, total request deadline. KLAP sets tighter response limits.
- `ipp`: read-only IPP encoding and attribute decoding.
- `printer`: endpoints, supply levels, durable fault monitoring; no worker thread.
- `awair`: local API reads, explicit units, validation and timestamp freshness.
- `klap`: authenticated Tapo LAN handshake and bounded AES-CBC messages.
- `bulb`: shared L430C/L900 state, model ranges, verified settings and overheating reads.
- `plug`: P100/P110 relays, P110 metering, overload guards and retained fault events.
- `samsung` / `websocket`: encrypted H6200 remote, WOL and guarded shutdown.
- `androidtv` / `protobuf`: pinned mutual TLS and the bounded DIGI remote subset.
- `routine` / `routine::tv`: named routines and the shared-outlet workflow with
  statically owned/borrowed child managers; `tv` remains a Rust compatibility re-export.
- `entity`: standard capability tools, borrowed metadata and per-request snapshot sharing.
- `media`: shared small protocol helpers and schemas for media adapters.
- `light_schedule` / `solar`: optional native L900 curve and local sunset calculation.
- `fault`: one statically dispatched polling implementation for printers, lights
  and plugs, shared confirmation and durable deduplication. Background monitoring
  returns events/errors without retaining status snapshots or copying active
  faults. Snapshot checks still return the documented full report.
- `meaco`: physical device state/protocol adapter, configuration/state ownership
  and durable schedule attempts; `routine::meaco` owns day/night/off planning and
  verified sequences. Existing Rust operation exports remain compatible.
- `schedule`: configured clock rules with daylight-saving support.
- `mcp`: framing, initialization, control permissions and tool presentation;
  receives static standard category/routine metadata, or adapter metadata in legacy mode.
- `naming`: presentation-only light/bulb compatibility aliases.
- `main`: CLI parsing, concrete adapter composition, timers and output routing.

MCP reads stdin directly on the main thread when no background flags are enabled.
Schedules or monitors enable one bounded stdin-reader thread; controls and
scheduling still share the main thread. Short-lived device sockets need no
background connection worker. Queues and protocol frames are bounded. Release
builds strip symbols, enable LTO and abort on unexpected panics; ordinary
protocol/device errors still return through `Result`. A panic terminates the
process without stack unwinding or running destructors. OS configuration locks
are released when the process exits; persistence still precedes controls/events.
Measure idle/peak resident memory on the actual target; desktop numbers are not
Android guarantees.

### Static device adapters

Device families implement `device::Adapter`: private configuration loading,
legacy `TOOLS` and validation, static `SUPPORT` category flags, borrowed
`entities()` descriptors, `read_device(index)` snapshots and optional
`control_device(index, command, arguments)` controls. Read-only adapters inherit
a rejecting control implementation. Protocol parsing, identity checks, live
ranges, uncertain-outcome guards and verified writes stay inside adapters.

`Entity` descriptors contain a domain, configured label, a tiny physical-device
route, optional snapshot field/unit, capabilities and routine ownership.
Descriptors borrow existing configuration; the application retains no separate
entity objects or state cache. IDs remain stable while configured labels remain
stable. `Devices::validate()` checks ownership and duplicate entity IDs at
startup, without heap allocation. Embedding callers should validate after
constructing `Devices` and use `entity::call` for validated standard commands.

`static_adapters!` in `src/device.rs` generates `Family`, inline
`Option<ConcreteAdapter>` storage, category support, discovery and dispatch from
one list. It adds no trait objects, plugin loader, interpreter or worker.
`plugs/Plugs: crate::plug::Manager => tv.plugs` also declares a statically borrowed
fallback from the workflow owner, preserving one manager/configuration lock.

Standard schema definitions live with capability tools and routines. Adapter
schemas remain available for CLI/legacy compatibility. Both paths reject invalid
arguments before device I/O; MCP independently gates all write tools through
`Hints.read_only` and `--allow-control`. Schemas are built for a response and
released, with no schema cache. `mcp::handle_entities` is the application entry
point; `handle_selected`, `handle_configured`, and `ToolSet` retain legacy Rust
compatibility. `--legacy-tools` selects that interface in the executable.

`device::Monitored` supplies optional typed fault monitoring. One physical
adapter owns a device's monitor state, even when several entities describe it.
Batched entity reads share a snapshot within the request; background monitoring
discards snapshots after examining them. Durable confirmation/attempt state
remains bounded and persists before publishing events or sending controls.

Background MCP waits until the earliest enabled poll or light-schedule deadline,
or the next minute boundary for fixed-clock Meaco rules. Input immediately wakes
the receiver. Before initialization it blocks on input without periodic wakeups.
Standalone schedules use the same deadline calculations. Idle MCP without
background flags still blocks directly on stdin with one thread. Meaco rules
retain minute-wide due windows and no late catch-up; idle clock checks follow minute boundaries
when that scheduler is enabled. Serialized device operations can delay jobs.

To add a device family:

1. Implement bounded configuration and protocol behavior in its adapter module.
   Declare entities and optional controls using existing domains/capabilities;
   keep credentials out of metadata. Reuse transports where the protocol matches.
2. Implement `Adapter` and select static category `SUPPORT`. Register the concrete
   type and `Family` variant in `static_adapters!`. Existing category tools need
   no MCP changes; introduce a new category only for materially different behavior.
3. Implement `Adapter::from_source` using the shared typed decoder; declare
   `CONFIG_SECTION = "services"` for a service (hardware defaults to `adapters`).
   Unified configuration wiring follows the static catalog automatically. Keep
   separate-file CLI flags only when needed for compatibility. Use `Monitored`
   for real fault monitoring.
   Compose multi-device behavior in `routine`, keeping ordering and durable guards
   out of agent prompts. New routines must provide explicit discovery and execution.
4. Add protocol/behavior tests, update docs, run formatting/tests/Clippy and
   allocation/resident-memory checks. Verify batch sharing, argument validation,
   unknown-state behavior, ownership and synthetic control outcomes.
5. Rebuild the source catalog for the target OS/ABI. Modules are statically
   compiled; OS libraries are linked according to the target. A host build does
   not produce an Android executable.

The 2026-10-08 entity/routine refactor was checked with 158 synthetic tests,
formatting, Clippy and an optimized release build. Awair multi-sensor requests
and P110 relay/meter requests verify one physical snapshot per batch; a second
request fetches again. Standard switch controls verify the existing single-write
sequence. Tests also cover unknown targets before I/O, unavailable/null results,
MCP control gates, TV power ownership and deadline selection. One lock-reopen
test failed once under parallel execution; five subsequent plug-suite runs and
the final complete suite passed. No lock implementation was changed in this refactor.

On macOS ARM64, the same seven physical adapter families were measured before
(commit `ac7f16e`) and after the refactor. Each column is the median of three
runs of `python3 scripts/measure_memory.py --media`, with 1,000 metadata requests,
synthetic credentials and no device traffic, schedules or monitoring:

| Measurement | Before | Standard entity/routine interface |
| --- | --- | --- |
| Initialized idle RSS | 3,248 KiB | 3,360 KiB |
| Largest sampled RSS per run | 3,664 KiB | 3,632 KiB |
| Release executable | 2,670,592 bytes | 2,720,336 bytes |

The new architecture adds 112 KiB to median idle residency in this sample;
metadata-request residency remains around 3.5 MiB. Running the same new binary
with `--legacy-tools` measured 3,328 KiB median idle and 3,712 KiB median largest
sample. These are desktop samples, not true peaks or Android measurements.

The allocation probe compares both tool interfaces with controls enabled for
Meaco, lights, plugs, printers and Awair, excluding request construction:
legacy discovery uses 398 allocations / 49,805 requested bytes for 15 tools;
standard discovery uses 244 / 30,378 for seven tools (about 39% fewer allocations
and requested bytes). This measures transient allocation requests, not live heap.
Existing zero-allocation checks for unchanged faults, in-place KLAP decryption
and valid configuration remain intact. Whole-device status JSON moves into the
reply after scalar projections, avoiding a redundant whole-tree clone.

Background waits now follow enabled deadlines (normally a 30-second poll or a
minute clock boundary), replacing the one-second receiver timeout. Idle MCP
without background flags retains no periodic wakeup. No measured CPU percentage
or battery improvement is claimed; target CPU/RSS, screen-off and reboot testing
remain pending.

The 2026-10-08 modularization compared against `00f926f`: macOS ARM64 idle RSS
remained 3,152 KiB and the median largest sampled RSS remained 3,440 KiB across
three runs. The executable changed from 1,957,584 to 1,957,616 bytes (+32 bytes).
The full writable light-alias tool list still uses 398 allocations/49,805
requested bytes; configuration validation and unchanged-fault decisions still
allocate nothing. A synthetic comparison of 26,240 MCP tool-list/call responses
(including invalid arguments, disabled families, control permissions and aliases)
matched the previous implementation exactly. No real devices were queried or
controlled. These are desktop measurements, not Android guarantees.

### Memory and allocation checks

The 2026-10-08 media build measured **3,248 KiB idle / 3,648 KiB median largest
sample** across three macOS ARM64 harness trials, with all seven device families
configured (`python3 scripts/measure_memory.py --media`, synthetic credentials,
1,000 tool-list requests, no device I/O). The release binary is 2,670,592 bytes,
up from 1,957,616 at `aadf986`; the added native TLS implementation accounts for
most of the added dependency surface. Cargo.lock keeps 23 new crates, including
target-specific/build dependencies, at versions older than seven days.

A separate live **TV workflow** process (Samsung, DIGI and one P110 manager)
measured 3,312 KiB idle, 3,408 KiB largest sampled RSS, and 3,280 KiB after 30
read-only MCP calls, sampling RSS every 50 ms. Neither these samples nor the
metadata harness prove a true peak or Android footprint. No media adapter retains
TLS configs, sockets or protobuf messages between operations. The deployment
now has a tested Android build and phone idle sample; real peaks and longer
background-operation validation remain to measure. See [Android deployment](#android-deployment).


All adapters share one process. No adapter adds a thread, async runtime, session cache
or task queue. With no background flags, MCP has no reader thread, input queue
or periodic wakeup. With background jobs, the reader has a 128 KiB stack and a
queue of one message. Both input modes use a 1 KiB reader buffer and cap messages
at 64 KiB. Private configuration and state files are
capped at 8 KiB, including Meaco state writes. Device groups allow at most eight
endpoints each. Readings and replies are released between operations.

Unchanged fault observations borrow saved values and allocate nothing. A poll
builds endpoint identities and copies durable memory only for confirmed
transitions; the complete batch is persisted before published memory/events
change. If saving fails, published memory stays intact, no event is returned,
and confirmed candidates may be checked again on the next poll. Unknown or
unavailable readings cancel pending confirmation without clearing saved faults.
Configuration locking and atomic private writes use one implementation shared
with Meaco and the light scheduler.

KLAP encryption allocates one exactly sized frame; the HTTP response is
authenticated and decrypted in place. A typed reply envelope moves the
decoded result into the adapter rather than cloning its JSON tree. Handshake
responses are limited to 48 bytes and zero bytes respectively; application
responses to 16 KiB plus 48 bytes of overhead. Error responses preserve their
HTTP status without reading their bodies. HTTP header comparisons avoid
lowercase-string allocations. MCP moves arguments and results into response
envelopes; compatibility still requires both text and structured content. One
static tool registry supplies advertising and validation. Tool-list schemas and
annotations are moved into replies rather than cloned. Fault event diagnostics
serialize directly to stderr, avoiding intermediate JSON strings. Name/identity
validation uses stack storage for each bounded group rather than allocated
sets. Chrono uses UTC clock support without host-local timezone detection;
explicit IANA timezone scheduling and daylight-saving behavior remain supported.

Run the development allocation probe (synthetic data, no device I/O):

```sh
cargo run --release --locked --example allocation_probe
```

The probe asserts allocation counts, reporting requested allocation bytes;
these numbers are neither live heap usage nor RSS. On macOS ARM64 (2026-10-07),
an unchanged reported fault fell from 2 allocations/34 requested bytes to zero;
encrypting `get_device_info` fell from 4 allocations/180 requested bytes to
1 allocation/64 requested bytes. In-place decryption uses zero allocations.
Only the fault decision is allocation-free; network reads, decoded statuses and
MCP replies still allocate bounded temporary storage.

For repeatable process samples, the optional development script requires Python
3 and `ps` on macOS/Linux. Python is not part of the deployed runtime:

```sh
cargo build --release --locked
python3 scripts/measure_memory.py
# Include the two media adapters; compare the compatibility tool interface:
python3 scripts/measure_memory.py --media
python3 scripts/measure_memory.py --media --legacy-tools
# Compare against a saved executable built before changes:
python3 scripts/measure_memory.py --binary /path/to/previous/homewire
```

The script creates private synthetic configuration for Meaco, two printers,
Awair, three lights and eight plugs, with scheduling/monitoring disabled. It
initializes MCP and makes 1,000 tool-list requests in each of three runs, sampling
RSS every 100 requests. It sends no network queries or controls. Samples include
the whole process, but do not measure a true peak, real device traffic or Android
memory. Target-device idle/peak profiling remains required before deployment.

In the 2026-10-07 macOS ARM64 comparison against commit `51dd544`, the process
measurements were modestly lower. RSS rows below are medians of three runs;
the second row uses each run's largest sample:

| Measurement | Before | After |
| --- | --- | --- |
| Initialized idle RSS | 6,928 KiB | 6,880 KiB |
| Largest sampled RSS per run | 7,392 KiB | 7,264 KiB |
| Release executable size | 2,089,856 bytes | 2,073,344 bytes |

A further 2026-10-08 pass compared against commit `16efa93`, using the same
macOS ARM64 harness. RSS values are medians of three runs:

| Measurement | Before | After |
| --- | --- | --- |
| Initialized idle RSS | 6,880 KiB | 3,152 KiB |
| Largest sampled RSS per run | 7,248 KiB | 3,472 KiB |
| Release executable size | 2,073,344 bytes | 1,957,584 bytes |
| Full writable light-alias tool list: allocations | 555 | 398 |
| Full tool list: requested allocation bytes | 70,548 | 49,805 |

This idle comparison disables background jobs; those modes still need a reader
thread. Runtime memory includes OS/platform overhead and will differ on Android.
The release panic setting alone saved 115,856 bytes in an intermediate build.
Changing Chrono features removed 28 packages from the lockfile, mainly unused
host-timezone/platform dependencies, without adding dependencies. Validation of
all three example endpoint groups allocates nothing after deserialization; MCP
replies still allocate temporary JSON trees. The allocation probe checks both
paths alongside unchanged faults and KLAP frames.

All 129 tests pass, including both MCP input modes, invalid JSON, notification
silence, final messages without a newline, oversized/non-UTF-8 input, atomic
batch-save failures, watch/check event deduplication and unknown readings during
recovery. All 128 family/control/alias tool-list combinations were compared
against the prior implementation and matched exactly. Formatting and Clippy
checks also pass. No device was controlled during these synthetic measurements.

Protocol tests cover independent Tuya/KLAP fixtures, bounded HTTP/IPP parsing,
stale sensor data, signature/identity rejection, uncertain-control outcomes and
durable fault recovery. Keep physical-device acceptance results in private
deployment records: verify reads, explicit controls, schedules and recovery on
the target before replacing another automation controller.

Android builds target `aarch64-linux-android` using a pinned Android NDK.
See [Android deployment](#android-deployment) for target installation and checks.

The Meaco data-point mapping follows
[Tuya Local's device definition](https://github.com/make-all/tuya-local/blob/main/custom_components/tuya_local/devices/meaco_aretetwo_dehumidifier.yaml).
Printer encoding follows [RFC 8010](https://www.rfc-editor.org/rfc/rfc8010.html);
state/reason semantics follow [RFC 8011](https://www.rfc-editor.org/rfc/rfc8011.html).
The Tuya wire protocol follows the
[TinyTuya protocol documentation](https://github.com/jasonacox/tinytuya/blob/master/PROTOCOL.md).
Tuya 3.3's CRC detects corruption rather than authenticating the peer; use a
trusted local network. Device keys are never logged.

## Persistent daemon and MCP bridge

`serve` remains the original standalone stdio server and exits on stdin EOF.
Use `daemon SOCKET` for an independent device owner:

```sh
mkdir -p .local/run
chmod 700 .local/run
target/release/homewire --config .local/meaco.json daemon .local/run/homewire.sock
# In another terminal; no device configs or credentials needed by the client:
target/release/homewire bridge .local/run/homewire.sock
```

Daemon accepts the same control, legacy, maintenance, schedule and watch flags
as `serve`. Read-only is the default. Its background jobs start at daemon startup,
independently of MCP initialization. Standard MCP requests and permission checks
are identical across transports. Each stdio bridge connects once per message;
notifications receive a private acknowledgement and produce no MCP response.
Multiple clients share one device owner and serialized controls. One malformed
or disconnected client cannot stop it. Scheduled jobs continue after client EOF.

The socket parent must be an existing real directory with no group/other
permissions (700); the socket and lock use 600. This grants access to processes
running as the same OS user. No HTTP port, TLS endpoint, token service or additional
runtime dependencies are added. An OS lock serializes socket ownership, allows
stale-socket recovery after a crash, and refuses replacing regular files/symlinks.
Android uses Bionic `flock` directly because Rust 1.95's `File::try_lock` returns
unsupported there. Desktop uses the standard-library lock. Other lock errors
retain their real cause rather than pretending another instance owns the config.

The daemon has one 128 KiB accept thread and one device/job thread. Accepted
connections use a one-entry queue; one additional accepted stream can wait in
the accept thread, and the OS bounds the listen backlog. Requests remain limited
to 64 KiB; private replies are limited to 1 MiB. Request framing and reply writes
have five-second socket timeouts; the bridge waits up to 300 seconds for a reply.
Read timeout applies per socket read, so a same-user client sending fragments can
delay the serialized owner. Only trusted same-user clients should use this socket.
Device operations and schedules remain serialized; slow operations can delay jobs.
A timeout/disconnect never retries a control. Bridges exit with an explicit
transport error; after a lost reply, inspect fresh state before another action.
Supervision can restart the daemon; existing durable attempt guards still apply.

## Android deployment

### Private installation profile

Set `deployment` in `.local/home.json` using the generic
`examples/deployment.json` schema, and pass `--config .local/home.json`
before the command to Android helpers. It is required for isolation and
assistant setup, and recommended for recovery operations. Legacy
`--deployment .local/deployment.json` remains supported. Basic owner-only
installation without a profile retains the primary-user Termux layout.

- `android.package` and `android.user` select the supported debuggable Android
  package/profile. CE/DE, service and sandbox paths are derived consistently.
  Optional `root_command` and `zeroclaw_config` must be normalized absolute paths.
  Without `root_command`, the host discovers `su` through ADB; no device-specific
  root executable is compiled into the launcher.
- `agent.name` selects an existing agent; it is never silently created.
  `gateway_port`, `mcp_port`, and optional `transcription_port` are distinct,
  nonzero TCP ports. The latter opens only that configured loopback exception.
  `provider_host` selects the DNS probe target. Optional `tools` narrows the
  supported Homewire capability allowlist; shell, filesystem and device/security
  configuration are never selectable. Telegram remains read-only.
- `network.dns_servers` is an explicit 1–4 IPv4 server list. `lan_probe` is the
  operator's blocked IPv4:PORT test endpoint; `admin_port` must match the API
  listener. IPv6 remains denied by the sandbox. Private/special network denials
  remain compiled security policy; the profile does not allow arbitrary LAN rules.

The profile is bounded to 8192 bytes and read by host-side setup tools. Generated
launch scripts, firewall rules, MCP URL and native probe arguments use the same
validated values. Homewire still loads only its own typed adapter settings once
at startup. Device credentials stay in their existing configs, not this profile.
On the phone the resolved profile is retained as a root-only policy snapshot.
Private profiles and automation settings must not be committed to Git.

Install and update validate the prepared policy through the pinned ZeroClaw CLI
before changing the active policy; an update fails before stopping the agent if
validation changes its declared scope.

An isolation update requires the profile to match the operator-managed API and
assistant endpoints before stopping anything. To change ports or agent identity,
coordinate those private configs first; mismatches fail instead of silently
switching or restoring permissive grants. Changing DNS/provider/tool subsets
uses the normal sandbox update. Alias changes require the selected agent to
already exist in the active policy, and its workspace/mount is prepared by update.

For existing configs, explicitly add Meaco presets/schedule, curve/outside target
to any enabled light automation, and TV timings before installing the new binary.
Preserve current schedule flags and HA ownership. Back up settings, replace private
files atomically and restart the owner to load changes; no hot reload or control
replay occurs. Restart isolation through its operator helper to remount policy.


Host requirements: ADB, Python 3.11+, rustup with Rust 1.95.0 and
`aarch64-linux-android` installed, and Android NDK **30.0.16248370**. Install the
Rust target with `rustup target add --toolchain 1.95.0 aarch64-linux-android`.
Set `HOMEWIRE_NDK` to the extracted NDK directory. Build scripts accept macOS
and Linux hosts and use Android API 28; this is an Android/Bionic executable,
not a generic Linux or macOS binary. NDK host tools run only on the build host.

```sh
scripts/android/build.sh
python3 scripts/android/phone.py --config .local/home.json --serial "$HOMEWIRE_ADB_SERIAL" install
python3 scripts/android/phone.py --config .local/home.json --serial "$HOMEWIRE_ADB_SERIAL" connect-zeroclaw --agent "$HOMEWIRE_AGENT"
python3 scripts/android/phone.py --config .local/home.json --serial "$HOMEWIRE_ADB_SERIAL" verify
# Optional strict availability check (offline devices make this fail):
python3 scripts/android/phone.py --config .local/home.json --serial "$HOMEWIRE_ADB_SERIAL" verify --require-online
# Update the executable and enable MCP controls, preserving configs/state:
python3 scripts/android/phone.py --config .local/home.json --serial "$HOMEWIRE_ADB_SERIAL" install --allow-control
python3 scripts/android/phone.py --config .local/home.json --serial "$HOMEWIRE_ADB_SERIAL" rollback
```

Set `HOMEWIRE_ADB_SERIAL` to the target shown by `adb devices` and choose an existing
agent alias. Unlock Android before installation or verification after a reboot.
The Python script runs on the host, never on the deployed runtime. It requires the installed
Termux app to permit `run-as com.termux` and already have `termux-services` with a
running supervisor. It fails early without that access; arbitrary release Termux
installations may require a separate SSH/root installation route. Homewire itself
runs unprivileged as Termux's user. Root is not a runtime requirement.

Build output is `target/android-package/homewire` with `manifest.json`, recording
the binary SHA-256, source commit/dirty flag, Rust source/manifest tree hash, target,
API level and tool versions.
Installer compares host and phone hashes and checks the executable before
switching the active version. Executables live in `~/homewire/releases/HASH/`;
`current` and `previous` links select versions. Configs, state and locks stay in
`~/homewire/config/`, independently of executable updates. This is a local build
and installation procedure, not a package publication or GitHub Release workflow.

`--config` imports one runtime `home.json` plus known certificate/key files;
the host-only deployment block is omitted. Legacy `--configs` imports known separate files;
it does not copy test configs, lock files, saved state or validation recordings.
Test/tunnel `connect` overrides, sample addresses and placeholders are rejected.
Existing phone configs are archived privately before explicit imports. An installed
API agent endpoint is retained when the imported API omits that generated endpoint;
its token and exact tool scope remain unchanged. An explicit endpoint overrides it,
and removal of the API service removes the frontend. Certificate
and TV child paths should be relative to the config directory. Subsequent installs
use a legacy deployment-only profile and omit `--configs` to retain target configs.
With unified `--config`, installation also updates runtime settings. A TV config owns its Samsung, DIGI and entire plug
manager, so the generated service does not load those separately.

The installer writes an enabled `termux-services` service at
`$PREFIX/var/service/homewire`. It waits for shutdown, removes the stale socket,
starts the selected version and checks discovery/read replies. Unavailable devices
are reported and do not invalidate a working executable; `--require-online` makes
standalone verification strict. Each successful install marks its executable as
validated. Failed upgrades restore a previously validated executable when one
exists. Rollback restores only the executable; it never rewinds device state,
configs or attempt guards. Schema compatibility still matters when rolling back
future versions. A first install has no earlier validated version to restore.
Retained releases/backups can grow across installs; clean older versions deliberately.
Logs rotate at 64 KiB with three archived files in `~/homewire/log/`.

Each install preserves existing `service.args`. A new installation starts
read-only; `--allow-control` or `--no-allow-control` explicitly changes that grant.
Schedules, maintenance and monitoring start off on a new installation.
To enable them later, add one supported flag per line and restart the service;
first disable the corresponding HA rules and validate the desired controls.
Installed configs alone do not enable automations. Installer verification sends
no device controls. Socket is `~/homewire/run/homewire.sock`.

ZeroClaw integration defines server `homewire`, a matching MCP bundle, and adds
that bundle only to the selected existing agent. It preserves other config values,
compares the original config before writing, keeps a `config.toml.before-homewire`
backup and restarts ZeroClaw to pick up the grant. Repeating the command is a no-op
when already configured; differing existing Homewire definitions fail for review.
The server launches the same executable as `bridge` with absolute paths and a
300-second tool timeout. Tools appear as `homewire__entity_list`, etc. A working
model provider is separately required for agent inference.

Termux:Boot can start the existing service supervisor from `~/.termux/boot/`.
Open the Boot app once after installation; its script can acquire a wake lock and
start `service-daemon`. The installer preserves existing boot configuration;
configure it explicitly on a new target. Android battery/background restrictions
and credential-unlock state
can affect boot/network access; screen-off, reconnect and reboot checks must be
made on the actual target. See [Termux:Boot](https://github.com/termux/termux-boot)
and [NDK tooling](https://developer.android.com/ndk/guides/other_build_systems).

Validate each deployment independently: discovery, fresh reads, exclusive device
ownership, update/rollback, screen-off operation, Wi-Fi reconnection and reboot.
Observe actual scheduled runs and alert/recovery delivery before transferring
automation ownership. Keep those results and any migration decisions in private
installation records.

Model inference requires a separately configured provider. Store its credentials
privately and use a dedicated key with an appropriate spending cap. A secret
manager on the same rooted device cannot protect a credential while the agent
uses it; keeping provider credentials off the device requires an external proxy.
Use paginated discovery or targeted status queries for clients with output limits.

## Brand assets

The copper cable cottage represents local home device control. The README logo
is [`logo.png`](../logo.png); transparent logo, text-free icon, and wide social sources live in
`image-assets/sources/`. Sized exports, PNG/ICO favicons, a web manifest, and the
1200×630 social image are indexed in [the asset manifest](../image-assets/manifest.json). The outlined
artwork works on light and dark backgrounds. These are branding assets; Homewire
does not serve a web app or install the web manifest.

## Household services

Use `--adapter FAMILY=PATH` for the following statically compiled adapters. The
single catalog in `device.rs` generates storage, discovery and dispatch; no
plugin loader, retained schemas, dynamic registry or interpreter is deployed.
Optional autonomous reads declare their next deadline in the adapter. These run
in the existing device owner, independently of an AI session. HTTP messages,
SOAP XML, SNMP packets, disk state and queries have explicit bounds.

| Config example / family | Required fields and defaults |
| --- | --- |
| `examples/router.json` / `router` | `name`, HTTP(S) origin `url`, local admin `password`; up to 64 `trackers` with name/MAC/optional `person`; timeout 8 s, polling 60 s (15–900), absence 180 s (30–3600) |
| `examples/brother.json` / `brother` | `name`, literal-IP `host`; community `public`, timeout 3 s; optional test `connect` |
| `examples/cast.json` / `cast` | `name`, literal-IP `host`, `server_cert_sha256`; port 8009, timeout 8 s; optional test `connect` |
| `examples/dlna.json` / `dlna` | `name`, `description_url`, exact `udn`; timeout 8 s; service endpoints must stay on the description origin |
| `examples/mobile.json` / `mobile` | Up to 8 `devices`, home `latitude`/`longitude`; optional `persons` mapping name to device, radius 150 m, freshness 600 s |
| `examples/alerts.json` / `alerts` | Up to 8 companion names and 4 delivery channels; channels specify name/HTTPS URL, optional authorization/push registration; timeout 8 s |
| `examples/history.json` / `history` | 1–32 numeric entity IDs; interval 300 s (60–3600), max points 4096 (96–16384) per entity |
| `examples/weather.json` / `weather` | `latitude`, `longitude`; current MET compact measurements and 12 forecast entries |
| `examples/shopping.json` / `shopping` | Empty object; durable household list |
| `examples/radio.json` / `radio` | Optional HTTPS server origin, default `https://de1.api.radio-browser.info`; timeout 8 s |
| `examples/speech.json` / `speech` | Optional language, default `en`; Google Translate public speech URLs |

The Archer AXE75 adapter implements the V1_11 RSA-only login variant, verifies
live model before control, and reads status afresh. Continuous healthy absence
must last the configured delay; missing lists, failed requests and restarts never
establish absence. Person-designated router trackers drive `nobody_home` (null
while uncertain). No device-state cache is substituted for a failed request.
Main/guest/IoT Wi-Fi use 2g/5g/6g settings; DHCP and OpenVPN/PPTP are supported.
Reboot, main Wi-Fi disable and DHCP disable require `--maintenance`. IPsec is not
advertised by this adapter. Validate administrative network access, identity and
firmware support from the deployment target before relying on router presence.

Brother SNMP sends only GET, verifies HL-L2350DW identity, and exposes page,
duplex, drum count/life, remaining drum pages and toner percentage. It complements
IPP; supply percentages are separate warnings, never inferred hardware faults.
Cast requires its own observed certificate pin; do not assume the remote port 6466
certificate also identifies port 8009. Explicit CLI `certificate-sha256 IP:PORT`
inspects first-use identity without saving or trusting it in any adapter. Verify
the endpoint before copying its pin; normal adapters reject changed certificates.
Cast launches the standard media receiver when needed. Controls are never replayed
on a timeout; results distinguish sent from confirmed playback.

### Typed settings

Discover schemas with `entity_list {"device":"FAMILY.NAME","details":true}`;
follow `next_offset`. Discovery defaults to 8 compact descriptors or 1 detailed
descriptor, accepts a limit of 1–16, and bounds a page to approximately 6 KiB.
Category tools validate the whole request before dispatching to the adapter.
Adapters prepare read-modify-write payloads before sending changes, preserve
unrelated settings, and verify after writes. A later failure can leave earlier
changes applied; no blind replay or fake transaction rollback occurs.

- Meaco: `power`, `target_humidity` 35–70 in steps of 5, `mode` manual/laundry/
  purify/sleep, `child_lock`, on/off timers cancel or 1h–24h. Power-off cannot be
  combined with mode. Sleep retains its one-send toggle semantics.
- Plugs: `auto_update`, `auto_off`, `auto_off_minutes` 1–1440, `indicator_led`
  always/never/night_mode, and P110 `protection_w` 0–3680 subject to live maximum (0 disables protection and
  requires maintenance).
  TV-owned outlet auto-off changes require maintenance. Firmware acceptance is
  checked live; no protection threshold is inferred from consumption.
- Lights: `auto_update`, `smooth_transitions`, separate fade on/off seconds where
  supported, `preset` 1–16, and `effect_enabled`. Live options include bounded
  preset pages and transition ranges. Effect activation restores only the effect
  already stored on the device; arbitrary factory effect programs and music
  configuration are not implemented. Apply preset/effect requests separately.
- DIGI: HTTP(S) `app_link` or `text` up to 512 bytes, one action per request.
  Text requires negotiated IME support and live counters from a focused field.

### Alerts, history and convenience tools

`--watch-meaco`, `--watch-printers`, `--watch-lights` and `--watch-plugs` enable
independent debounced monitoring. Meaco tracks tank-full/problem, printer reasons
track hardware faults, and printer supplies at <=15% emit separate low-supply
warnings. Existing 3-second fault / 30-second recovery confirmation remains.
Missing readings do not clear confirmed faults. Schedule and standard control
failures also enter the optional ledger. Events still go to stderr.

`alert_list {}` returns durable cards. The private ledger holds at most 64 alerts
and 64 KiB, acknowledges each channel independently, retries failed delivery no
sooner than 30 seconds, and keeps recovery updates pending until configured
companions acknowledge their current revision. Unchanged alerts cause no writes.
Delivery is at least once; receipt persistence prevents routine repeats. A full
ledger retains pending alerts rather than silently evicting them.

Channels default to `delivery: "webhook"`, preserving existing payloads and
acknowledgements. Optional `keys` is an exact allowlist of at most 64 alert keys;
omitting it or using an empty list routes all alerts. Filtered-out cards do not
become pending deliveries for that channel.

For Telegram, use `delivery: "telegram"`, `chat_id` (a numeric chat ID string or
`@channel_username`), and `url` in the form
`https://api.telegram.org/bot<BOT_TOKEN>/sendMessage`. Only that official HTTPS
endpoint is accepted. No push registration or authorization header is allowed
for this delivery type. Keep the URL in protected private configuration because
it contains the bot token. A changed recipient invalidates the previous receipt.
Telegram delivery requires both HTTP success and `ok: true` with a message ID;
response bodies and token-bearing URLs are never included in delivery errors.
Messages use plain text. `tank_full` becomes a request to empty the dehumidifier
tank, and confirmed recovery sends an explicit resolved message. This is outbound
notification only; it does not enable an agent's Telegram input channel. Telegram
may accept a message before a connection or local persistence failure, so retries
can still duplicate it; recipient reading and phone display are not guaranteed.
See [Telegram's sendMessage contract](https://core.telegram.org/bots/api#sendmessage).

`history_series {}` lists configured entities/retention. `history_query` accepts
entity, optional Unix start/end, aggregation raw/hour/day and limit 1–96. Each
fixed 32-byte CRC record lives in an on-disk ring; maximum disk storage is
32 × 16384 × 32 bytes (16 MiB). It retains counters rather than JSON snapshots in
RAM. Raw samples and UTC hourly/daily min/max/mean are available. The dashboard
plots raw values or bucket maxima. Missing periods are not interpolated, and a
daily energy counter's observed maximum may miss use before midnight. This is
not a migration/import of HA's existing statistics database. Preserve that archive
before deleting the VM.

`shopping_list {}` and `shopping_update` support add (name), check (id/complete),
remove (id) and clear_completed. Names are limited to 128 bytes; 64 items fit a
private 32 KiB state file. Duplicate incomplete names are suppressed.
`weather_status {}` returns MET measurements/forecast with timestamps.
`radio_search {"name":"..."}` returns up to 8 resolved station URLs.
`tts_media {"text":"..."}` accepts 1–200 characters and returns a public MP3 URL
for `media_play`. Google receives that text when playback fetches the URL. These
public services require Internet and are best effort; no VM service is involved.

## Native Android companion

The platform Java app in `companion/android` uses no Gradle runtime, framework,
WebView, bundled model key or third-party app dependencies. Build with JDK 17,
Android API 35 platform and build-tools 35, setting `HOMEWIRE_ANDROID_PLATFORM`,
`HOMEWIRE_ANDROID_BUILD_TOOLS`, `HOMEWIRE_SIGNING_KEY` and
`HOMEWIRE_SIGNING_PASSWORD_FILE`, then run `companion/android/build.sh`.
Keep the signing key/password private and back them up: updates require the same
signer. The output is `companion/android/build/homewire-companion.apk`.

Enable the daemon's frontend with `--api PATH`. `examples/api.json` documents the
schema: LAN listen address, a random 64-hex-character dashboard token, up to 8
clients with distinct name/token, certificate PEM and private-key PEM paths
(relative to config). LAN access requires TLS; unencrypted HTTP is restricted to
loopback. Generate independent tokens (e.g. `openssl rand -hex 32`), replace all
example values, and protect config/key files with mode 600. The frontend handles
one bounded connection at a time on a 256 KiB-stack thread and forwards device
operations to the existing private IPC owner. It creates no second scheduler.

Install the signed APK on a compatible Android phone and enter the server HTTPS
URL, certificate SHA-256
and **that phone's scoped token**, and press Save and start. Grant notifications;
location is optional. Companion tokens can only report their own telemetry and
read/acknowledge alerts, unless explicitly granted assistant access. They cannot
use the dashboard control API. Dashboard credentials stay separate and in browser
memory only. Android notification delivery may be delayed by network or power
management; polling is normally once per minute. Location uses passive/network
updates, and remains unknown when stale or its accuracy overlaps the home radius.
After a reboot, alerts/battery resume once Android unlocks; open the app to resume
optional location collection. There is no background-location permission request.

Optional voice uses Android's installed speech provider (preferring offline),
shows the transcript for review and sends it to ZeroClaw. Text is also available.
To configure the local assistant proxy, run on the host:

```sh
python3 scripts/android/connect_assistant.py --config .local/home.json --serial SERIAL --agent "$HOMEWIRE_AGENT"
```

It adds a dedicated gateway token while preserving model keys and existing paired
clients. Set `assistant:true` only for clients allowed to use that agent, then
import the updated API config using the normal installer. The frontend proxies
only its configured loopback ZeroClaw webhook. Agent calls are sent once, even after
an uncertain response. A 90-second wait can delay other frontend connections;
the independent device owner/scheduler continues. Faster-whisper pipelines
and Android TV voice audio streaming are not implemented.

Remote access can use a Cloudflare One client and a tunnel private IP route
to the Homewire server's HTTPS port. Include that IP in WARP routing and
restrict network access to your identity/port. The pinned phone certificate works
with a private TCP route; publishing an edge-terminated HTTPS hostname requires a
different certificate trust strategy. A stable LAN address/DHCP reservation is
required. Keep the tunnel connector independent of any controller you plan to
retire so removing that controller does not remove remote access. Tailscale is
optional; this project installs neither.

## ZeroClaw isolation

Rooted ARM64 Android deployments can use an external native sandbox with ZeroClaw **0.8.5**. It is
separate from ZeroClaw's optional built-in sandbox: `security status` still reports
that built-in sandbox as inactive. The native launcher enforces the boundary.

- A signed, permission-only `eu.tsilva.homewire.agent` APK assigns a dedicated
  Android UID and INTERNET permission. It has no code, activity, service or app
  process. A package-specific battery exemption allows the native background UID
  to use Android DNS. Do not uninstall it while the sandbox is installed.
- A private mount namespace and `chroot` expose read-only native programs, Android
  libraries/properties and two networking helper sockets. `/proc` is deliberately empty, supporting
  kernels without PID namespaces. Homewire, Termux, device credentials, shared
  phone storage, root helpers and host process descriptors are absent.
- The launcher drops all capabilities, supplementary groups except Android's
  internet group, and inherited non-standard descriptors; sets `no_new_privs`;
  disables core dumps and limits open descriptors to 128. Static executables and
  unaudited ZeroClaw versions are refused at installation. An obligatory preload
  guard installs seccomp before worker threads start. It denies program execution,
  separate processes, ptrace/process-memory access, namespace/mount changes,
  privilege changes and kernel-management calls. Ordinary worker threads remain
  available. Setup failure stops the agent; there is no permissive fallback.
- Only agent-owned state/temp files are writable. Configuration, its encryption
  key and `TOOLS.md` are read-only bind mounts. The parent state directory cannot
  be renamed to substitute a different configuration. Homewire's private Unix IPC
  and its settings stay outside the jail.
- The UID firewall permits the scoped MCP endpoint, local transcription service,
  the home resolver and public IPv4 HTTPS. Direct LAN/admin connections and IPv6
  are blocked. Established replies allow the loopback gateway to answer trusted
  clients. DNS uses Android's broker. Public HTTPS is not a destination allowlist.

The model receives only Homewire MCP tools; shell, filesystem, browser, arbitrary
HTTP, Google/photo/camera/microphone bundles, delegation and agent cron are removed.
The `homewire` risk profile is `supervised`: ZeroClaw 0.8.5 bypasses channel exclusions
under full autonomy ([pinned implementation](https://github.com/zeroclaw-labs/zeroclaw/blob/cb2b20a9fe59004b7ef03e123942d31f29236d15/crates/zeroclaw-channels/src/orchestrator/mod.rs#L6555)). Telegram excludes every Homewire mutation and its peers have
no agent-administration grant. Existing peer IDs, channel enablement, paired
clients and encrypted model keys are preserved. Installation does not enable
Telegram or send messages; validate delivery separately when it is enabled.

The trusted local assistant gateway retains 20 Homewire tools. It cannot call
`entity_configure`. Telegram can use the 12 read tools: `entity_list`,
`entity_status`, `entity_options`, `routine_list`, `routine_plan`, `alert_list`,
`history_series`, `history_query`, `weather_status`, `shopping_list`, `radio_search`
and `tts_media`. It cannot change devices, run routines, play media or edit shopping
items. A compromised allowed Telegram identity could still read household state
and use the capped model account. This is a reduction in privileges, not protection
against a compromised root user or kernel. The provider key necessarily exists in
process memory; retain a dedicated provider-side spending cap to bound cost.

### Trusted Homewire MCP endpoint

The optional `agent` object in the private API config enables a second listener:

```json
"agent": {
  "listen": "127.0.0.1:42620",
  "token": "REPLACE_WITH_AN_INDEPENDENT_64_HEX_TOKEN",
  "tools": ["entity_list", "entity_status", "shopping_list"]
}
```

Use a token distinct from dashboard and companion tokens. Scope must contain
1–32 unique names. Only loopback is permitted; only authenticated `POST /mcp` is
served. The endpoint wraps requests into the existing private owner IPC with its
operator-controlled role and scope. Caller-supplied roles/scopes cannot widen
permissions, nor can the agent use dashboard or companion routes. MCP messages
are stateless JSON responses, with 202 for notifications. Requests are limited to
8 KiB and bounded deadlines. Endpoint counters permit 30 authenticated MCP
requests per minute and 20 control attempts per hour, independently of ZeroClaw.
Discovery/reads count toward the minute limit; initialization and denied attempts
also count. Excess traffic receives HTTP 429 before owner dispatch. Counters reset
on Homewire restart; schedules and trusted dashboard/companion requests are
unaffected. A restart burst can exhaust the minute budget: wait for the next minute
and restart ZeroClaw if MCP initialization failed. Agent responses are capped at
64 KiB by ZeroClaw. The separate API worker uses a 256 KiB stack and fixed-size
counters; it creates no additional device manager, polling loop or scheduler.

### Install, verify and update

Requires an unlocked rooted ARM64 Android 15 target with mount namespaces and
seccomp, Magisk, debuggable Termux, a dynamic ARM64 ZeroClaw 0.8.5 binary and an
existing ZeroClaw agent named in the private deployment profile and Homewire HTTPS API/assistant configuration.
Configure the assistant gateway with `connect_assistant.py` **before** isolation.
Use the same SDK/platform/signing environment as the companion build; retain the
private persistent signing key for future APK updates.

```sh
# HOMEWIRE_NDK points to NDK 30.0.16248370.
sh scripts/android/sandbox/build.sh
# Set JAVA_HOME, HOMEWIRE_ANDROID_PLATFORM, HOMEWIRE_ANDROID_BUILD_TOOLS,
# HOMEWIRE_SIGNING_KEY and HOMEWIRE_SIGNING_PASSWORD_FILE as for the companion.
sh scripts/android/sandbox/build-identity.sh
python3 scripts/android/isolate.py --config .local/home.json --serial SERIAL install
python3 scripts/android/isolate.py --config .local/home.json --serial SERIAL verify
# Rebuild native artifacts before updating them; policy/model keys remain intact.
python3 scripts/android/isolate.py --config .local/home.json --serial SERIAL update
python3 scripts/android/isolate.py --config .local/home.json --serial SERIAL restart
```

The installer checks the configured MCP port is free, takes a cold copy of agent state,
hash-verifies transfers, disables the
old runit agent and moves the legacy root/pre-unlock boot hook out of service.d.
Original ZeroClaw files remain a private recovery copy. Homewire keeps its saved
schedule/monitor/control flags. If adding the agent listener fails, the installer
restores Homewire's prior API configuration without re-enabling the old agent.
Do not run installation twice over an existing jail; use `update` for native guard
changes. Updates retain previous native files alongside their replacements and
stay stopped if their negative probe fails. They never restore older permissions
on failure. `verify` checks policy, the actual daemon's guard/capabilities/seccomp
and native file/execution/network denials;
perform a separate trusted read-only assistant request to check model connectivity.
No automated device-changing verification is performed.

The root boot supervisor waits for Android credential unlock, sets the UID
firewall before launch, holds a single-instance lock and restarts after failure.
A bounded native log writer keeps at most two 64 KiB log files. Restarting the
supervisor kills its child via a kernel parent-death signal. Agent credentials
remain in CE storage; Homewire's separate Direct Boot owner can run before unlock.
Legacy grant helpers refuse changes using the `zeroclaw-isolated` marker.
Operator-only policy files live under
`/data/data/com.termux/files/home/zeroclaw-sandbox/policy`. Stop the supervisor
before an operator policy edit, preserve private modes/UID ownership, and restart
it to remount replacements. Never edit the dormant `.zeroclaw` copy expecting to
change the active deployment. The agent cannot administer this procedure.

## Android Direct Boot and recovery

On a supported rooted Android device with Magisk,
`scripts/android/direct_boot.py --config .local/home.json --serial SERIAL install`
can move the native owner into `/data/user_de/0/com.termux/files/homewire` and
install a Magisk boot supervisor under `/data/adb/service.d`. It preserves the CE
copy, verifies the native executable as Termux's UID, keeps private modes/SELinux
context, stops the old owner before starting the new one, and rewrites only device
config paths and executable version links. Normal installs then update the DE
deployment and preserve flags,
state and version rollback. Re-run `connect-zeroclaw` to migrate the exact existing
managed MCP bridge paths; conflicting custom definitions still fail for review.

The device owner runs under Termux's unprivileged UID with the network group and
all Linux capabilities dropped. Magisk supplies the runtime SELinux domain; its
MCS categories match Termux so newly written private files remain accessible to
the normal bridge after unlock. Root is used by setup and supervision, without
changing the phone's SELinux policy. The AI key/config remains in
credential-encrypted storage, so
ZeroClaw still requires Android unlock. Direct Boot does not protect LAN device
keys from root: they must be readable to automate before unlock. The supervisor
rotates two small logs and restarts only the owner after failure. Do not treat an
installed boot script as proof of before-unlock operation; validate with a reboot.

An ARM64 Android 15 sample with monitors enabled measured 5,260 KiB owner RSS /
5,852 KiB process high-water RSS, three threads and one CPU tick over ten idle
seconds. ZeroClaw separately measured 23,968 KiB RSS. The companion measured
6,534 KiB proportional memory and about 78 MiB RSS, including shared Android
runtime pages; its APK was about 21 KiB. The native executable was about 3.4 MiB.
These are short platform observations, not worst-case memory or battery budgets
and not a statement about any installation. Measure the target workload yourself.
Updates and ZeroClaw configuration still require Android unlock.

Before replacing another controller, validate exclusive scheduler ownership,
a full TV cycle, companion notifications/recovery, router and Cast controls, actual timed runs, power/network
recovery and sustained operation with the previous controller stopped. Back up
the phone's device config, state/history, media certificates and APK signing key outside the phone. Existing
Samsung pairing is still imported/backed up; fresh H-series pairing has no native
setup helper. `pair_digi.py` is an independent host-only PIN pairing helper for
Android TV and requires the displayed code and a reachable LAN endpoint.

## Infisical configuration references

A whole JSON string `${HOMEWIRE_SECRET_NAME}` resolves from the process environment.
References must name uppercase Homewire variables; missing, empty, oversized or malformed
values fail configuration loading without displaying the value. For PEM private-key paths,
`env:HOMEWIRE_SECRET_NAME` reads the key directly from the environment.

Run `infisical login` and `infisical init` to select your workspace. The resulting
checkout-local `.infisical.json` is Git-ignored. Launch the native process through
`infisical run --env=prod -- ...`. The Android installer
also resolves JSON references in memory. For `api.json` `private_key` and `digi.json`
`client_key`, it sends referenced PEM bytes to the corresponding protected device key file
and rewrites only the destination JSON path. Host files remain references, while target
files use the existing atomic mode-0600 transfer. Use Infisical when installing with
unified `--config` or legacy `--configs`; deployment-only updates preserve existing target credentials.
