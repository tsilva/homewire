# Homewire TV

An Android TV companion for native TCL backlight reads and absolute controls.
It runs without root and exposes a small HTTPS API to Homewire on the LAN.
The first version supports only the audited Android 12 display-service ABI;
pairing pins the TV model, device, build fingerprint and incremental version.
Do not provision another firmware merely because it uses the TCL brand.

## One-command releases

After the one-time setup below, run from any directory:

```sh
python3 /path/to/homewire/companion/tv/release.py --version 0.1.5
```

From the repository root, the equivalent is:

```sh
python3 companion/tv/release.py --version 0.1.5
```

Choose an unused version greater than the installed version; `0.1.5` is an example.
The default private configuration is `.local/tv-release.json` under the repository
root, independent of the current working directory. Use `--config /private/path/tv-release.json`
for another saved configuration. Once that file and the toolchain are set up,
future releases need only the command above and an authenticated Infisical session.

The command runs the required Rust checks, Android deployment tests, companion
Java/Python tests and shellcheck, builds and verifies a signed APK, saves its
source and hashes, backs up the installed APK, installs once, and verifies the
installed version/hash, unchanged pairing pin, authenticated native status and
rejection of missing/incorrect credentials. It reads picture settings without
changing them. An existing, reachable, paired companion is required; first
provisioning remains the deliberate procedure below.

Versions are explicit `MAJOR.MINOR.PATCH`, with each component in 0–999. The APK
version code is `MAJOR * 1000000 + MINOR * 1000 + PATCH`, must be positive and must
exceed the installed code. The requested version overrides the manifest's base
version in the built APK; tracked sources are not rewritten during a release.
Each attempted version gets a fresh private directory and cannot be overwritten.
No command publishes, commits, pushes, uninstalls, clears data or retries installs.

### One-time setup

Install Python 3.10+, Rust/Cargo with rustfmt and clippy, shellcheck, zip, JDK 17,
the Infisical CLI, Android platform-tools (adb), platform 35 and build-tools
35.0.0. Android's SDK manager can install the latter:

```sh
sdkmanager 'platform-tools' 'platforms;android-35' 'build-tools;35.0.0'
```

There are no downloaded application dependencies or Gradle plugins. The builder
checks JDK 17, API 35, build-tools 35.0.0 and the required executables. Tool paths
can come from the private configuration or `JAVA_HOME`,
`HOMEWIRE_ANDROID_PLATFORM`, `HOMEWIRE_ANDROID_BUILD_TOOLS`; a standard
`ANDROID_HOME`/`ANDROID_SDK_ROOT` layout is also recognized.

Copy [release.example.json](release.example.json) to ignored
`.local/tv-release.json`, `chmod 600` it, and fill in local paths and existing
pairing/profile references. Paths are relative to this configuration file.
Keep the configuration owner-only, with no secret values; use real inventory
only in ignored `.local/` files. Authenticate the Infisical CLI once.

The signing folder must contain only `HOMEWIRE_TV_SIGNING_KEY_B64` (the existing
base64 PKCS12 signing identity) and `HOMEWIRE_TV_SIGNING_PASSWORD`; preserve this
identity permanently. The separate runtime folder supplies
`HOMEWIRE_SECRET_TV_API_TOKEN`. The command uses native Infisical injection with
an explicit project, environment, instance and non-root folder, disables imports
and expansion, and scopes each injection to the build or deployment child. It
pipes the signing key to apksigner and never writes credentials to disk or logs.
A credential/authentication failure stops the release; renew Infisical login
before starting another candidate.

Authorize ADB on the intended TV. `serial` selects it explicitly. The command
connects the configured TCP serial automatically; the TV must already authorize
this ADB host. Normal HTTPS verification connects to the audited profile's host/port.
An operator-managed tunnel can be specified only in the private release config
as `"verify_connect": ["127.0.0.1", 18766]`; this never changes Homewire's LAN config
or the certificate pin. Alternatively, add `"ssh_alias": "managed-phone"` to reuse an existing
FleetMan-managed SSH master. The command checks that master, temporarily forwards
the audited TV's ADB port 5555 and HTTPS port to local ephemeral ports, connects
ADB, and cancels only its own forwards on exit. It never starts another SSH
master or stops shared ADB/SSH services. This option cannot be combined with
`verify_connect`; restore the managed connection through FleetMan if unavailable.
The temporary route configuration contains metadata only and is removed on exit.

### Artifacts, verification and recovery

Each `.local/tv-releases/VERSION/` contains the exact signed `homewire-tv.apk`,
`source.tar.gz`, extracted source, a source/archive/APK hash and signer receipt,
a credential-free `checks.log`, and, after deployment, `installation.json` plus
the previous APK backup. The receipt records `built`, `verified`, or `failed`
and its failed gate. A dirty local source tree is allowed and recorded alongside its content hash and Git commit;
this is a local deployment workflow, not a publication certification.

Build without contacting the TV, then deploy those exact bytes:

```sh
python3 companion/tv/release.py --version 0.1.5 --build-only
python3 companion/tv/release.py --deploy-existing .local/tv-releases/0.1.5
```

Normal deployment verifies the autostart permission but does not prove boot
recovery. To deliberately reboot the TV once after installation and verify the
boot identity changes and the API returns without opening the app, append
`--verify-reboot` to either deployment command:

```sh
python3 companion/tv/release.py --version 0.1.5 --verify-reboot
```

This interrupts viewing. If ADB does not reconnect or boot/API verification
fails, the release remains failed; the command never reboots a second time.

A successful deployment prints `Deployed and verified: ...`; confirm that directory's
`receipt.json` has `phase` set to `"verified"` and `installation.json` records the acceptance
checks. With `--verify-reboot`, `reboot_verified` must also be `true`. A `built`
receipt proves only a signed build, and does not establish a successful deployment.

A connection failure before the installer starts leaves the candidate `built`,
with the failed connection gate recorded. Restore TV/network access and use
`--deploy-existing` for those exact bytes. This retries connection setup only.
Once the installer starts, an uncertain result blocks redeployment of that candidate.

A failed install or post-install check stops immediately, retains the APK,
source, old pairing config and backup, and never reports a verified release.
Inspect the failed gate and the device before taking another action. There is
no automatic rollback after an uncertain transport result.

To restore an earlier saved implementation, rebuild its archived source as a
**higher new version** and deploy it through the same checks:

```sh
python3 companion/tv/release.py --version 0.1.6 --rollback .local/tv-releases/0.1.5
```

This preserves pairing, TLS keys and replay state without relying on Android
release-APK downgrades. Restore the matching Homewire executable/config backup
if the integration also changed. Future private-state schema changes must remain
compatible with these recovery builds. APK backups alone are not a guaranteed
downgrade path. Never uninstall or clear app data to bypass a version/signature gate.

### Standalone build and CI

For a signed local build with credentials already injected:

```sh
python3 companion/tv/build.py --version 0.1.5 --output /private/fresh/homewire-tv.apk
```

For a credential-free compile/package check:

```sh
python3 companion/tv/build.py --unsigned --version 0.1.5 --output /private/fresh/homewire-tv-unsigned.apk
```

The output must not already exist. Every build uses fresh temporary compiler/DEX
outputs. CI compiles the unsigned APK and runs companion Java/Python tests,
Android deployment tests and shellcheck; it receives no signing/runtime credentials
and performs no installation. Production signing and device verification are local.

## Install and pair

Enable and authorize ADB for installation. Prepare a mode-600 private JSON
profile with exactly `host` (literal IP), `port` (8766), `model`, `device`,
`fingerprint`, `incremental`, and `framework_sha256`. The last field is the
audited SHA-256 of `/system_ext/framework/com.tcl.tv.display.jar`.
Keep actual inventory and firmware evidence in ignored `.local/` files.

From the repository root, inject only `HOMEWIRE_SECRET_TV_API_TOKEN` (a random
32-byte lowercase hexadecimal token) from the runtime credential folder:

```sh
python3 companion/tv/install.py --serial ADB_ENDPOINT \
  --apk .local/homewire-tv-v1-HASH.apk --sha256 APK_SHA256 \
  --profile .local/tv-profile.json --output .local/tv-companion.json \
  --backup-directory .local/tv-apk-backups
```

The helper checks live firmware and vendor-framework hash, backs up any existing
APK, installs with `-r`, verifies the installed hash, and opens the app before
pairing to accommodate TCL's provider startup restriction. It enables and
verifies `APP_AUTO_START` for this package on the audited TCL firmware. It sends the token
through ADB stdin to an operator-only provider, never through command arguments.
The provider requires shell UID 2000 and DUMP permission, accepts initial
provisioning once, and never returns the token. TLS keys remain in AndroidKeyStore.
The output contains a credential reference, certificate pin and firmware identity;
merge it under `adapters.tcl.companion` in Homewire's protected config. The helper
also verifies pinned HTTPS authentication and native status before saving this output.
For later full-config checks or Android projection, inject this runtime folder
around the project's existing credential launcher so the TV token and other
adapters' credential references are both available. A root-folder injection alone
does not include this separately scoped token. Only the protected Android runtime
config receives its resolved value; the host config retains the reference.

Open **Homewire TV** to start or stop its foreground service. It requests boot
startup when enabled. Verify startup on the particular TV before
depending on it. A force-stop requires opening the app again. There is no wake
lock or periodic polling. Standby may suspend network access.

Use the release command for future updates and source-based recovery with the
same signing identity; do not uninstall or clear app data during updates.
That would erase pairing, TLS keys and the replay journal. Restore the matching
Homewire executable/config backup when rolling back an integration. Token
rotation/re-pairing is not exposed by this first API; deliberate reprovisioning
requires stopping clients and clearing app data, then pairing and pinning anew.

## API and limits

TLS 1.2 on the configured LAN port; clients must verify the exact paired
certificate SHA-256 **before** sending `Authorization: Bearer TOKEN`.
Unauthenticated requests receive 401. No cleartext listener exists.

- `GET /v1/status`: protocol version 1, manufacturer, model, device, fingerprint,
  incremental build and current native `backlight_level` (0–100).
- `POST /v1/backlight`: JSON `{"backlight_level":50,"request_id":"32_LOWERCASE_HEX_CHARACTERS"}`.
  Send fields in that order. Returns `request_id`, `applied`, `verified`, and
  fresh `status`. The setting applies to the current picture mode and is saved.

Native reads use transaction 101; writes use transaction 100 with CURRENT (0)
and EXEC_SAVE (2). Every operation verifies paired firmware and the Binder
descriptor. No other vendor transactions or shell command execution are exposed.
Status reads do not change display settings.

The server uses one blocking worker and one active client. Request headers are
bounded to 4096 bytes, bodies to 256, responses to 4096, and sockets to a
3-second read timeout. Malformed framing, duplicate headers, chunked input,
invalid ranges and unknown routes fail explicitly. Slow clients can temporarily
delay access, so expose the port only on a trusted LAN.

The server durably records each of the last 16 request IDs before any native
write; duplicates return 409 even after restart. An uncertain pending control
blocks conflicting writes for 120 seconds unless a fresh matching reading
resolves it. This is bounded replay protection, not permanent deduplication:
always generate a fresh ID for each deliberate operation and never automatically
retry a failed control. Homewire sends a single POST, verifies the reported
identity and level, and skips matching values after a fresh GET.

Run repository Rust checks and Android deployment tests alongside `test.sh`.
Run `PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s companion/tv/tests -p 'test_*.py'`
from the repository root for installation validation tests.
Hardware validation must check pinned authentication, status, a deliberate
absolute control with restoration, and state preservation after a signed update.
Foreground service and boot behavior require checks on the actual firmware.
