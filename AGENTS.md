# Homewire

Homewire is a small native Rust process with modular device adapters, deterministic
automation sequences, and an MCP stdio interface. The first adapter is the Meaco
Arete Two dehumidifier using Tuya LAN protocol 3.3. The printer adapter uses
read-only IPP for Brother HL-L2350DW and HP DeskJet 3630 status. The Awair Element
adapter reads fresh measurements from its local HTTP API.
The shared Tapo L430C/L900 adapter uses KLAP v2 for verified absolute light
controls and reported overheating faults. The plug adapter reuses KLAP v2 for
Tapo P100/P110 relays, P110 metering and reported overload monitoring.

- Work on the current branch and preserve local changes.
- Keep credentials in ignored private files; never log keys or device configuration.
- Keep messages, queues, caches, and retained state bounded. Measure resident memory
  on the target device before claiming a memory footprint.
- Keep device protocol code separate from Meaco behavior, scheduling, and MCP.
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
- Update README.md when commands, configuration, or behavior change.
- Commit Cargo.lock. Avoid unnecessary dependencies and interpreter subprocesses.
- Required checks: `cargo fmt --check`, `cargo test --locked`, and
  `cargo clippy --locked --all-targets -- -D warnings`.
