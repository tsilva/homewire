# Development and architecture

[Reference index](reference.md) · [First use](../README.md)

## Implementation and validation

### Public repository privacy

Install the local privacy hooks once per clone (Python 3, standard library only):

```sh
python3 scripts/privacy_guard.py install
```

The installer preserves existing/custom hooks rather than replacing them. If
you already use hooks, integrate the `staged` and `pre-push` commands explicitly.
Installed standalone copies keep working on branches without the source script;
repeat installation after updating the guard or its denylist.

The pre-push hook checks every outgoing ref's complete ancestry and reachable
objects against `scripts/homewire-sensitive-history.txt`. This blocks retired
commits, copied historical files and tags to old trees/blobs, including new
branches and force pushes. Deletions remain allowed. Missing/corrupt inspection
data and shallow clones fail closed; no device or network access is performed.
The pre-commit hook rejects retired blobs and tracked private configuration
paths, including force-added `.local/`, `.env`, `.access.local.toml` and
`.infisical.json` files. Template `.env.example` files are allowed.

Before each commit, review the full staged diff and commit message for household
identifiers, network/location data, actual schedules, inventories, credentials
and deployment records. Keep those in ignored `.local/` files. Hash checks cannot
detect newly written private details; passing the guard does not replace review.
Hooks are local safeguards and can be deliberately bypassed; do not bypass them.

```sh
python3 scripts/privacy_guard.py staged
python3 scripts/privacy_guard.py check HEAD
PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s scripts/tests -p 'test_*.py'
```

### Required checks

Run from the repository root:

```sh
cargo fmt --check
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
```

Android deployment changes also require:

```sh
PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s scripts/android -p 'test_*.py'
shellcheck scripts/android/build.sh scripts/android/sandbox/build.sh scripts/android/sandbox/build-identity.sh
```

TV companion changes also run `sh companion/tv/test.sh` with `JAVA_HOME` set
to JDK 17, Python installation tests under `companion/tv/tests`, and shellcheck
on its two shell scripts. Follow the [TV companion build and hardware checks](../companion/tv/README.md)
for a signed installation, pinned authentication and deliberate native controls.

CI also compiles the TV APK unsigned with JDK 17, API 35 and build-tools 35.0.0,
and runs its Java/Python tests, Android deployment tests and shellcheck without
credentials. Local signed updates use `python3 companion/tv/release.py --version VERSION`;
see the [release setup and recovery contract](../companion/tv/README.md#one-command-releases).

Sandbox changes must build the native guard and run its negative probe on the
target. CI runs Rust checks and a release build with Rust 1.95.0 on Linux.
Synthetic tests cover protocol framing, stale/unknown readings, identity/signature
rejection, uncertain controls, durable faults and workflow ordering. Physical-device
acceptance remains deliberate; keep deployment results private.
[Historical measurements](validation.md) describe earlier builds.

Device protocols stay inside adapters; household sequences belong to `routine`.
CLI and MCP share validated commands and capability entities. Shared modules cover
configuration/locking (`config`, `bundle`), schemas (`entity`, `settings`), framing
(`tuya`, `klap`, `ipp`, `protobuf`), network deadlines (`transport`, `http`,
`websocket`, `media`), scheduling and durable fault confirmation (`fault`).

Release builds use stripping, LTO and panic abort. Ordinary errors return through
`Result`; panics exit without unwinding. Persistence precedes controls/events and
OS locks release on exit. See [daemon and bridge](automation.md#persistent-daemon-and-mcp-bridge)
for transport threads, queues and deadlines.

### Static device adapters

`device::Adapter` provides configuration loading, legacy tools/validation, static
`SUPPORT` category flags, borrowed `entities()`, `read_device(index)` snapshots
and optional `control_device(index, command, arguments)`. Read-only adapters
inherit a rejecting control implementation. Protocol validation, identity checks,
live ranges, attempt guards and verified writes belong to the adapter.

Entities borrow configuration and describe domain, label, physical-device route,
snapshot field/unit, capabilities and routine ownership. IDs remain stable while
labels do. No entity-state database, allocated registry or schema cache is retained.
`Devices::validate()` checks ownership/duplicate IDs without heap allocation;
embedding callers should validate after construction and use `entity::call`.

`static_adapters!` in `src/device.rs` generates concrete inline storage, `Family`,
discovery and dispatch from one list. Its `plugs/Plugs: crate::plug::Manager =>
tv.plugs` entry borrows the workflow-owned manager, preserving one ownership lock.
Standard schemas live with capabilities/routines; adapter schemas support legacy
clients. Both validate before I/O. MCP also gates writes with `Hints.read_only`
and `--allow-control`. `mcp::handle_entities` is the standard entry point;
`handle_selected`, `handle_configured` and `ToolSet` retain Rust compatibility.

`device::Monitored` supplies optional typed fault monitoring. Each physical adapter
owns its monitor state and next deadline through `next_background`/`background_tick`.
Batch reads share one snapshot per physical device, then discard it. Confirmed
state and attempts remain bounded and persist before events or controls. Unknown
readings cancel pending confirmation without clearing saved faults; a failed save
leaves published memory intact and returns no event.

To add a family:

1. Implement bounded configuration/protocol behavior and borrowed entities. Reuse
   existing domains, capabilities and matching transports; exclude credentials
   from metadata.
2. Implement `Adapter`, choose static `SUPPORT`, and register the concrete type in
   `static_adapters!`. Existing categories require no MCP routing changes.
3. Implement `from_source` with the shared typed decoder. Services declare
   `CONFIG_SECTION = "services"`; hardware defaults to `adapters`. Unified loading
   follows the catalog. Use `Monitored` for faults and `routine` for multi-device
   sequences with explicit discovery/execution.
4. Test validation, batch sharing, unknown states, ownership, synthetic controls
   and allocation bounds; update docs and run the required checks. Rebuild for
   the target OS/ABI and measure target RSS before claiming a footprint.

### Memory and allocation checks

All adapters share one process without retained session caches. An enabled
brightness schedule lazily starts at most nine device I/O threads, each with a
256 KiB configured stack, one in-flight cycle and no pending successor. These
threads block for work and have no timers; one coordinator owns the curve and
deadline. They isolate blocking protocol clients and join before ownership locks
are released. This stack setting is not a resident-memory measurement.
Idle MCP blocks on stdin; background jobs add one reader thread with a 128 KiB
stack and a one-message queue. Inputs use a 1 KiB buffer and a 64 KiB message cap.
Screen-capable adapters return temporary JPEG payloads bounded to 512 KiB and
1280 × 720; MCP emits one image block without duplicating base64 in JSON metadata.
The private IPC reply limit remains 1 MiB. Generic media-screen capabilities and
adapter hooks own routing; MCP only formats the category's native image result.
Waits follow enabled deadlines: adapter polls, light schedules or minute boundaries
for Meaco rules. Pre-initialization waits directly for input. Serialized operations
can delay jobs; Meaco rules have minute-wide windows with no late catch-up.

Adapter configs/state are typically bounded to 8 KiB, unified config to 64 KiB,
and endpoint groups to eight devices. Readings, schemas and replies are temporary.
KLAP uses one exactly sized encryption frame and decrypts/authenticates responses
in place; application responses allow 16 KiB plus 48 bytes overhead. Typed reply
envelopes avoid JSON clones. Unchanged fault decisions and identity validation
allocate nothing; network reads and MCP replies still allocate temporary storage.

Run the synthetic allocation probe (no device I/O):

```sh
cargo run --release --locked --example allocation_probe
```

It reports requested allocation counts/bytes, not live heap or RSS. For process
samples, use Python 3 and `ps` on macOS/Linux:

```sh
cargo build --release --locked
python3 scripts/measure_memory.py
python3 scripts/measure_memory.py --media
python3 scripts/measure_memory.py --media --legacy-tools
python3 scripts/measure_memory.py --binary /path/to/previous/homewire
```

The harness creates private synthetic configs for Meaco, two printers, Awair,
three lights and eight plugs; `--media` adds Samsung/DIGI. Three trials each
initialize MCP and issue 1,000 tool-list requests, sampling RSS every 100 requests.
No device traffic or background jobs run. Samples are neither true peaks nor
Android guarantees; profile idle and active workloads on the target.

Android builds use a pinned NDK and `aarch64-linux-android`; see
[deployment](android.md#android-deployment).
Protocol sources: [Tuya Local's Meaco mapping](https://github.com/make-all/tuya-local/blob/main/custom_components/tuya_local/devices/meaco_aretetwo_dehumidifier.yaml),
[TinyTuya framing](https://github.com/jasonacox/tinytuya/blob/master/PROTOCOL.md),
[IPP encoding (RFC 8010)](https://www.rfc-editor.org/rfc/rfc8010.html) and
[semantics (RFC 8011)](https://www.rfc-editor.org/rfc/rfc8011.html).
Tuya 3.3 CRC detects corruption without authenticating the peer; use a trusted LAN.

## Brand assets

[`logo.png`](../logo.png) is the README logo. Sources in `image-assets/sources/`
and exports, favicons, web manifest and 1200×630 social image are indexed in the
[asset manifest](../image-assets/manifest.json). The outlined artwork supports light
and dark backgrounds. The asset web manifest is not installed automatically.
