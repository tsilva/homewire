# Agents and automation

[Reference index](reference.md) · [First use](../README.md)

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
| `entity_configure` | `{"entity":"dehumidifier.meaco","settings":{"target_humidity":55}}` | Advertised typed settings + control flag |
| `media_play` | `{"entity":"media_player.cast_player","media":{"url":"https://example.org/music.mp3","content_type":"audio/mpeg"}}` | Cast/DLNA + control flag |
| `media_command` | `{"entity":"media_player.dlna_player","command":{"action":"volume","percent":20}}` | Playback + control flag |
| `entity_status` | `{"entities":["light.bulb_1","sensor.awair_co2"]}` | Fresh batched reads |
| `dehumidifier_set` | `{"entity":"dehumidifier.meaco","settings":{"purification":true,"sleep":false}}` | Independent mode booleans + control flag |
| `light_set` | `{"entity":"light.led_strip","settings":{"power":true,"brightness_percent":30}}` | Lights + control flag |
| `switch_set` | `{"entity":"switch.tv","power":false}` | Plugs + control flag; TV outlet requires maintenance |
| `media_power` | `{"entity":"media_player.digi","power":true}` | Media + control flag; routine-owned power requires maintenance |
| `wake_list` | `{}` | Configured Samsung or generic WoL targets; no network I/O |
| `wake_on_lan` | `{"device":"wol.workstation"}` | Known wake targets + control flag; TV ownership requires maintenance |
| `remote_key` | `{"entity":"media_player.samsung","key":"hdmi"}` | Media + control flag |
| `routine_list` | `{}` | Configured Meaco/TV routines |
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
explicit. A physical `device` groups all its entities; adding entities does not
create connections, threads, copies of credentials or separate managers.

Discovery reports model capability envelopes, remote-key choices and power
ownership. Light temperature ranges are model envelopes; actual live ranges
are checked before controls. Light settings can disable or restore the device's stored effect; L900 fixed
settings still require effects/music mode confirmed off. Meaco also exposes typed
humidity, modes, child lock and timer settings. The narrower `dehumidifier_set`
accepts only `purification` and `sleep` booleans: turning purification off restores
normal dehumidification; omitted modes, power and humidity target are preserved.
While asleep, changing purification clears sleep once, verifies the underlying
mode, changes it if necessary, and restores sleep. Uncertain controls stop without
retries. Status reports `sleep` and `purification`; purification is null while
sleep hides its underlying mode. This tool can be granted to an assistant without
granting `entity_configure` or access to timers and other device settings. `meaco_day`, `meaco_night` and
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
advertises only capabilities supported by loaded adapters; tool availability follows the configured
adapters and enabled controls. It rejects legacy tool names.
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

The daemon has one 512 KiB accept thread and one device/job thread. The existing
accept thread validates frames and answers initialization, ping and tool discovery
without waiting for device polling. Scoped agent MCP metadata uses the same path,
with its configured tool allowlist and control permissions preserved. This confirms
protocol responsiveness; it does not prove that a physical device is reachable or
that an in-flight device operation has finished.

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
