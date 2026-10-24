# Device guides

[Reference index](reference.md) · [First use](../README.md)

Examples below use legacy per-family flags. With unified configuration, load the
same adapter objects through `--config .local/home.json` instead; do not combine
both forms. Keep configs in ignored `.local/` with mode 600 and replace synthetic
addresses, names and credentials. See [configuration](configuration.md).

Controls stop on uncertain outcomes. Use one config path per device; ownership
locks cannot coordinate overlapping configs, Home Assistant or manufacturer apps.
Standalone watch commands emit JSON events on stdout and diagnostics on stderr;
MCP watch flags keep both on stderr. Faults require matching observations at least
3 seconds apart, recovery at least 30 seconds apart. Unknown/unavailable readings
interrupt confirmation and never clear retained faults. Transitions persist
before output; [alerts](services.md#alerts-history-and-convenience-tools) handle delivery.

## Printers

The printer adapter sends only IPP `Get-Printer-Attributes` over local HTTP. It
reports idle/printing/stopped state, reasons, job acceptance/count, and ink/toner
levels when provided. No printing, cancellation, power control, discovery,
authenticated IPP or TLS is implemented in the IPP adapter. A separate
`brother` adapter provides read-only SNMP diagnostics.

Printer power is controlled through configured smart outlets with `switch_set`,
for example `{"entity":"switch.printer_outlet","power":false}`. This verifies
the outlet relay, not a graceful printer shutdown. No printer shutdown or wake
command precedes the outlet operation.

Each `printer.NAME` also exposes `sensor.NAME_state`,
`sensor.NAME_queued_jobs` (jobs), `sensor.NAME_accepting_jobs` and
`sensor.NAME_fault_active`. Read them together with `entity_status` to share one
fresh IPP query per printer. Missing job readings remain `null`; `fault_active`
is `null` when hardware fault status is unknown, rather than reporting healthy.
The printer snapshot retains detailed fault reasons and individual supplies.

```sh
target/release/homewire --printers .local/printers.json printers
target/release/homewire --printers .local/printers.json printer-status brother
target/release/homewire --printers .local/printers.json printer-check
target/release/homewire --printers .local/printers.json watch-printers
```

The [printer schema](../examples/printers.json) has top-level `printers` (1–8 endpoints),
`timeout_seconds` (8; range 1–30), and `poll_seconds` (30; range 5–300).
Each endpoint requires `name` (unique, 1–32 letters/digits/hyphens/underscores)
and literal-IP `host`; optional `port` defaults to 631 and `path` to `/ipp/print`.
Optional `connect` (`IP:PORT`) overrides just the TCP connection for tunnels.
Optional `supply_warning_percent` defaults to 15 (range 0–100); set it to `null`
to disable percentage warnings for that printer.
Unknown fields are rejected. Simulated monitoring needs a separate config.

Fault classification uses reported IPP reasons: paper,
jam, ink/toner, covers/trays, fuser and other specified supply/hardware reasons,
plus unknown reasons explicitly ending in `-error`. Idle/printing and routine
sleep/pause/offline reasons do not become fault alerts. Missing data, failed
queries, unclassified reasons, or stopped without a specific fault cannot prove
recovery. Percentages alone never generate faults. Negative or out-of-range
supply levels remain unknown; raw values are retained.

Low-supply warnings are separate from hardware faults. `printer-check` and the
legacy `printer_check` tool include `supply_events` and `active_supply_warnings`
alongside the existing hardware fields. A warning confirms a reported supply
percentage at or below the configured threshold; recovery confirms a percentage
above it. `supply_warning` / `supply_recovered` events identify the printer and
zero-based supply index, with `warnings` and `previous_warnings`. Missing supplies,
unknown percentages and failed queries cannot clear a retained warning.

Monitoring is opt-in and can wake sleeping printers. At 30-second polling,
confirmation normally needs one more poll. Hardware transitions persist in
`printers.state.json`, supply warnings in `printers.supply-state.json`;
`printers.json.lock` excludes concurrent owners. Name/host/port/path changes
invalidate saved assumptions. `active_faults` returns retained faults after
missed events. Candidates live in-process, so separate `printer-check`
invocations do not form a continuous monitor.

For SSH tunnel testing, use `connect` in a separate private config, e.g.
`127.0.0.1:62350` forwarded to printer port 631. The printer URI retains its LAN
host/path. Do not deploy test configs.

## Awair Element

This adapter sends only `GET /air-data/latest` to the Element's local HTTP API.
Enable that API in the Awair app if needed; no account token is required at
runtime. The device refreshes its measurements approximately every 10 seconds.
See [Awair's local API documentation](https://support.getawair.com/hc/en-us/articles/360049985053-Awair-2nd-Edition-Local-API-Feature).

The [Awair schema](../examples/awair.json) requires literal-IP `host`. Optional fields are `port` (80; nonzero),
`timeout_seconds` (8; range 1–30), `max_age_seconds` (120; range 10–3600), and
`connect` (`IP:PORT`, nonzero port; overrides the TCP destination for testing).
Unknown config fields and files larger than 8 KiB are rejected.

```sh
target/release/homewire --awair .local/awair.json awair-status
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

Standard MCP reads these through `entity_status`; legacy `awair_status` takes
no arguments. There is no cached fallback, cloud access, display control,
threshold alerting or sensor-triggered Meaco automation.

## Tapo L430C bulbs

The shared light adapter uses KLAP v2 over local HTTP for L430C/L900 power,
brightness, hue/saturation, white temperature and reported overheating. See
[typed settings](services.md#typed-settings) for presets, effects and transitions.
Controls require matching live model/device identity. Pairing, discovery, cloud
access and firmware installation are unavailable.

The [bulb schema](../examples/bulbs.json) has `bulbs` (1–8 endpoints), `timeout_seconds` (8; range 1–30),
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
target/release/homewire --bulbs .local/bulbs.json bulb-status bulb_1
target/release/homewire --bulbs .local/bulbs.json bulb-set bulb_1 '{"power":true,"brightness_percent":10}'
target/release/homewire --bulbs .local/bulbs.json bulb-set bulb_1 '{"hue":210,"saturation_percent":70}'
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
authentication, requests and settling. Binary responses may be labelled
`text/html`; KLAP authentication/decoding validate the payload.

`overheated` is true, false or null; only true raises a fault. Confirmed faults
persist in `bulbs.state.json`, protected by `bulbs.json.lock`; endpoint/identity
changes invalidate them. Use `watch-bulbs` or repeated checks in one MCP server
for continuous confirmation. `active_faults` returns retained faults.

## L900 LED strip

Use [examples/lights.json](../examples/lights.json) with `--lights PATH`; its `lights`
array follows the bulb endpoint schema. A strip endpoint must explicitly set
`"model":"L900"`. Bulb endpoints can use `"model":"L430C"` or omit it.
The L900 allows 2500–9000 K, also constrained by its live reported range; L430C
controls remain limited to their live range within 2500–6500 K.

```sh
target/release/homewire --lights .local/lights.json light-status led_strip
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
`light-status` also reports `model` and `effect_active`: true if an effect/music mode is active,
false if both are confirmed off, or null if unknown. L900 fixed brightness,
colour and white-temperature changes fail before controls if effects are
active/unknown, and fail verification if an effect becomes active/unknown during
the change. Disable effects/music mode before applying fixed settings. Power-only changes
can preserve the existing effect. Use typed settings for presets and restoring or
disabling the stored effect; arbitrary effects/music programming is not implemented.

`--lights` and `--bulbs` select the same manager and are mutually exclusive.
Either config accepts `lights` or legacy `bulbs`, but cannot contain both. With `--lights PATH serve --legacy-tools`, MCP advertises `light_list`,
`light_status`, `light_check`, and (with `--allow-control`) `light_apply`.
Status/apply arguments use `light` instead of `bulb`; responses use `light` and
`lights`. Existing `--bulbs`, `bulb-*`, `watch-bulbs` and MCP `bulb_*` calls remain
supported. Only one tool naming scheme is advertised, avoiding duplicate tools.

For `lights.json`, state/lock paths are `lights.state.json` and `lights.json.lock`.
L900 model identity participates in saved fault identity. `light_check` returns
live statuses, unavailable readings, retained `active_faults` and debounced events.

## L900 scheduling

Set `automations.lights` in unified config (legacy `automation`) to select one
L900, timezone and location. Optional `brightness_lights` names up to seven
additional L430C/L900 followers. The legacy shape is:

```json
"automation": {
  "light": "led_strip",
  "brightness_lights": ["lamp_a", "lamp_b"],
  "follow_tv_backlight": false,
  "brightness_entities": [],
  "timezone": "Etc/UTC",
  "latitude": 0.0,
  "longitude": 0.0,
  "curve": [[360, 20, 3000], [1080, 80, 5000]],
  "outside_curve": [20, 2700]
}
```

Replace the sample timezone and zero coordinates with your desired timezone and
location in the private config. Omit `brightness_lights` or use an empty list
when no followers are wanted. All targets must exist and be distinct. The tracked
lights template omits `automation`.
A valid IANA timezone, latitude within ±89°, longitude within ±180° and
`poll_seconds` of 5–30 are required. Omit `automation` to disable its configuration.
Having it configured does **not** enable controls: `light-plan` previews, while
`light-schedule` explicitly runs the rules. MCP uses `serve --allow-control
--light-schedule`; the scheduler starts only after the client's initialized
notification. It shares the existing event loop and config ownership lock.

For Android, save `--light-schedule` and `--allow-control` in `service.args`;
`--schedule` selects Meaco. Validate with `config-check`, preview with `light-plan`
and preserve existing flags/attempt state. Schema rollback requires matching
configs and executable; see [deployment](android.md#android-deployment).

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

Brightness followers use the same local-time interpolation and outside brightness,
while preserving their white temperature, hue, saturation and power state. They
never turn on at sunset. Each follower has independent identity-bound attempt
state in `lights.schedule.NAME.state.json`; existing primary-light state remains
in `lights.schedule.state.json`. Failure on one light does not prevent the other
targets from being assessed, and uncertain writes are not replayed in that minute,
including after restart. Disable competing brightness automations for each added
target before enabling it in the native owner.

One coordinator calculates a frozen brightness/temperature target and timestamp
once per cycle, then dispatches it independently to each physical device. Each
device has one bounded I/O worker, with no separate timer or curve calculation.
An unreachable device cannot hold up another target. A busy worker skips cycles
instead of queuing obsolete controls; expired targets are discarded. Network
operations share the cycle deadline and the adapter's timeout. Polling keeps its
monotonic cadence and skips missed slots. Manual operations on the same physical
device fail promptly while it is busy; they are not queued or replayed.
Completed results are collected on the next regular cycle and feed the existing
`schedule.lights` alert stream, without extra polling wakeups.

`brightness_entities` selects followers through standard entity IDs, for example
`["light.lamp_a", "media_player.tcl"]`. Lights and the TCL companion implement
this optional adapter capability. Unknown entities, unsupported capabilities,
field projections, duplicate physical devices (including aliases), and more than
nine total targets fail validation before device I/O. `brightness_lights` and
`follow_tv_backlight` remain compatible aliases into this same coordinator;
select each device only once. The primary L900 retains its temperature and
sunset behavior; all followers use only the shared brightness target.

Set `follow_tv_backlight: true` to make `media_player.tcl` follow the exact same
brightness curve and timezone through the [TV companion](#tcl-backlight-companion).
It defaults to false and requires a configured TCL adapter and companion; invalid
combinations fail configuration validation. The TV uses the curve's brightness
percentage directly as its native backlight level, including the outside-curve
value. This does not change picture temperature, picture mode or power.

This follower runs in the existing `--light-schedule` job. It checks at the same
5–30-second interval, assesses an on TV at startup, after observing power-on, and
in each new minute. Standby and unknown/unreachable readings never trigger a
write or a wake. Matching backlight levels are skipped. Manual brightness
changes pause this device's schedule as described below. TV failures do not stop
the light targets, and light failures do not stop the TV assessment.
Its independent, identity-bound attempt state is
`lights.schedule.state.backlight.json` for unified configuration. Each write is
persisted before control; an uncertain operation cannot repeat in the same UTC
minute, including after restart. A subsequent minute reads fresh state before
considering the current target. Disable competing TV brightness automations
before enabling this follower.

### Manual brightness overrides

Every scheduled brightness device has an independent two-hour manual override.
A validated Homewire brightness request persists the pause before device I/O,
including when the command subsequently fails or its outcome is uncertain. Both
standard light controls and legacy light commands use this policy; TCL backlight
requests use it too. Power, volume, app and unrelated configuration requests do
not create a brightness override.

Changes from a remote, manufacturer app or another controller are detected on
the next successful on-device reading (normally within the 5–30-second poll).
Detection compares against the last observed/verified brightness, not the moving
curve target. The first successful reading establishes a baseline. Each further
observed brightness change or Homewire brightness request resets the two hours;
unchanged readings and failed/unknown/off readings do not extend it. External
changes are inferred from the reported level, so firmware-driven brightness
changes also count. Changes that return to the same level between polls cannot
be detected.

While paused, Homewire continues reading that device but sends no scheduled
brightness, temperature or sunset power-on commands to it. Other targets keep
running. Expiry resumes the current target at the next successful assessment of
an on device, without waking it or replaying a missed sunset. A scheduled write
checks again during preflight; a concurrent Homewire manual request revokes the
scheduled operation before its next write check. Already-sent commands cannot be
recalled. Uncertain automatic outcomes retain their expected level so they do
not masquerade as manual changes or cause an automatic replay.

`brightness_schedule_status {"entity":"media_player.tcl"}` reads `paused`, UTC
`paused_until`, `resume_requested` and the fixed `manual_override_seconds: 7200`
without contacting the device. `brightness_schedule_resume` with the same
arguments clears only that device's override and requests a fresh assessment
immediately when its I/O worker is available. A busy worker keeps the request
for its next regular cycle. Resume requires the control grant; it does not enable
disabled scheduling. It is an explicit new assessment, not a retry of a prior
uncertain operation.

Override expiry, the scalar baseline and pending outcomes persist in private,
identity-bound `*.override.json` sidecars alongside the existing attempt files.
The primary path is `lights.schedule.state.override.json`; light followers use
`lights.schedule.NAME.state.override.json` and TCL uses
`lights.schedule.state.backlight.override.json`. Old attempt-file schemas remain
unchanged. Restart preserves pauses and pending resume requests. Identity changes
discard the old baseline and override; invalid state or persistence failures
block scheduled controls. A rollback binary ignores these sidecars and uses its
previous scheduling behavior.

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

The P100/P110 adapter shares the lights' KLAP v2 transport and read-only session
refresh. It uses HTTP port 80; other models/login variants, discovery and pairing
are unsupported.

The [plug schema](../examples/plugs.json) has a `plugs` array of 1–8 endpoints:

- `name`: unique 1–32 ASCII letters, digits, hyphens or underscores.
- `model`: required `P100` or `P110`.
- `host`: the plug's IP; `port` defaults to 80.
- `device_id`: the exact reported identity, 1–64 alphanumeric characters.
- `auth_hash`: private 64-digit hexadecimal KLAP v2 hash, equivalent to decoded
  Home Assistant `credentials_hash`; do not use an account password here.
- `connect`: optional `IP:PORT` test/tunnel destination, preserving the actual
  Host header, expected model and device identity.

Maximum file size is 8 KiB. `timeout_seconds` defaults to 8 (1–30), `settle_seconds` to 2 (0–10, less
than timeout) and `poll_seconds` to 30 (5–300). The timeout covers the whole
handshake/read/write/settle/verification operation for each endpoint. Only
configured names can be accessed; tool arguments cannot change hosts or keys.

CLI commands are `plugs`, `plug-status NAME`, `plug-set NAME on|off`,
`plug-check`, and `watch-plugs`, all selected with `--plugs PATH`.
Legacy MCP (`serve --legacy-tools`) exposes `plug_list`, `plug_status {"plug":"metered_outlet"}`, and `plug_check`.
`serve --legacy-tools --allow-control` additionally advertises
`plug_apply {"plug":"relay","power":true}`. Only the named relay and boolean power are accepted. `--watch-plugs` starts
polling after MCP initialization; monitoring never controls relays.

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

If `get_energy_usage` returns `-1003` (seen with an uninitialized device clock),
status still reads the independent live meter. Power, voltage, current and the
meter's total remain available when reported; daily/monthly totals remain null.
Other errors and invalid or absent meter readings still fail explicitly.
Homewire does not change the outlet clock or replay the rejected request.

`plug_apply` reads and verifies the configured model/identity, skips a matching
relay state, then sends one `set_device_info {"device_on":BOOL}` command,
waits for settling and verifies fresh relay state. A rejected/unverified
command is never replayed. P110 power-on requires a confirmed `normal` protection status, and verification
must still show normal. Reported `overloaded`, missing or unrecognized status
blocks changing power to on; power-off remains available. P100 has no reported
overload feature, so `overloaded` is null. Relay commands preserve protection,
auto-off, LED and firmware settings. Explicit typed settings can change the
advertised configuration; firmware installation and factory reset are unavailable.

`plug-check` reads device information without energy queries and returns statuses,
unavailable readings, retained `active_faults` and debounced events. P110
`power_protection_status` maps `overloaded` to fault and `normal` to clear;
consumption never determines overload/recovery. State persists in
`plugs.state.json` with `plugs.json.lock`; device/model changes invalidate it.
Energy failures must not block power-off. For shared media outlets use the
[TV workflow](#shared-tv-outlet-workflow), rather than direct relay cutoff.

Protocol semantics follow python-kasa's
[energy module](https://github.com/python-kasa/python-kasa/blob/master/kasa/smart/modules/energy.py)
and [power-protection module](https://github.com/python-kasa/python-kasa/blob/master/kasa/smart/modules/powerprotection.py).

## TCL Android / Google TV

Held keys keep servicing remote keepalives when their frames overlap the release
deadline; one bounded partial frame is retained until the session can finish it.

The `tcl` adapter shares the synchronous Android TV Remote v2 transport with
DIGI, using mutual TLS on port 6466. It requires a paired client certificate/key,
a pinned server certificate and an exact configured `model`. Live manufacturer
must be `TCL`; both model and manufacturer are checked before sending controls.
The verified `Smart TV` model is supported; other TCL Android/Google TV models
must report the exact configured model through this same protocol.

Copy [examples/tcl.json](../examples/tcl.json) to an ignored private file, or add
its contents under `adapters.tcl` in the unified configuration. All DIGI transport
fields and bounds below also apply. `model` is required (1–128 UTF-8 bytes,
nonblank, no control characters). The standard entity is `media_player.tcl`,
independent of the Samsung/DIGI shared-outlet TV routine.

For a new pairing, run this host-only helper on a trusted LAN with the TV on:

```sh
python3 scripts/android/pair_digi.py --device tcl --host TV_IP --model 'Smart TV' --output-directory .local/tcl-pairing
```

Enter the six hexadecimal characters shown on the TV. The PIN authenticates
both certificates, and setup checks the remote service's key and live identity
before saving `tcl.json`. The helper refuses an existing output directory.
Store the generated private key in Infisical, replace `client_key` with an
`env:HOMEWIRE_SECRET_TCL_PRIVATE_KEY` reference, and remove the host key file
after checking the stored credential. Keep the certificate and private configs
protected with mode 600. Android projection materializes the referenced key on
the target and includes the TCL certificate; it never uploads tunnel configs.

```sh
infisical run --env=prod -- target/release/homewire --tcl .local/tcl.json call entity_status '{"entities":["media_player.tcl"]}'
infisical run --env=prod -- target/release/homewire --tcl .local/tcl.json call media_power '{"entity":"media_player.tcl","power":false}'
infisical run --env=prod -- target/release/homewire --tcl .local/tcl.json call remote_key '{"entity":"media_player.tcl","key":"home"}'
```

### TCL power and network standby

Configure standby on the TV before relying on remote wake. On firmware with
**Settings > System > Power and energy > Standby mode**, select **Network
standby**. **Doze** permits intermittent background maintenance and can suspend
the network services needed for wake, even when an older network-standby setting
remains enabled. Confirm the selected mode in the current menu. Firmware with a
separate **Quick start** setting also requires it enabled, as described in
[TCL's network-wake guide](https://www.tcl.com/content/dam/brandsite/region/europe/products/tv/c-series/c745/downloads/um/tcl-mt15-en-c745.pdf).

Network standby increases standby energy use. Homewire's network wake does not
require **Screenless** mode, hands-free Assistant, or microphone access; keep
microphone privacy configured independently. Validate a deliberate standby/wake
cycle with the outlet left on and confirm the TV's reported power state. A cold
boot after switching the outlet on tests a different path. The normal `tv_off`
routine cuts the outlet after verified shutdown, while `tv_on` uses the configured
standby recovery when the outlet is already on. A magic packet alone does not
exercise the complete authenticated Cast recovery described below.

If services still disappear between maintenance windows, the companion's
opt-in [standby connection](../companion/tv/README.md#keep-the-connection-available-in-standby)
keeps the CPU available for incoming requests while the screen is off. This
uses more standby energy, preserves microphone privacy, and requires the
connection service to remain enabled. Validate the paired wake key separately.

MCP uses the same standard tools; writes require `--allow-control`. TCL has no
additional legacy tools. `media_power` skips a matching fresh power state.
By default, on sends one absolute WAKEUP key; off sends one POWER key only after
the pinned session confirms the exact identity and an on state. Some firmware
ignores SLEEP. Both directions persist their attempt privately in
`tcl.power.state.json` before sending and verify on the same session. An
unconfirmed attempt blocks another power operation for 120 seconds, including
after restart. Only confirmation of the requested state clears it; an off
reading cannot clear an uncertain on attempt. Legacy saved shutdown attempts
remain supported. Invalid, nonprivate or mismatched state blocks opening the
adapter, and failure to persist an attempt blocks control. Raw power keys remain
unavailable through `remote_key`. DIGI retains absolute wake/sleep keys.

Optional fields under `adapters.tcl` support firmware-specific recovery:

| Field | Behavior |
| --- | --- |
| `power_on_key` | `wakeup` (default) or `power`. The latter sends one state-checked POWER toggle, with the same persistent guard. |
| `wake` | Object with verified unicast `mac`, UDP `wake_address` and `ready_seconds` (default 45, range 1–120). If the remote service is unreachable before any control, send one magic packet and attempt read-only recovery before the one power key. |
| `cast_wake` | Object with independent `server_cert_sha256`, `port` (default 8009) and `timeout_seconds` (default 8, range 1–30). Uses the TV's configured host; no media URL or separate host is accepted. |

With `cast_wake` omitted, `power_on_key` selects the paired remote wake key.
Firmware that ignores WAKEUP can use `"power_on_key": "power"`; Homewire checks
fresh power state before sending that one toggle. With `cast_wake` configured,
Cast owns power-on recovery and `power_on_key` is not used for that operation.

Cast wake is **experimental and opt-in**. It first reads the pinned remote
interface; identity errors abort. If that interface is unavailable or reports
off, the independently pinned Cast interface must explicitly report standby.
The Cast certificate is an independent identity check: a mismatch stops wake
and must not trigger automatic pin replacement or another control. On firmware
where the paired Android TV Remote wake key works in network standby, omit
`cast_wake` to use that paired wake path. Deliberately validate that configuration
on the TV before relying on it.
An active Cast receiver is left alone. A standby receiver launches the Default
Media Receiver once, stopping an existing instance first if necessary; it never
loads media. Connection failures known to precede any STOP/LAUNCH receive
bounded read-side recovery. If `wake` is also configured, one magic packet may
restore connectivity; pinned remote power is checked before considering a Cast
launch. If that packet already woke the TV, no Cast launch is sent. Pin,
identity and protocol errors never trigger this fallback, and any attempted
STOP/LAUNCH ends connection retries and network fallback, even if its reply is
lost. An already active receiver is never interrupted to compensate for a
missing remote report.
If a Cast activation reply is lost, remaining time is used only for pinned
remote power verification. A confirmed on report completes the request even
when `cast_command_sent` remains null; an unconfirmed request stays guarded.

Recovery, activation and subsequent pinned Android TV power-on confirmation
share one total deadline of `wake.ready_seconds` or 45 seconds, including each
Cast TCP/TLS exchange. An early connection timeout is therefore not the end of
the operation. Only the remote power confirmation counts as success. This path
never sends a remote power key. No standby setting is changed automatically. Validate this
option across standby cycles and idle periods on the actual TV before relying
on it; simulated protocol checks do not establish firmware reliability.

An integrated `wake` also advertises `tcl.tcl` through `wake_list`; invoking it
uses the same bounded power-on operation. Do not duplicate its MAC in a generic
[Wake-on-LAN target](#wake-on-lan). Generic targets remain available separately
and cannot confirm wake.

TCL power results include `requested_power`, `applied`, `verified`, `outcome`,
`phase`, `command_sent`, `wake_packet_sent`, `cast_command_sent`, `status` and an
optional bounded `error`. `outcome` is `confirmed`, `not_sent` or `unknown`.
`command_sent` describes the remote power key and `cast_command_sent` describes
Cast activation; either is null when write completion is uncertain. `applied`
means an attempt was made, not that the screen changed. `verified:true` requires
the pinned remote power report to match the request. When verification expires,
report the request as incomplete. An unreachable TV, a sent packet or Cast
availability alone never establishes screen power. Only read-side recovery is
retried; device-changing writes are never replayed after transport errors. The
`cast_recovery` phase identifies transient connectivity recovery. A sent magic
packet without power confirmation remains an unknown attempt protected by the
persistent guard; a recovery timeout before any device-changing write is
`not_sent` and does not leave a new attempt guard behind.

The remote keys, app-link launch and focused-field text settings described for
DIGI below also apply; key/app/text delivery remains unverified. Normal commands
never start pairing or silently replace a certificate pin. Roku/Fire TV remote
protocols and ADB are outside this adapter.

TCL/DIGI status includes `current_app` and optional `volume` with the reported
`level`, `max` and `muted` fields. Supplemental messages are collected for up to
500 ms after the initial power report; unavailable readings remain null. Levels
use the TV's native scale, not an assumed percentage. `entity_options` reports
live app/text/voice/volume support and the current supplemental readings.
Before an on-state TCL control, the adapter waits up to one second for the
negotiated initial IME and volume reports. Missing readiness fails before sending
the control. App/text/key sessions service pending messages and close TLS cleanly.

### Android TV activities

`media_activity` accepts an `entity` and an `activity` object. Discover supported
names through `entity_list`; each plan sends exactly one key or app link:

| Activity `name` | Optional parameter |
| --- | --- |
| `home`, `quick_settings`, `settings`, `inputs`, `live_tv` | None |
| `hdmi` | Required `input`: integer 1–4 |
| `netflix` | Numeric `content_id` (up to 12 digits) |
| `youtube` | `query` or 11-character `content_id` |
| `prime_video` | None |
| `stremio` | `query` |

For example, `media_activity {"entity":"media_player.tcl","activity":{"name":"youtube","query":"nature documentaries"}}`.
Queries are limited to 256 UTF-8 bytes and safely encoded into fixed app links.
`quick_settings` holds Home for 800 ms and releases once; other navigation
activities send a single short key. Direct Settings/Input/HDMI keys may be ignored
by firmware even when the quick-settings panel can be reached by holding Home.
The TV must already report on; use guarded `media_power` separately. Links depend
on installed apps, supported firmware and signed-in accounts. No arbitrary
intents, addon installation, UI macros or automatic replay are added.

The adapter collects reports for up to two seconds after sending and returns
`current_app` only from fresh messages. `app_confirmed` is true/false when a fresh
package report can be compared with the requested app, otherwise null. This does
not prove playback, title selection, screen contents or HDMI input selection;
`verified` remains false. Activity controls require `--allow-control` and a
separate `media_activity` agent grant.
`entity_options.activity_parameters` exposes these parameter rules to agents.
Netflix home uses the fixed `netflix://` link; title links use its HTTPS title URL.
Prime Video uses its `/home` link; an account/profile picker may still require
navigation before viewing. App links never bypass authentication or subscriptions.

### Android TV extended input

Extended `remote_key` names include `stop`, `next`, `previous`, `rewind`,
`fast_forward`, `skip_forward`, `skip_backward`, `0`–`9`, `delete`, `menu`,
`search`, `info`, `tv`, `guide`, `captions`, `settings`, `input`, `red`, `green`,
`yellow`, `blue`, `app_switch`, `assist`, `audio_track`, `last_channel`,
`teletext`, and `hdmi_1`–`hdmi_4`. Use discovery for the complete list.
HDMI shortcuts, subtitles, audio tracks and settings depend on firmware and the
foreground app; successful delivery is not proof that the requested UI appeared.
The remote protocol does not report input state or provide picture controls.
An optional [Homewire TV companion](#tcl-backlight-companion) provides native
backlight reads and writes separately.

### TCL backlight companion

The [TV companion](../companion/tv/README.md) is an ordinary Android TV APK
with an authenticated, certificate-pinned HTTPS API. It reads the native
backlight level and sets an absolute `backlight_level` from 0–100 for the
current picture mode, using the same apply-and-save operation as the TV menu.
It does not set Android's unrelated `screen_brightness` value. This initial
implementation requires a paired, audited TCL Android 12 firmware identity;
firmware changes fail closed until the native interface has been checked again.

After installing and pairing it, add the generated `companion` object to
`adapters.tcl`; see [configuration](configuration.md#tcl-companion-configuration).
`media_player.tcl` then advertises `backlight_level` through its existing
`entity_options` and `entity_configure` interfaces. Status adds `picture` with
fresh firmware identity and native backlight readings. A companion failure
appears as `picture_error` while remote-protocol status remains available.

```sh
homewire --config .local/home.json call entity_configure '{"entity":"media_player.tcl","settings":{"backlight_level":50}}'
```

MCP writes require `--allow-control` and the caller's existing `entity_configure`
grant. Controls require confirmed TV-on status, authenticate the server before
sending credentials, read before changing, skip an already matching value, and
verify the result. Neither client nor server retries an uncertain write. The
companion retains a bounded replay journal across restarts. Network availability
in standby and vendor restrictions on starting apps still depend on the TV.

### Android TV held keys and voice

`remote_hold {"entity":"media_player.tcl","key":"enter","hold_ms":800}`
holds a supported key for 1–2000 ms, services incoming messages, and attempts one
release on the same connection, including after a read failure. Holds are
serialized under the adapter's existing configuration lock. Power keys remain
unavailable, and failed presses/releases are never reconnected or replayed.

`media_voice {"entity":"media_player.tcl","audio":"BASE64_RAW_PCM"}`
sends supplied signed 16-bit little-endian PCM, mono at 8000 Hz, with no WAV
header. Audio must contain 1–24000 samples (at most 3 seconds / 48000 bytes /
64000 base64 characters). Voice is negotiated only for that request. The client
sends SEARCH once, waits up to 2 seconds for a voice session, echoes its id,
streams 8192-byte chunks with final zero padding, and attempts session end once.
Only these outgoing audio frames may exceed the usual 8 KiB protobuf bound,
with an 8256-byte limit; ordinary commands and incoming frames retain 8 KiB.
The total operation uses `timeout_seconds`; use at least 8 seconds for holds/voice.
There is no microphone capture or speech synthesis. Recognition and any
assistant action remain unverified; audio is not retained or logged by the adapter.
Both tools require `--allow-control` in MCP and separate agent grants.

### TCL Google Cast

Configure the existing `cast` adapter alongside `tcl`, with a distinct entity
name. Both use the same TV address, but Cast needs its own independently inspected
port-8009 certificate pin, not the remote-service certificate. For example:

```json
"cast": {
  "name": "tcl_cast",
  "host": "192.0.2.40",
  "server_cert_sha256": "REPLACE_WITH_CAST_CERTIFICATE_SHA256"
}
```

Read `homewire certificate-sha256 TV_IP:8009` on a trusted LAN, verify the endpoint
identity, and save the pin in private configuration. This creates
`media_player.tcl_cast` for the standard `entity_status`, `media_play` and
`media_command` tools. For example:

```json
{"entity":"media_player.tcl_cast","command":{"action":"volume","percent":25}}
{"entity":"media_player.tcl_cast","command":{"action":"mute","muted":true}}
{"entity":"media_player.tcl_cast","command":{"action":"seek","seconds":120}}
{"entity":"media_player.tcl_cast","media":{"url":"https://media.example/movie.mp4","content_type":"video/mp4"}}
```

Cast adds absolute volume/mute, play/pause/stop/seek and HTTP(S) media playback.
Status includes the receiver and available media metadata, position and playback
state. Native apps without the Cast media namespace retain receiver/volume status
with `media:null`; playback controls fail explicitly. Cast metadata and seeking
depend on the active app. URL playback may launch the default Cast receiver and
replace the current app; a Netflix webpage is not a direct media URL. Use the
remote entity for app links and power. Cast availability never establishes TV power.
Each URL replacement stops an existing default media receiver session once,
verifies its removal, and launches a fresh receiver before LOAD. This prevents a
hidden receiver from playing behind the TV launcher and ends its previous media.
An uncertain stop aborts before launch/load. Other native app sessions are not
stopped individually; the new receiver launch can still replace the foreground app.
Agent access to Cast playback uses separate `media_play` / `media_command` grants;
enabling an adapter alone does not grant those tools or enable a disabled channel.
Some receivers first echo a LOAD request as `IDLE` with an extended `LOADING`
state. The adapter keeps that same connection open and services heartbeats until
the request completes, within its existing deadline/frame bounds. It never resends
LOAD. Only a matching URL and reported `PLAYING` state verify URL playback;
rejected sources or an expired deadline do not establish playback.
An unsolicited completion without the original request ID is accepted only when
its new media session ID matches the acknowledged load. If that incremental update
omits the URL, one fresh GET_STATUS on the same connection must confirm both the
session and requested URL. Unrelated playback updates do not complete the request.
`timeout_seconds` defaults to 8 and accepts 1–30 seconds. Adaptive streams can
need a longer initial buffering deadline. After an uncertain timeout, read fresh
status to learn whether playback started; do not automatically resend the URL.

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
identity/connection alone never defaults to off. Incoming messages and ordinary
commands are limited to 8 KiB; only outgoing voice payloads use the bounded
exception described above. Parsing allows 128 fields and 128 frames per state
wait, with a socket deadline. Unknown valid
fields are skipped; malformed/truncated/overflowing fields fail.

`digi-status` / `digi_status` takes no arguments. `digi-set on|off` /
`digi_apply {"power":BOOL}` sends absolute `KEYCODE_WAKEUP` (224) or `KEYCODE_SLEEP`
(223) once, then verifies a fresh reported state on that same TLS session. Already
correct settings send no key. Errors/disconnects after controls are not reconnected
or replayed. `digi-key KEY` / `digi_key {"key":"home"}` supports `home`, `back`,
`up`, `down`, `left`, `right`, `enter`, `volume_up`, `volume_down`, `mute`,
`play_pause`, `play`, `pause`, `channel_up`, `channel_down`. It sends one short key
and returns an unverified delivery result. After sending, the connection services
pending TV messages (including pings) for up to 500 ms within the operation
timeout, then closes TLS cleanly. This avoids dropping navigation keys during
immediate connection teardown. Session failures remain uncertain and never
replay the key; the wait does not acknowledge a resulting screen change.
The result includes `current_app` from messages received after sending, or null
when unavailable; this is supplemental evidence, not key execution confirmation.
`entity_configure` supports HTTP(S)
`app_link` launch and `text` (up to 512 bytes) in a live focused input field.
The same operations have narrower tools suitable for agent grants:
`media_launch {"entity":"media_player.tcl","url":"https://www.netflix.com/title/12345"}`
and `media_text {"entity":"media_player.tcl","text":"A movie title"}`.
These require `--allow-control` and do not grant `entity_configure` or permit
changing other devices' settings. They exist only for adapters with app/text
support; `entity_options` reports the live negotiated features and current app
when the TV supplies it. Navigate to a focused search field before sending text.

App links accept HTTP(S) and documented `stremio:///` board, discover, library,
search and detail pages. For example,
`stremio:///detail/movie/tt0066921/tt0066921?autoPlay=true` opens a title and
requests autoplay. Custom installation links and arbitrary intent/file schemes
are rejected. [Stremio documents](https://stremio.github.io/stremio-addon-sdk/deep-links.html)
that autoplay depends on an available prior stream selection. Netflix links
require a compatible installed app and may still show account/profile/title UI.
For Netflix title targeting, use `https://www.netflix.com/title/TITLE_ID?source=30`
as described by the [Android TV Remote integration](https://www.home-assistant.io/integrations/androidtv_remote/#launching-activities).
No tool finds title IDs, sees the TV screen, bypasses subscriptions, or confirms
that video is playing. Use actual supplied title links/IDs and reported results.
Requests are sent once and remain unverified. Supplied PCM voice streaming uses
the bounded `media_voice` tool described above; ADB is not implemented.
Host-only `scripts/android/pair_digi.py` performs PIN pairing
without HA; existing credentials remain the normal runtime path.

Both managers lock their configs and omit credentials from MCP/error output.
Sessions are created per operation and discarded. Validate physical shutdown
and the full power sequence against each installation's firmware and wiring.

### Shared TV outlet workflow

`examples/tv.json` references `samsung`, `digi`, and `plugs` config paths (relative
to its own directory), plus `plug` (normally `tv`) and the required
`shutdown_policy: "fixed_delay"`, plus required
`timings: {"startup_seconds": 5, "shutdown_seconds": 5, "ready_seconds": 120}`.
Startup/shutdown delays must be 1–60 seconds and readiness 1–120 seconds; the
example values are explicit settings, not implicit defaults. Use standard `routine_run` (`tv_on` / `tv_off`), CLI `tv-set`, or legacy
`tv_apply`. With `--tv PATH`, do not load its child configs separately; their
normal tools borrow the same managers. Direct child power requires
`--maintenance`; reads/navigation and other adapters remain available.

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

The explicit fixed-delay policy allows cutoff without physical Samsung-off
confirmation. Success reports workflow completion; DIGI sleep must still verify.

An attempt is durably saved before any step. A failed/interrupted attempt blocks
workflow replay for 180 seconds across restart; success clears it. Individual
Samsung shutdown protection still applies. No failed controls are rolled back,
automatically retried, or caught up. Long workflows occupy the single request
thread, so clients need sufficient timeouts; queued requests execute afterwards.
Disable competing HA TV rules when deliberately transferring ownership.

Protocol references: [Samsung encrypted remote](https://github.com/xchwarze/samsung-tv-ws-api/tree/master/samsungtvws/encrypted),
[HA Samsung bridge](https://github.com/home-assistant/core/blob/dev/homeassistant/components/samsungtv/bridge.py),
[Android TV remote protocol](https://github.com/tronikos/androidtvremote2/blob/main/src/androidtvremote2/remotemessage.proto).

## Computer shutdown service

This optional Linux service uses Ed25519 signatures on fixed 162-byte UDP
frames in both directions. It accepts status/validation and orderly shutdown,
with no shell, SSH login, reboot, file access or arbitrary command interface.
Fresh random challenges expire after 10 seconds, bind the client nonce, device
identity and peer address, and are consumed before controls. There are at most
eight pending challenges; malformed or unauthenticated traffic is discarded
without per-packet logs. The native server has one thread and blocks while idle.
The client has a total 1–10-second request deadline (default 3) and sends each
protocol phase once. An uncertain shutdown is never automatically retried.

`adapters.computer` follows [computer.json](../examples/computer.json): one named
computer, a literal UDP `address`, pinned `server_public_key`, private
`signing_key`, unicast nonzero `mac`, and explicit `wake_address`. The name is
1–24 ASCII letters/digits/underscores/hyphens and must match the server name.
Public keys and signing seeds are 64 hex characters. Store the client signing
seed in Infisical using a `${HOMEWIRE_SECRET_...}` reference. Deployment resolves it into protected backend storage. Backend compromise can
expose it; a stolen authorization key grants shutdown requests, not a general login.
The separate computer identity key authenticates responses and cannot authorize
shutdown. Keep its seed root-only and deliver it to the unprivileged service
through systemd `LoadCredential`.

Build both native executables with `cargo build --release --locked` on the
computer. Generate the client seed into a protected temporary file outside the
repository with `homewire-power keygen PATH`, which prints only the public key.
Store the seed in Infisical, delete the temporary file, and put its public key
in a private copy of [computer-server.json](../examples/computer-server.json).
Set `listen` to the computer's LAN address; the default example uses UDP 39817.
Install with root privileges:

```sh
sudo sh scripts/computer/install.sh target/release/homewire-power SHA256 SERVER_JSON
```

Supply the binary's exact SHA-256. The installer verifies it, creates a dedicated
non-login account, keeps versioned binaries, generates the root-only response
identity once, and prints its public key for pinning. Existing identity and
authorization configurations are preserved; a different configuration requires
explicit rotation. The listener is enabled at boot with systemd hardening.
Polkit grants that account only starting the fixed, root-owned
`homewire-poweroff.service`, whose sole action is orderly OS poweroff.
The installer does not run that action. Restrict LAN exposure with the target's
firewall as appropriate; neither an obscure port nor source IP replaces signatures.

Run `homewire-power status CLIENT_JSON` for authenticated running/pending status
and `homewire-power validate CLIENT_JSON` for a challenge/signature round trip
without shutdown. Validation does not execute the OS action. Unreachable is
unknown, never confirmed off. `homewire-power shutdown CLIENT_JSON` is the
deliberate administrative control; its acknowledgement proves only that the
fixed OS action was queued. The server rejects another shutdown until restart.

For coordinated power, add:

```json
"routines": {
  "computer": {
    "plug": "metered_outlet",
    "startup_seconds": 5,
    "ready_seconds": 120,
    "shutdown_policy": "metered_standby",
    "shutdown_verification": {
      "standby_power_w": 1.0,
      "stable_seconds": 30,
      "timeout_seconds": 180,
      "poll_seconds": 2
    }
  }
}
```

Startup delay is 1–60 seconds; readiness is 1–120 seconds. `computer_on` verifies
the outlet, turns it on if needed, waits for standby power, sends one WoL packet
if the authenticated service is not already running, and verifies readiness.
Startup reads identity, relay and protection state without energy statistics;
an unavailable energy-statistics method does not block waking the computer.
Enable persistent WoL on the host NIC separately; wake after outlet power loss
also depends on firmware support. Metered `computer_off` reads the P110 relay and meter,
requires auto-off to be confirmed inactive, and requires running consumption
more than 10 W above the configured standby threshold before sending one signed
orderly shutdown request. After authenticated acceptance, it waits for both the
service to become unreachable and consumption to stay at/below the calibrated
threshold continuously for the configured interval, with at least three fresh
samples. Any reachable service or higher consumption restarts that interval.
Only then does it persist the cutoff attempt, turn off the outlet and verify
its relay. Missing/invalid consumption, unknown/active auto-off, authentication
or protocol errors, device errors and timeouts abort without cutting power.
Transport unavailability alone never authorizes cutoff.

Use [computer-routine.json](../examples/computer-routine.json) as a schema example,
not as a universal threshold. Measure actual orderly-shutdown standby consumption
with the monitor/charger off or disconnected, account for observed variation and
a small margin, and verify an entire off/on cycle before enabling the policy.
Thresholds are 0.05–5 W, stability 10–60 seconds, polling 1–5 seconds and timeout
30–300 seconds, at least 20 seconds longer than stability. Choose timeouts that
fit the caller's timeout including adapter reads; the standard bridge waits 300
seconds. Shared monitors and chargers can keep consumption above the threshold,
in which case the outlet safely stays on. Recalibrate after hardware/firmware
changes. Never raise the threshold merely to make a running monitor/charger pass.

Results report `shutdown_verified:true` under the explicit `metered_standby`
evidence policy, its measured maximum standby draw, elapsed stable interval,
sample count and verified outlet state. They retain `physical_off_verified:false`:
this is corroborated electrical/network evidence, not an independent hardware
power-state sensor. Neither an acknowledgement nor disappearance alone proves
physical shutdown. To disable cutoff, use `shutdown_policy:"leave_on"` and omit
`shutdown_verification`; that policy requests shutdown and keeps the outlet on.

Standard `routine_list`, `routine_plan`, `routine_run`, `entity_status` and
`wake_list` discover this adapter without family-specific MCP tools. Plans do
not query devices. Direct computer wake/shutdown, outlet power and outlet auto-off
changes require explicit `--maintenance` while the routine owns them. Workflow
attempts are persisted before controls and replay is blocked for 180 seconds
after an uncertain result, including across backend restart. Accepted shutdowns
also persist their acceptance time and a fingerprint binding both devices and
credentials. For up to 10 minutes, an explicit `computer_off` call may resume
verification after timeout/restart without resending shutdown. Changing a
calibrated threshold does not change that equipment fingerprint. Changed devices
or credentials, expired acceptance and unresolved attempts fail explicitly.
Acceptance is consumed and the attempt timestamp refreshed before relay cutoff,
so an uncertain cutoff cannot be replayed as a resumed verification. Errors leave
outlet power unchanged after any completed startup step; no implicit rollback occurs.

Disable with `sudo systemctl disable --now homewire-power.service`. Remove its
Polkit rule to revoke the OS privilege. For binary rollback, select the preserved
`/opt/homewire-power/previous` version and restart only the listener; retain keys
and authorization config. A phone rollback must restore its matching config and
binary, preserving all monitor, schedule, alert and device-attempt state.

## Wake-on-LAN

The Samsung adapter contributes
`samsung.samsung` to `wake_list`, including when owned by the TV routine.
`wake_on_lan {"device":"samsung.samsung"}` follows the same identity checks and
verified wake path as `media_power` with `power:true`. TV-owned wake requires
explicit `--maintenance`; normally use `routine_run {"routine":"tv_on"}` so
outlet startup and the other media device remain coordinated.

Configure other known wake targets in `adapters.wol` in the unified configuration:

```json
{
  "devices": [
    {"name":"workstation","mac":"02:00:00:00:00:01","wake_address":"192.0.2.255:9"}
  ]
}
```

The separate-file equivalent is `--adapter wol=.local/wol.json`; see
[examples/wol.json](../examples/wol.json). Keep real inventories and addresses in
private configuration. The adapter accepts 1–8 devices with unique names of
1–24 ASCII letters, digits, underscores or hyphens, nonzero unicast MACs, and
an explicit UDP destination with a nonzero port. A subnet broadcast destination
is typical. Duplicate MACs, including aliases of Samsung targets, are rejected.
Configuration is locked across processes; use daemon/bridge for multiple clients.

`wake_list {}` returns device IDs, whether wake can be verified, and whether TV
ownership requires maintenance. It performs no network requests and exposes no
MACs or destinations. `wake_on_lan {"device":"wol.workstation"}` selects only
configured IDs, accepts no caller-supplied MAC/address, and sends one 102-byte
magic packet without retries. Generic results contain `sent:true`,
`verified:false` and `evidence:"magic_packet_sent_once"`. No reachability probe,
power state, shutdown capability or successful wake is inferred from the send.
Hardware must support and enable network wake, retain standby power, and be
reachable by the configured LAN destination; an unpowered outlet must be restored
through its owning routine first.

The tools work through `serve`, daemon/bridge and authenticated API MCP. Controls require
`--allow-control`. Grant agent tools separately; deploying the adapter never expands ZeroClaw grants.
