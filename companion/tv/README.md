# Homewire TV

An Android TV companion for native TCL backlight reads and absolute controls.
It runs without root and exposes a small HTTPS API to Homewire on the LAN.
The first version supports only the audited Android 12 display-service ABI;
pairing pins the TV model, device, build fingerprint and incremental version.
Do not provision another firmware merely because it uses the TCL brand.

## Build

Requires JDK 17, Android platform 35 and build-tools 35.0.0 (`aapt2`, `d8`,
`zipalign`, `apksigner`). There are no downloaded application dependencies or
Gradle plugins. Set `JAVA_HOME`, `HOMEWIRE_ANDROID_PLATFORM` and
`HOMEWIRE_ANDROID_BUILD_TOOLS` to those installations. Run `sh test.sh` for the
host-side request parser tests, then build from this directory:

```sh
python3 build.py
```

Inject `HOMEWIRE_TV_SIGNING_KEY_B64` (base64 PKCS12) and
`HOMEWIRE_TV_SIGNING_PASSWORD` from a narrowly scoped Infisical signing folder.
`build.py` pipes the signing key through stdin and supplies a password environment
reference to `apksigner`; it creates no local credential files. Preserve this
signing identity for updates. `build.sh` accepts `HOMEWIRE_SIGNING_KEY` (a PKCS12
input path such as `/dev/stdin`) and `HOMEWIRE_SIGNING_PASSWORD_SOURCE` (an
`env:VARIABLE_NAME` reference).
The signed, signature-verified artifact is `build/homewire-tv.apk`.

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
merge it under `adapters.tcl.companion` in Homewire's protected config.
For later full-config checks or Android projection, inject this runtime folder
around the project's existing credential launcher so the TV token and other
adapters' credential references are both available. A root-folder injection alone
does not include this separately scoped token. Only the protected Android runtime
config receives its resolved value; the host config retains the reference.

Open **Homewire TV** to start or stop its foreground service. It requests boot
startup when enabled. Verify startup on the particular TV before
depending on it. A force-stop requires opening the app again. There is no wake
lock or periodic polling. Standby may suspend network access.

Keep versioned, hashed APKs and use the same signing identity with `-r` for
updates and rollback; do not uninstall or clear app data during updates.
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
