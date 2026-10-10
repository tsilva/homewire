# Development and architecture

[Reference index](reference.md) · [First use](../README.md)

## Implementation and validation

[Historical measurements and validation](validation.md) are dated evidence, not current guarantees.

Run the required Rust checks from the repository root:

```sh
cargo fmt --check
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
```

Android deployment changes additionally require:

```sh
PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s scripts/android -p 'test_*.py'
shellcheck scripts/android/build.sh scripts/android/sandbox/build.sh scripts/android/sandbox/build-identity.sh
```

Sandbox changes also build the native guard and run its negative probe on the
target. GitHub Actions runs the Rust checks and release build with Rust 1.95.0
on Linux. Tests use synthetic credentials and simulated devices; they do not
replace deliberate physical-device acceptance checks.

The executable uses concrete adapters and static dispatch. Device protocols stay
inside adapters; household sequences belong to `routine`. CLI/MCP clients share
validated commands and capability entities.

- `device`, `entity`, `settings`: catalog, borrowed entities and command schemas.
- `config`, `bundle`: typed inputs, private persistence and exclusive ownership.
- `tuya`, `klap`, `ipp`, `protobuf`: bounded device protocol framing.
- `transport`, `http`, `websocket`, `media`: shared deadline-bound network helpers.
- `meaco`, `bulb`, `plug`, `printer`, `awair`, `samsung`, `androidtv`: device behavior
  and reported-state validation; other families use the same adapter contract.
- `routine`, `schedule`, `light_schedule`, `solar`: deterministic sequences and
  timezone-aware scheduling, including shared-outlet ownership.
- `fault`: confirmation, durable deduplication and optional monitor deadlines.
- `cli`, `naming`, `mcp`, `main`: command metadata, compatibility normalization,
  transport permissions, startup and household job supervision.

Controls and scheduling share the main thread. Idle MCP reads stdin directly;
background jobs enable one bounded reader thread. Release builds use stripping,
LTO and panic abort. Ordinary errors return through `Result`; a panic exits without
unwinding. OS locks release on exit, while persistence precedes controls/events.
Measure memory on the actual target; desktop samples are not Android guarantees.

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

Fault monitors retain their next deadline in their adapters, including TV-owned
plugs. The existing background interface drives reads and reconciles retained
alerts; household schedule deadlines stay in the application.

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

### Memory and allocation checks

All adapters share one process. No adapter adds a thread, async runtime, session cache
or task queue. With no background flags, MCP has no reader thread, input queue
or periodic wakeup. With background jobs, the reader has a 128 KiB stack and a
queue of one message. Both input modes use a 1 KiB reader buffer and cap messages
at 64 KiB. Adapter configuration and common state files are bounded, typically
to 8 KiB; unified configuration allows 64 KiB. Device groups allow at most eight
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

The probe reports requested allocation counts and bytes, not live heap or RSS.
Network reads, decoded statuses and MCP replies allocate bounded temporary storage.

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

Protocol tests cover independent Tuya/KLAP fixtures, bounded HTTP/IPP parsing,
stale sensor data, signature/identity rejection, uncertain-control outcomes and
durable fault recovery. Keep physical-device acceptance results in private
deployment records: verify reads, explicit controls, schedules and recovery on
the target before replacing another automation controller.

Android builds target `aarch64-linux-android` using a pinned Android NDK.
See [Android deployment](android.md#android-deployment) for target installation and checks.

The Meaco data-point mapping follows
[Tuya Local's device definition](https://github.com/make-all/tuya-local/blob/main/custom_components/tuya_local/devices/meaco_aretetwo_dehumidifier.yaml).
Printer encoding follows [RFC 8010](https://www.rfc-editor.org/rfc/rfc8010.html);
state/reason semantics follow [RFC 8011](https://www.rfc-editor.org/rfc/rfc8011.html).
The Tuya wire protocol follows the
[TinyTuya protocol documentation](https://github.com/jasonacox/tinytuya/blob/master/PROTOCOL.md).
Tuya 3.3's CRC detects corruption rather than authenticating the peer; use a
trusted local network. Device keys are never logged.

## Brand assets

The copper cable cottage represents local home device control. The README logo
is [`logo.png`](../logo.png); transparent logo, text-free icon, and wide social sources live in
`image-assets/sources/`. Sized exports, PNG/ICO favicons, a web manifest, and the
1200×630 social image are indexed in [the asset manifest](../image-assets/manifest.json). The outlined
artwork works on light and dark backgrounds. These are branding assets; the native HTTPS frontend serves the optional dashboard.
The asset web manifest is not installed automatically.
