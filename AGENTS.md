# Homewire

Homewire is a small native Rust process with modular device adapters, deterministic
automation sequences, and an MCP stdio interface. The first adapter is the Meaco
Arete Two dehumidifier using Tuya LAN protocol 3.3.

- Work on the current branch and preserve local changes.
- Keep credentials in ignored private files; never log keys or device configuration.
- Keep messages, queues, caches, and retained state bounded. Measure resident memory
  on the target device before claiming a memory footprint.
- Keep device protocol code separate from Meaco behavior, scheduling, and MCP.
- Read status and verify outcomes. Never blindly retry sleep: it is a toggle.
- Invalidate persisted mode assumptions before controls. Serialize controls for a
  device, including across Homewire processes using the same configuration path.
- Do not enable a replacement schedule while the corresponding Home Assistant
  automation still owns the device. Device-changing tests must be deliberate.
- Update README.md when commands, configuration, or behavior change.
- Commit Cargo.lock. Avoid unnecessary dependencies and interpreter subprocesses.
- Required checks: `cargo fmt --check`, `cargo test --locked`, and
  `cargo clippy --locked --all-targets -- -D warnings`.
