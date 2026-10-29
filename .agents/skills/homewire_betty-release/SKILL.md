---
name: homewire_betty-release
description: Build, verify and update the native Betty Android assistant APK while preserving its Telegram session and assistant shortcut. Use for Betty phone releases; TCL companion and daemon deployments use their own skills.
---

# Betty Android releases

Use the [Betty guide](../../../companion/betty/README.md), its maintained
`companion/betty/build.py`, and this skill's [artifact helper](scripts/artifact.py).
Do not use the TCL release launcher or TV version selector for Betty. The shared
private toolchain/signing binding format does not select a deployment target.

## Scope and bindings

A build-only request authorizes local artifacts. A build-and-update request also
authorizes the normal verified USB update. Creating/loading this skill does not
authorize a build, installation, app launch, reboot, permission change or message.
The install helper is update-only; handle first installation as a separate request.

Use the current checkout and its instructions. Keep actual device bindings,
artifacts and installation evidence in ignored, owner-only `.local/` storage.
The existing build binding is `.local/tv-release.json`; it supplies toolchain
paths and scoped Infisical signing references. Preserve the signing identity and
use the build script's native injection. Provider login failure is a stop, not
permission to use a different key or plaintext credential cache.

The protected `.local/betty-device.json` pins the intended USB target and the
certificate from a previously verified installed Betty APK:

```json
{
  "schema": 1,
  "serial": "USB_DEVICE_SERIAL",
  "signer_sha256": "VERIFIED_CERTIFICATE_SHA256"
}
```

These are placeholders, not usable bindings. Preserve an existing binding. If
one is absent, establish it from authorized device selection and verified earlier
installation evidence; do not trust the candidate APK as the source of its own
expected signer. Both private JSON files must be regular, owner-only files.
The helper accepts explicit alternate binding paths.

## Prepare and build

1. Resolve the desired version. For deployment, read the bound installed version
   under the user's authorized device scope; do not guess after a failed query.
   Build-only work may use local version/receipt information without contacting
   a phone. Choose an unused artifact directory and a higher installation code.
2. Keep version name/code consistent in `companion/betty/AndroidManifest.xml`,
   the receipt in `companion/betty/build.py`, and the application version in
   `companion/betty/src/eu/tsilva/homewire/betty/Telegram.java`.
3. Finish source changes and required repository checks before the release build.
   Include Betty's contract tests. For capture/UI changes use the separate
   no-network preview and production instrumentation on a **disposable emulator**.
   Never run that instrumentation on a personal installation: it writes synthetic
   protected state. Recheck cold-cache destination resolution when changing login
   or startup. Keep chat/user identity pins and confirmed-send handoff intact.
4. Build serially from stable sources. Do not edit inputs during compilation or
   source archiving. The generated source archive records the completed snapshot;
   it cannot by itself prove that inputs never changed during compilation.

From the repository root, substitute the selected version for `VERSION`:

```sh
python3 companion/betty/build.py --signing-config .local/tv-release.json \
  --output .local/betty-artifacts/VERSION/final/Betty-VERSION.apk
python3 .agents/skills/homewire_betty-release/scripts/artifact.py verify \
  .local/betty-artifacts/VERSION/final/Betty-VERSION.apk \
  --source-root companion/betty
```

`verify` contacts no phone and injects no credentials. It checks the APK and source
receipt hashes, actual package/version, pinned signer, source manifest and 16 KiB
alignment; with `--source-root`, every archived source byte and file is compared
with the current source. It refuses unsigned, preview and instrumentation APKs.
If sources changed during the build, reconcile them and make a fresh candidate;
never overwrite a retained artifact or describe an uncertain build as verified.
When changing this helper, run its offline regression tests with
`PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s .agents/skills/homewire_betty-release/tests -p 'test_*.py'`.

## Authorized update

Connect and unlock the bound phone. Use one targeted USB session; do not choose
another connected device, restart the shared ADB server or enable wireless
debugging to recover a missing USB connection. The optional wait below lasts at
most 45 seconds and happens before any installation begins.

```sh
python3 .agents/skills/homewire_betty-release/scripts/artifact.py install \
  .local/betty-artifacts/VERSION/final/Betty-VERSION.apk --wait-seconds 45
```

The helper verifies the artifact, requires a newer version, snapshots the current
assistant/power-button/USB-debugging settings, and backs up the installed APK with
its signer and version checked. It reserves `installation/` beside the candidate,
records the attempt before invoking `adb install -r` once, then pulls the installed
APK back and verifies its exact hash, version and unchanged settings. Files in
that evidence directory are owner-only. Run only one Betty update at a time.

Default installation does not open the app. Add `--launch` only when opening Betty
is within the user's request: successful setup can immediately start recording.
Never uninstall, clear app data, log out, replace Telegram API credentials,
change assistant/debugging settings, or bypass Android's secure lock screen to
make an update work. Keep login codes/passwords on the phone and out of QA output.

If a device is absent before an attempt starts, report that the APK is ready but
not installed. Once `installation/` exists, inspect its private receipt before
recovery. A timeout or uncertain install is never replayed automatically; first
use targeted read-only package/hash checks to establish what actually happened.
Do not delete evidence or choose a different version merely to bypass a failure.
For an authorized rollback, rebuild a reviewed older source snapshot with a new,
higher code and verify it normally; do not downgrade with `adb -d` or erase data.

## Report and acceptance

Require `installation/installation.json` phase `verified` and an installed hash
match before reporting success. Separate built/verified APKs from installed
updates. Report the version and artifact/receipt paths, and whether the app was
opened. Cold reopen, live recognition, the physical power shortcut and a real
Telegram handoff are user acceptance checks, not proven by signing or installation.

Do not send a live test message without showing the complete draft and intended
recipient in this chat and obtaining approval for that exact message. The app's
normal user-operated review/Send flow remains the default acceptance path.
