# Configuration and CLI

[Reference index](reference.md) · [First use](../README.md)

## Unified configuration

Computer sequencing uses `adapters.computer` plus `routines.computer`; see
[computer setup](devices.md#computer-shutdown-service). Its outlet may share the plugs
adapter with TV but cannot be the outlet owned by the TV routine.

The primary input is `homewire --config .local/home.json COMMAND`.
Copy [examples/home.json](../examples/home.json) into ignored `.local/`,
protect it with `chmod 600`, replace the synthetic values and remove unused
families. The file is bounded to 64 KiB and requires `version: 1`.

| Section | Contents |
| --- | --- |
| `adapters` | `meaco`, `printers`, `awair`, `lights`, `plugs`, `samsung`, `digi`, `tcl`, `brother`, `cast`, `dlna`, `wol`, `computer`; objects use the existing adapter schemas below |
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

Optional TCL `wake`, `cast_wake` and `power_on_key` fields are described in
[TCL power and network standby](devices.md#tcl-power-and-network-standby).
Defaults retain the existing WAKEUP key with no integrated network/Cast wake.
Cast wake requires its own certificate pin and firmware validation; adding a
separate Cast playback adapter does not enable it.

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

Homewire stores confirmed base mode and schedule attempts alongside the config
as `meaco.state.json`; `meaco.json.lock` serializes ownership. Use the same config
path for all clients of one device. A running server holds the lock; access it
through MCP instead of starting a concurrent CLI instance. This lock cannot
coordinate with Home Assistant or the manufacturer's app.

## Command-line usage

Run from the repository root with your private unified configuration prepared.
Only commands for configured adapters are available:

```sh
target/release/homewire --help
target/release/homewire --config .local/home.json status
target/release/homewire --config .local/home.json plan night
target/release/homewire --config .local/home.json apply day
target/release/homewire --config .local/home.json apply night
target/release/homewire --config .local/home.json apply off
target/release/homewire --config .local/home.json printer-status brother
target/release/homewire --config .local/home.json awair-status
target/release/homewire --config .local/home.json light-status led_strip
target/release/homewire --config .local/home.json light-set led_strip '{"power":true,"brightness_percent":10}'
target/release/homewire --config .local/home.json plug-status metered_outlet
target/release/homewire --config .local/home.json plug-set relay off
target/release/homewire --config .local/home.json tv-status
```

`status`, `plan`, and `apply` address Meaco; `plan` queries live state without
controls. `apply`, `light-set`, and `plug-set` change devices. CLI command metadata supplies help, argument counts and adapter requirements.
Legacy names translate at the boundary into canonical commands. CLI controls are
explicit commands; `--allow-control` gates MCP writes. Results are JSON on stdout;
failures go to stderr with a nonzero exit code. Meaco sleep is a toggle and is
never automatically resent. Keep the Meaco settling delay at 20 seconds.

Use `watch-printers`, `watch-lights` or `watch-plugs` for continuous fault/recovery
events. Monitoring retains confirmed faults across restarts; unknown readings
never prove recovery. [The optional alert ledger](services.md#alerts-history-and-convenience-tools)
adds HTTPS/Telegram delivery and companion notifications with persisted receipts.

Legacy per-family examples remain below; do not combine their configuration
flags with a unified `--config`. Use one config path per device across clients;
access an existing owner through [MCP or bridge](automation.md#persistent-daemon-and-mcp-bridge).

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
