<p align="center">
  <img src="logo.png" alt="Homewire" width="280" /><br />
  <!-- repo-tagline:start -->
  <strong>🏠 Local home automation with verified device control 🔌</strong>
  <!-- repo-tagline:end -->
</p>

Homewire reads and controls supported devices on your local network. It runs
from one Rust executable and offers the same adapters through its CLI and MCP.

## Quickstart

Requires **Rust 1.89+**, Git, and network access to a supported device. This
example reads a Brother HL-L2350DW or HP DeskJet 3630 over IPP without credentials
or print jobs. Queries can wake a sleeping printer.

```sh
git clone https://github.com/tsilva/homewire.git
cd homewire
cargo build --release --locked
mkdir -p .local
chmod 700 .local
```

Create `.local/home.json`, replacing the documentation address with your printer's
LAN IP:

```json
{
  "version": 1,
  "adapters": {
    "printers": {"printers": [{"name": "printer", "host": "192.0.2.10"}]}
  }
}
```

Validate without contacting devices, then read live status:

```sh
chmod 600 .local/home.json
target/release/homewire --config .local/home.json config-check
target/release/homewire --config .local/home.json printer-status printer
```

Validation returns `{"device_io":false,"valid":true}`. Status returns JSON with
`printer`, `state`, `reasons` and `supplies`; an idle printer reports
`"state":"idle"`. Missing readings remain unknown. Failures go to stderr with a
nonzero exit code.

For other devices, copy only the sections you need from
[examples/home.json](examples/home.json) and follow the setup guides below.
Replace all placeholders, keep real settings in ignored `.local/` files, and use
[Infisical references](docs/configuration.md#environment-configuration-references)
for credentials. There is no automatic discovery.

For supported hardware, see [devices](docs/devices.md). For rooted Android,
see [battery guard setup](docs/android.md#battery-guard).

[Homewire operation and device setup](docs/homewire-guide.md).
