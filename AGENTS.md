# Homewire

Homewire is a small native Rust process with modular device adapters, deterministic
automation sequences, and an MCP stdio interface. The first adapter is the Meaco
Arete Two dehumidifier using Tuya LAN protocol 3.3. The printer adapter uses
read-only IPP for Brother HL-L2350DW and HP DeskJet 3630 status. The Awair Element
adapter reads fresh measurements from its local HTTP API.
The shared Tapo L430C/L900 adapter uses KLAP v2 for verified absolute light
controls and reported overheating faults. The plug adapter reuses KLAP v2 for
Tapo P100/P110 relays, P110 metering and reported overload monitoring. Samsung
UE40H6200 uses an existing encrypted pairing; DIGI R2A uses pinned mutual TLS
and absolute wake/sleep. The TV workflow composes those adapters and a shared plug.

- Work on the current branch and preserve local changes.
- Keep credentials in Infisical and references in ignored private configs. Necessary device runtime files must remain protected; never log keys or device configuration.
- Keep tracked examples and documentation generic: use synthetic device names,
  documentation-only addresses and credential placeholders. Store household
  inventories, personal locations, device serials and deployment/migration logs
  in ignored `.local/` files, never in public examples or reference documentation.
- This repository is public. Before every commit, inspect the complete staged
  diff, filenames and proposed commit message for private household details.
  Review code, tests, fixtures, screenshots and documentation equally: never
  commit real device IDs/serials, MAC or LAN addresses, personal hostnames,
  precise locations, room/device inventories, actual household schedules,
  credentials, pairing data or deployment/cutover records. Keep those in ignored
  `.local/` files; use documentation addresses and synthetic fixtures in Git.
  Report findings by file/line and category without printing private values.
- Install the privacy hooks with `python3 scripts/privacy_guard.py install`.
  Before committing, run `python3 scripts/privacy_guard.py staged` and complete
  the manual privacy review above. Before pushing, run
  `python3 scripts/privacy_guard.py check HEAD`. The pre-push hook checks every
  outgoing ref against retired commit/tree/blob hashes, including full ancestry.
  Never bypass these checks, weaken their denylist, or restore the retired
  history for troubleshooting/tests. Test the guard with synthetic repositories.
  Hash checks cannot recognize newly written private details: sanitize those
  before staging, even when the automated checks pass.
- Keep messages, queues, caches, and retained state bounded. Measure resident memory
  on the target device before claiming a memory footprint.
- Keep device protocol code separate from Meaco behavior, scheduling, and MCP.
- New device families implement `device::Adapter`, declare borrowed capability
  entities/static `Support`, own protocol validation, and register concrete types
  in `src/device.rs`. Reuse standard category tools; add no family routing to MCP.
- Optional adapter background work declares its own next deadline through
  `Adapter::next_background`/`background_tick`; avoid central family polling
  branches, additional threads or retained JSON snapshots.
- Compose household behavior in `routine`; keep device protocols inside adapters.
  Batch entity reads once per physical device and release snapshots afterwards.
  Retain no entity-state/schema cache or allocated runtime registry.
- TV owns child power controls in standard and legacy interfaces. Require explicit
  --maintenance for direct media/TV-outlet power; preserve device guards.
- Use `daemon` for persistent device ownership and `bridge` for stateless MCP
  clients. Keep IPC frames/queues bounded, socket access private, and never replay
  controls after transport errors. Share Android-aware OS locking with configs.
- Keep Android installs versioned, hash verified and reversible; preserve state.
  Replacement schedules remain opt-in and require a deliberate HA cutover.
- Wait for the next enabled job deadline instead of periodic per-second wakeups.
  Idle MCP and pre-initialization wait directly for input.
- Use `device::Monitored` for supported fault monitoring and shared `fault` logic.
  Keep optional capabilities separate; use static dispatch and bounded storage.
- Read status and verify outcomes. Never blindly retry sleep: it is a toggle.
- Printer queries must use Get-Printer-Attributes only. Do not submit print jobs
  or infer faults from ink percentages. Unknown readings never prove recovery.
- Confirm printer faults/recovery before emitting events; persist deduplication
  state and keep protocol stdout free of diagnostics.
- Awair queries must use the read-only local air-data endpoint. Preserve units
  and actual sensor values; missing, invalid or stale data must fail explicitly.
- Tapo light controls must verify the configured device identity and model. Use
  absolute settings, gate MCP writes with --allow-control, and never retry an
  uncertain command. Unknown overheating readings cannot prove recovery.
- Validate temperatures against the model and its live range. L900 fixed
  brightness/colour controls require effects and music mode to be confirmed off.
- Plug power controls must be absolute, identity checked and verified. P110
  active/unknown overload status blocks changing power to on. Never infer
  overloads or safe recovery from consumption; preserve device protection and
  auto-off configuration. Energy query failures must not block relay power-off.
- Invalidate persisted mode assumptions before controls. Serialize controls for a
  device, including across Homewire processes using the same configuration path.
- Keep light schedules opt-in, timezone aware, and persist attempts before controls.
- Do not enable a replacement schedule while the corresponding Home Assistant
  automation still owns the device. Device-changing tests must be deliberate.
- Media controls must verify live identity. Pin the DIGI server certificate and
  verify TLS signatures; never silently accept replacement device credentials.
- Send Samsung shutdown once and persist attempts before sending. Unknown or
  unreachable state is not confirmed off. Keep fixed-delay outlet cutoff explicit
  in TV workflow configuration; abort cutoff on earlier device/control errors.
- Update README.md when commands, configuration, or behavior change.
- Commit Cargo.lock. Avoid unnecessary dependencies and interpreter subprocesses.
- Required checks: `cargo fmt --check`, `cargo test --locked`, and
  `cargo clippy --locked --all-targets -- -D warnings`.
- Privacy guard changes also run
  `PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s scripts/tests -p 'test_*.py'`.

- Android deployment changes also run
  `PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s scripts/android -p 'test_*.py'`
  and `shellcheck scripts/android/build.sh scripts/android/sandbox/build.sh scripts/android/sandbox/build-identity.sh`.
  Sandbox changes also build the native guard and run its negative probe on the target.
  Never restore permissive agent grants on failure or enable disabled channels.
  Never copy test tunnel configs to phones.

## TCL companion releases

For TCL companion builds, deployments, verification or recovery, use
`$homewire_build-release`.
It uses `companion/tv/release.py`, the protected `.local/tv-release.json` bindings,
and the [release guide](companion/tv/README.md#one-command-releases).
An explicit build-and-deploy request authorizes the normal update checks;
reboot verification remains opt-in for the current run.
