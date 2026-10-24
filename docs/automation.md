# Agents and automation

[Reference index](reference.md) · [First use](../README.md)

## MCP and ZeroClaw

Start a local stdio MCP server:

```sh
target/release/homewire --config .local/home.json serve
```

### Standard entities and routines

The standard interface exposes configured capability entities and household
routines. Writes require `--allow-control`; read-only is the default.

| Tool | Arguments | Availability |
| --- | --- | --- |
| `entity_list` | Optional `domain`, `device`, `offset`, `limit`, `details` | Paginated discovery; no network I/O |
| `entity_options` | `{"entity":"light.bulb_1"}` | Live choices where implemented |
| `entity_configure` | `{"entity":"dehumidifier.meaco","settings":{"target_humidity":55}}` | Advertised typed settings + control flag |
| `media_play` | `{"entity":"media_player.cast_player","media":{"url":"https://example.org/music.mp3","content_type":"audio/mpeg"}}` | Cast/DLNA + control flag |
| `media_command` | `{"entity":"media_player.dlna_player","command":{"action":"volume","percent":20}}` | Playback + control flag |
| `entity_status` | `{"entities":["light.bulb_1","sensor.awair_co2"]}` | Fresh batched reads |
| `dehumidifier_set` | `{"entity":"dehumidifier.meaco","settings":{"purification":true,"sleep":false}}` | Independent mode booleans + control flag |
| `light_set` | `{"entity":"light.led_strip","settings":{"power":true,"brightness_percent":30}}` | Lights + control flag |
| `brightness_schedule_status` | `{"entity":"light.led_strip"}` | Configured brightness target; no device I/O |
| `brightness_schedule_resume` | `{"entity":"media_player.tcl"}` | Configured brightness target + control flag; clear its manual pause and request the current target |
| `switch_set` | `{"entity":"switch.tv","power":false}` | Plugs + control flag; TV outlet requires maintenance |
| `media_power` | `{"entity":"media_player.digi","power":true}` | Media + control flag; routine-owned power requires maintenance |
| `wake_list` | `{}` | Configured Samsung or generic WoL targets; no network I/O |
| `wake_on_lan` | `{"device":"wol.workstation"}` | Known wake targets + control flag; TV ownership requires maintenance |
| `remote_key` | `{"entity":"media_player.samsung","key":"hdmi"}` | Media + control flag |
| `routine_list` | `{}` | Configured routines |
| `routine_plan` | `{"routine":"meaco_night"}` | Configured routines; no controls |
| `routine_run` | `{"routine":"tv_on"}` | Configured routines + control flag |

IDs use `<domain>.<configured-label>` for lights, switches and printers. Singleton
IDs are `dehumidifier.meaco`, `media_player.samsung`, and `media_player.digi`.
The previous `humidifier.meaco` ID remains accepted as a compatibility alias;
discovery advertises the Meaco as a dehumidifier.
Awair contributes six sensor entities: `sensor.awair_temperature`,
`awair_humidity`, `awair_co2`, `awair_tvoc`, `awair_pm25`, `awair_score` (each with
`sensor.` prefix). P110 adds `<label>_power`, `_voltage`, `_current`,
`_today_energy`, `_month_energy`, `_total_energy` under `sensor.`. Their units are
explicit. A physical `device` groups its entities. Discovery reports capability
ranges, remote keys and routine ownership; live constraints are checked before
controls. See [typed settings](services.md#typed-settings) and the
[device guides](devices.md) for family-specific behavior.

`dehumidifier_set` accepts only `purification` and `sleep` booleans, preserving
omitted modes, power and humidity. Turning purification off restores normal
dehumidification. While asleep it clears sleep once, verifies/changes the base
mode, then restores sleep. Uncertain controls stop without replay. Purification
status is null while sleep hides the underlying mode. Grant this narrower tool
when timers/other `entity_configure` settings are unnecessary. Meaco routines are
`meaco_day`, `meaco_night`, `meaco_off`; TV routines are `tv_on`, `tv_off`.

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

For a configured TCL TV routine, use `routine_run` with `tv_on` or `tv_off`.
The routine owns TCL and its selected outlet: startup powers the outlet first;
shutdown verifies TCL power-off before cutting the outlet. An uncertain TV result
leaves the outlet on. Agent channels need an explicit `routine_run` grant.

When legacy `--tv` is configured, its Samsung, DIGI and entire plug adapter are borrowed
from one owner. Loading separate adapters for those families is rejected, even
with different config paths. Direct power commands for the two media devices
and the TV's selected outlet are blocked in CLI and both MCP interfaces.
`--maintenance` explicitly permits those direct controls, with device validation
and verification still enforced. Other outlets, reads and navigation keys remain
available. MCP writes also require `--allow-control`.

Standard mode rejects legacy tool names. Existing consumers can use
`--legacy-tools` until migrated. Monitoring uses the watch flags and writes events
to stderr, keeping MCP stdout clean.

### Legacy adapter tools

Add `serve --legacy-tools` for the original interface described below.

Only loaded families appear; all writes also require `--allow-control`.

| Family | Read tools | Write tools / example arguments |
| --- | --- | --- |
| Meaco | `meaco_status`, `meaco_plan` | `meaco_apply {"operation":"day"}` (`night` / `off` also accepted); plan uses the same argument |
| Printers | `printer_list`, `printer_status {"printer":"brother"}`, `printer_check` | None |
| Awair | `awair_status` | None |
| Bulbs | `bulb_list`, `bulb_status {"bulb":"bulb_1"}`, `bulb_check` | `bulb_apply {"bulb":"bulb_1","settings":{"power":true,"brightness_percent":10}}` |
| Lights | `light_list`, `light_status`, `light_check` | `light_apply`; uses `light` instead of `bulb` |
| Samsung / DIGI | `samsung_status` / `digi_status` | Family `_apply {"power":true}` and `_key {"key":"home"}` |
| TV | `tv_status`, plus child tools | `tv_apply {"power":false}`; direct child power still requires maintenance |

Per-family loading flags remain supported; do not combine them with unified
`--config`. For example:

```sh
target/release/homewire --printers .local/printers.json serve --legacy-tools --watch-printers
```

Repeated check calls in one server confirm/deduplicate events. Automatic watch
jobs begin after MCP initialization and first poll within their configured
interval. `--watch-lights` covers bulbs/L900; `--watch-bulbs` remains an alias.
Queries are sequential: allow client timeouts for multiple devices and Meaco's
20-second settling delay per step. Cancellation does not abort an in-flight sequence.

The server implements initialization, ping, tool listing and tool calls for
MCP 2024-11-05, 2025-03-26, 2025-06-18, and 2025-11-25. Messages are newline-delimited JSON-RPC,
limited to 64 KiB. Device operations run sequentially; stdout contains only
protocol messages. For authenticated HTTP MCP, use the
[agent endpoint](android.md#trusted-homewire-mcp-endpoint). Model inference is
separate.

For ZeroClaw 0.8.x, add a server to an MCP bundle, then reference that bundle
from the appropriate agent. Use absolute executable and config paths; start
without `--allow-control` during development. See
[ZeroClaw's configuration reference](https://docs.zeroclaw.com/v0.8.3/en/reference/config.html#mcp_bundles)
for the exact schema supported by your installed version.

## Scheduling

The scheduler uses `automations.meaco` and `routines.meaco` in unified config
(legacy `schedule` and `presets`).
The timezone is parsed once at startup; day/night times have minute resolution
and follow that timezone's daylight-saving rules. Missing schedule settings cause
an enabling command to fail at startup, before any device controls.

```sh
# Standalone scheduler:
target/release/homewire --config .local/home.json schedule

# Or one process combining MCP and scheduling:
target/release/homewire --config .local/home.json serve --allow-control --schedule
```

Disable the corresponding Home Assistant rules before enabling this scheduler.
Starting it outside a scheduled minute does not change the device or catch up
missed runs. It attempts each rule once per local date, persists that attempt
before control, and does not replay failed or interrupted runs after restart.
Failures go to stderr and, when configured, the
[alert ledger](services.md#alerts-history-and-convenience-tools). The MCP client
must keep stdin open; EOF stops `serve`. Supervise the standalone scheduler when running without an MCP client.

## Persistent daemon and MCP bridge

`serve` remains the original standalone stdio server and exits on stdin EOF.
Use `daemon SOCKET` for an independent device owner:

```sh
mkdir -p .local/run
chmod 700 .local/run
target/release/homewire --config .local/home.json daemon .local/run/homewire.sock
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

The socket parent must be a real directory with mode 700; socket/lock use 600.
Access is limited to the same OS user. The ownership lock permits stale-socket
recovery after crashes and refuses replacing regular files/symlinks. Android
uses Bionic `flock` because Rust 1.95's `File::try_lock` is unsupported there;
desktop uses the standard-library lock. Other lock failures preserve their cause.

One 512 KiB-stack accept thread validates frames and serves initialization,
ping and discovery while the device/job thread is busy. Scoped agent discovery
preserves its allowlist and control permissions. Metadata responsiveness does
not establish device availability or completion of in-flight work.

Device calls, mobile reports/acknowledgements and schedules remain serialized on
the device owner. Up to four device requests can wait in the bounded queue, accommodating both API
workers and ordinary bridge clients. The owner services up to four admitted calls
between background ticks, so every queued call need not wait for a fresh polling
batch; the bound also gives jobs a turn under sustained client load. When full,
new device work receives JSON-RPC error -32000 stating that it was not queued or
executed; the agent HTTP endpoint exposes the error as HTTP 503 with the caller's
request ID. Neither transport automatically retries it. Metadata remains available
while that queue is full. The OS bounds the listen backlog.

Requests remain limited to 64 KiB; private replies are limited to 1 MiB. Framing and
reply writes each have an absolute five-second deadline, including fragmented I/O;
the bridge waits up to 300 seconds for device work. An incomplete same-user client
can delay the accept thread until its framing deadline. Only trusted same-user
clients should use this socket. Slow physical-device operations can still delay
other device calls and jobs; unavailable readings and protocol timeouts remain
explicit and never count as successful device recovery.
A timeout/disconnect never retries a control. Bridges exit with an explicit
transport error; after a lost reply, inspect fresh state before another action.
Supervision can restart the daemon; existing durable attempt guards still apply.
