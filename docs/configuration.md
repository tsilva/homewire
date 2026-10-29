# Configuration and CLI

[Reference index](reference.md) · [First use](../README.md)

## Unified configuration

The primary input is `homewire --config .local/home.json COMMAND`.
Copy [examples/home.json](../examples/home.json) into ignored `.local/`,
protect it with `chmod 600`, replace the synthetic values and remove unused
families. The file is bounded to 64 KiB and requires `version: 1`.

| Section | Contents |
| --- | --- |
| `adapters` | `meaco`, `printers`, `awair`, `lights`, `plugs`, `samsung`, `digi`, `tcl`, `brother`, `cast`, `dlna`, `wol`, `computer`; see [device guides](devices.md) |
| `groups` | Up to eight explicitly configured light groups for concurrent absolute power |
| `routines.meaco` | Day/night presets, previously `meaco.json`'s `presets` |
| `routines.tv` | TCL outlet sequencing below, or legacy `plug`, `shutdown_policy`, `timings` for Samsung/DIGI |
| `routines.leave_house`, `routines.sleep` | Explicit shutdown lists, with optional PC/kitchen groups for leave_house |
| `routines.wake_up` | Explicit power-on list with required device exclusions |
| `automations.meaco` | `timezone`, `day`, `night`, previously Meaco `schedule` |
| `automations.lights` | L900 curve/location/timezone settings, previously lights `automation` |
| `services` | `api`, `alerts`, `history`, `mobile`, `weather`, `shopping`, `radio`, `speech`; see [services](services.md) |
| `deployment` | [Host-only Android installation profile](android.md#private-installation-profile) |

Sections are optional; routines/automations require their adapters. Unknown keys,
misplaced families, unsupported versions and conflicting declarations fail loading.
Computer sequencing uses `adapters.computer` plus `routines.computer`; see
[computer setup](devices.md#computer-shutdown-service). It may share the plugs
adapter with TV, but cannot own the TV outlet.

Configuration is read once at startup. Relative certificate/history paths resolve
beside `home.json`; runtime state and locks also live there (`meaco.state.json`,
`lights.state.json`, `tv.state.json`, `FAMILY.json.lock`, plus `home.json.lock`).
Use one config directory per installation and [daemon/bridge](automation.md#persistent-daemon-and-mcp-bridge)
for additional clients. Temporary JSON and host-only `deployment` data are discarded.

### Device groups

Define reusable groups independently of their physical adapters. The first group
capability is verified absolute light power; each group has 1–8 explicit light
entity members, with at most eight named groups. For example:

```json
"groups": {
  "sample_lights": {"members":["light.bulb_1","light.bulb_2","light.led_strip"]}
}
```

`group_list` discovers group IDs and membership without device I/O.
`group_plan` accepts `{"group":"group.sample_lights","power":false}` and
previews the resolved members without queries or controls. `group_set` takes
the same arguments and requires the ordinary control grant. Independent members
run concurrently on bounded, request-scoped workers. Starting the requests close
together is best effort; network/device delays prevent guaranteed simultaneity.
Each member retains its adapter identity checks, fault guards, absolute controls
and verification. Brightness, colour and effects are preserved by power-only
changes. No new polling or persistent workers are introduced.

All members resolve before controls begin. Unknown entities, nested groups,
non-light members and duplicate physical devices fail validation. Groups cannot
bypass routine-owned power. Every member is attempted once; success requires
all members verified, and failures include their individual results. There is no
rollback or automatic replay after partial/uncertain results. Configure room
membership explicitly, including any intended strips; discovery never expands it.

### Explicit household shutdown lists

Configure `routines.leave_house` and `routines.sleep` independently. Each has a
nonempty `steps` array; the combined mandatory and optional lists contain at most
16 steps. Nothing is added from device discovery. For example, with the referenced
adapters, TV/PC routines and synthetic outlet names already configured:

```json
"leave_house": {
  "steps": [
    {"action":"routine","name":"tv_off"},
    {"action":"printer","entity":"printer.office","outlet":"switch.printer_power"},
    {"action":"outlet","entity":"switch.heater_power"},
    {"action":"outlet","entity":"switch.iron_power"}
  ],
  "optional_pc": [{"action":"routine","name":"computer_off"}],
  "optional_kitchen": [{"action":"outlet","entity":"switch.kitchen_power"}]
},
"sleep": {
  "steps": [
    {"action":"routine","name":"tv_off"},
    {"action":"printer","entity":"printer.office","outlet":"switch.printer_power"},
    {"action":"outlet","entity":"switch.heater_power"},
    {"action":"outlet","entity":"switch.iron_power"},
    {"action":"routine","name":"computer_off"},
    {"action":"outlet","entity":"switch.kitchen_power"}
  ]
}
```

`leave_house` appends `optional_pc` and `optional_kitchen` only when the caller
explicitly passes the corresponding boolean flag. PC shutdown cannot appear in
its mandatory list or kitchen group. `sleep` accepts no nonempty optional groups.
Requesting an unconfigured optional group fails before device I/O.
Only `tv_off` (verified TCL sequencing) and `computer_off` are allowed as child
routines; arbitrary tools, power-on routines and recursive lists are rejected.
The existing child routine owns device shutdown and dependent outlet cutoff.
PC shutdown without configured completion evidence leaves its outlet on and
reports the household routine incomplete.

`outlet` explicitly permits direct power-off for a simple load. It cannot target
a TV/PC-owned outlet. `printer` checks the outlet first; if it is on, a fresh
read-only IPP result must report `idle` and exactly zero queued jobs before cutoff.
Missing, busy, stopped, unreachable or unknown printer status leaves power on.
This policy permits cutting power to an idle printer; it does not issue a graceful
printer shutdown or prevent new external jobs arriving after the check.
An already-off outlet is verified without querying its unpowered printer.
Duplicate physical targets and unknown entities fail configuration validation.

`light` steps, such as `{"action":"light","entity":"light.bulb_1"}`, turn
one explicitly configured light off. `group` steps, such as
`{"action":"group","group":"group.sample_lights"}`, turn every configured
member off concurrently. Both are off-only and reject extra power-on settings.
They participate in the same duplicate-physical-target validation as other steps.
A failed group retains its member results, marks the routine incomplete and
leaves the durable attempt guard active. Independent steps still run.

Steps run concurrently with up to 16 scoped workers; group members also overlap.
Configuration order controls result order, not command timing. Each child retains
its internal safety ordering: outlets first on startup, verified shutdown before
cutoff, and fresh printer checks before removing printer power. A failed step
leaves its dependent cutoff unexecuted; independent steps continue. Results
include `execution:"concurrent_best_effort"`, `completed:false` and per-step errors
on partial completion. Worker creation failures and unwinding panics mark the
affected step incomplete without retrying it.
There is no rollback or automatic replay. An attempt is saved before controls;
partial/uncertain attempts block replay for 180 seconds, including after restart.
Plans describe configured steps without any device I/O. These routines do not
schedule execution or change the Android host's separate offline sleep schedule.

### Explicit startup list

`routines.wake_up` has `steps` and `excluded_entities`, each containing 1–16
entries. All targets and exclusions resolve before device I/O. There is no
automatic discovery expansion when the routine runs. For example, using synthetic
entities whose adapters and child routines are already configured:

```json
"wake_up": {
  "excluded_entities": ["switch.protected_load"],
  "steps": [
    {"action":"routine","name":"tv_on"},
    {"action":"routine","name":"computer_on"},
    {"action":"power_on","entity":"switch.sample_load"},
    {"action":"power_on","entity":"light.sample_light"},
    {"action":"power_on","entity":"dehumidifier.meaco"}
  ]
}
```

Only `tv_on` (configured TCL sequencing) and `computer_on` are accepted as child
routines. They retain outlet-first ordering, identity checks and readiness
verification. `power_on` supports ordinary switches, lights and dehumidifiers
with a supported absolute power setting. It preserves brightness, colours,
humidity targets, modes and timers. Existing overload, overheating and device
fault guards still apply. Keep printers and their supply outlets out of wake-up
steps. Add both each printer entity and its supply switch to `excluded_entities`,
for example `printer.sample_printer` and `switch.printer_supply`. The physical
device exclusion guard rejects attempts to power an excluded outlet, including
through a child routine; exclusions do not query or switch off those devices.
Other explicitly included supply outlets verify relay power, not attached-appliance
readiness.

Exclusions protect the resolved physical device, including entity aliases and
outlets reached by child routines. An excluded target, duplicate device,
unsupported capability, recursive routine, power-off child, or direct control
of TV/PC-owned power fails configuration validation. Excluded devices receive
no control; the routine does not forcibly switch them off.

Use `routine_plan` or `routine_run` with `{"routine":"wake_up"}`. No optional
flags are accepted. The plan is provisional and performs no device I/O. Runs
attempt independent steps concurrently, each once, and continue siblings on
failure. TV/PC startup and readiness waits overlap with other device chains;
dependent controls inside each chain remain ordered. Results stay in configured
step order and include `execution:"concurrent_best_effort"` plus
`completed:false` for partial completion. An
attempt is saved before controls; partial/uncertain attempts block replay for
180 seconds, including after restart. Never automatically retry them. No
schedule is enabled, and the phone's separate offline schedule is unaffected.

Router login, status, client-based presence and administrative controls are no
longer supported. Remove `adapters.router` from older unified files; the runtime
rejects that section as an unknown adapter. Android migration and installation
ignore legacy `router.json` files and leave existing private files untouched.
Remove obsolete router credential references from your active configuration.

```sh
infisical run --env=prod -- target/release/homewire --config .local/home.json config-check
infisical run --env=prod -- target/release/homewire --config .local/home.json serve
```

`config-check` validates adapter/service schemas, API credentials/TLS and stored
state without device requests or listener binding. It does not verify online
availability. Configuration presence alone does not enable controls or
schedules: `--allow-control`, `--schedule`, `--light-schedule` and watch flags
remain explicit runtime grants. Android updates preserve saved grants.

To sequence a TCL and its outlet, configure `adapters.tcl`, `adapters.plugs`
and this unified routine (the outlet label is synthetic):

```json
"routines": {
  "tv": {
    "target": "tcl",
    "plug": "tv",
    "startup_seconds": 5,
    "ready_seconds": 120
  }
}
```

`tv_on` verifies outlet power first, waits `startup_seconds` (1–60) if it changed,
then waits up to `ready_seconds` (1–120) for pinned TCL readiness after cold
boot before verified power-on. With the outlet already on, the TCL adapter handles
its configured standby wake recovery. `tv_off` requires verified TCL power-off before verified outlet cutoff.
If the outlet is already off, shutdown sends no TV control. Failed or uncertain
TV controls leave the outlet on and retain the 180-second routine attempt guard.
Direct TCL/outlet power and outlet auto-off changes require `--maintenance`.
Other outlets and TCL brightness/navigation remain available. The legacy
Samsung/DIGI routine remains supported with its explicit fixed-delay policy.

TCL recovery is opt-in; see [power and network standby](devices.md#tcl-power-and-network-standby).
A separate Cast playback adapter does not enable it.

To migrate existing private files in place:

```sh
python3 scripts/android/configuration.py --configs .local --output .local/home.json
infisical run --env=prod -- target/release/homewire --config .local/home.json config-check
```

The helper imports known adapter/service JSON, optional `computer-routine.json`
and `deployment.json`, moves
presets/schedules into the unified sections, and removes TV child file
references. Imported originals are copied to a mode-700 `.local/backups/`
directory before being retired. State, history, certificates, keys and unrelated
setup/test records remain untouched. It refuses an existing output, a changed
config directory or mismatching TV child paths. Verify with the new executable
before installation; backups provide recovery if adapter validation fails.

Legacy separate-file flags and legacy Meaco `--config` remain supported.
Do not mix unified `--config` with `--adapter`, `--api` or per-family config
flags. [Device guides](devices.md) document the per-family interfaces. A previous executable that predates unified
configuration requires restoration of the backed-up separate configs and
launcher; binary-only rollback cannot reverse a configuration format migration.

## Configuration

Templates under `examples/` use synthetic names, placeholder credentials and
`192.0.2.x` documentation addresses. Copy needed sections to ignored `.local/`,
replace placeholders (including zero coordinates), and use mode 600. Keep device
serials, inventories, pairing material and deployment records private.

The local key and device ID come from your existing Tuya-local setup. Only
Tuya protocol `3.3` is supported; pairing, discovery and cloud API access are absent.

Resetting or re-pairing a device changes its local key. If a reachable device
rejects a Tuya command, retrieve its current local key, update the corresponding
Infisical secret and securely update the deployed Meaco configuration. Restart
the device owner to load it, then verify a fresh status read before any controls.
Updating the secret alone does not update an already deployed runtime file.

Configuration fields: `device_id`, literal-IP `host`, `local_key`,
`protocol_version`, optional `port` (6668), `timeout_seconds` (8; range 1–30),
and `settle_seconds` (20; range 1–60). Optional `presets` and `schedule` contain
installation behavior; neither supplies an implicit household default.
Do not put device credentials in tracked files or tool arguments.

Meaco confirmed mode and schedule attempts persist in `meaco.state.json`.
The configuration lock excludes concurrent owners; use MCP/bridge while an owner
is running. Locks cannot coordinate with Home Assistant or manufacturer apps.

## Command-line usage

### TCL companion configuration

The optional [TV APK](../companion/tv/README.md) installation helper produces
this object for `adapters.tcl.companion` (or `companion` in a separate TCL file):

```json
{
  "port": 8766,
  "server_cert_sha256": "REPLACE_WITH_PAIRED_CERTIFICATE_SHA256",
  "token": "${HOMEWIRE_SECRET_TV_API_TOKEN}",
  "fingerprint": "REPLACE_WITH_AUDITED_BUILD_FINGERPRINT",
  "incremental": "REPLACE_WITH_AUDITED_BUILD_INCREMENTAL",
  "timeout_seconds": 8
}
```

Keep its token in Infisical and pins/identities in protected private configuration.
The optional `connect` socket address is for host-side tunnels only; deployment
must use the TV's LAN endpoint. Port defaults to 8766; timeout defaults to 8
seconds and is bounded to 1–30. The companion is TCL-only and adds the integer
`backlight_level` setting, 0–100, to the existing media entity. Omit it to retain
remote-only operation. Android deployment resolves its token reference using the
existing protected runtime configuration workflow; it requires no new grants.

To follow the existing light brightness curve, also set
`automations.lights.follow_tv_backlight` to `true` and run the already configured
`--light-schedule` owner with control enabled. See the
[schedule behavior and attempt guards](devices.md#l900-scheduling).
Alternatively, `automations.lights.brightness_entities` accepts standard entity
IDs such as `["light.lamp_a", "media_player.tcl"]`. It defaults to an empty
list. These targets and the legacy follower fields use one shared cycle target
and independent bounded device I/O. Select each physical device only once;
unsupported brightness capabilities and duplicates fail validation. The total
limit is nine devices including the primary L900. Existing grants, opt-in flags,
and durable per-device attempt files still apply.
All scheduled brightness targets automatically use a fixed two-hour manual
override, stored in private sidecars without changing configuration or attempt
schemas. No additional flag or competing scheduler is required. Read and resume
it through `brightness_schedule_status` / `brightness_schedule_resume`; see
[manual brightness overrides](devices.md#manual-brightness-overrides).

### Device commands

Run from the repository root with your private unified configuration prepared.
Only commands for configured adapters are available:

```sh
target/release/homewire --help
target/release/homewire --config .local/home.json status
target/release/homewire --config .local/home.json plan night
target/release/homewire --config .local/home.json apply day
target/release/homewire --config .local/home.json printer-status brother
target/release/homewire --config .local/home.json awair-status
target/release/homewire --config .local/home.json light-status led_strip
target/release/homewire --config .local/home.json light-set led_strip '{"power":true,"brightness_percent":10}'
target/release/homewire --config .local/home.json plug-status metered_outlet
target/release/homewire --config .local/home.json plug-set relay off
target/release/homewire --config .local/home.json tv-status
```

`status`, `plan` and `apply` address Meaco; `plan` reads live state without
controls, and `apply day|night|off` runs the corresponding routine. CLI control
commands are explicit; `--allow-control` gates MCP writes. Results are JSON on
stdout; failures go to stderr with a nonzero exit code. Use `--help` for the full
command list and adapter requirements.

Use `watch-printers`, `watch-lights` or `watch-plugs` for continuous fault/recovery
events. Monitoring retains confirmed faults across restarts; unknown readings
never prove recovery. [The optional alert ledger](services.md#alerts-history-and-convenience-tools)
adds HTTPS/Telegram delivery and companion notifications with persisted receipts.

## Device controls

- **Day/night:** apply the explicitly configured preset: power on if needed,
  clear sleep once if necessary, verify the optional humidity and base mode,
  then enable sleep only when requested. Confirmed matching settings are skipped.
- **Off:** switch off and verify that the device remains off; no preset required.

In a legacy Meaco file, presets and schedule use the following shape. In unified
configuration, place their contents under `routines.meaco` and
`automations.meaco`, respectively.

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

Plans are provisional: execution rechecks state after each step. Keep the
settling delay at 20 seconds.

For testing through a TCP tunnel, `--connect 127.0.0.1:PORT` overrides only the
connection address. The device identity and key remain in the private config.

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
