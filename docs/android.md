# Android deployment

[Reference index](reference.md) · [First use](../README.md)

## Android deployment

### Private installation profile

Set `deployment` in `.local/home.json` using the generic
`examples/deployment.json` schema, and pass `--config .local/home.json`
before the command to Android helpers. It is required for isolation and
assistant setup, and recommended for recovery operations. Legacy
`--deployment .local/deployment.json` remains supported. Basic owner-only
installation without a profile retains the primary-user Termux layout.

- `android.package` and `android.user` select the supported debuggable Android
  package/profile. CE/DE, service and sandbox paths are derived consistently.
  Optional `root_command` and `zeroclaw_config` must be normalized absolute paths.
  Without `root_command`, the host discovers `su` through ADB; no device-specific
  root executable is compiled into the launcher.
- `agent.name` selects an existing agent; it is never silently created.
  `gateway_port`, `mcp_port`, and optional `transcription_port` are distinct,
  nonzero TCP ports. The latter opens only that configured loopback exception.
  `provider_host` selects the DNS probe target. Optional `tools` narrows the
  supported Homewire capability allowlist; shell, filesystem and device/security
  configuration are never selectable. Optional `telegram_control_tools` grants
  a unique subset of the selected write tools to existing authorized Telegram
  peers; it defaults to an empty list (read-only). For example,
  `"telegram_control_tools": ["switch_set"]` permits smart-plug controls,
  including printer outlets. The grant applies to all configured Telegram
  channels without enabling disabled channels or changing peer IDs.
- `network.dns_servers` is an explicit 1–4 IPv4 server list. `lan_probe` is the
  operator's blocked IPv4:PORT test endpoint; `admin_port` must match the API
  listener. IPv6 remains denied by the sandbox. Private/special network denials
  remain compiled security policy; the profile does not allow arbitrary LAN rules.

The host reads this profile (maximum 8192 bytes) to generate consistent paths,
launchers, firewall rules, MCP URL and probe arguments. A root-only policy snapshot
is retained on the phone. Keep profiles private; credentials stay in adapter
configs. Homewire discards host-only deployment data after startup.

Install and update validate the prepared policy through the pinned ZeroClaw CLI
before changing the active policy; an update fails before stopping the agent if
validation changes its declared scope.

An isolation update requires the profile to match the operator-managed API and
assistant endpoints before stopping anything. To change ports or agent identity,
coordinate those private configs first; mismatches fail instead of silently
switching or restoring permissive grants. Changing DNS/provider/tool subsets
uses the normal sandbox update. Alias changes require the selected agent to
already exist in the active policy, and its workspace/mount is prepared by update.

Before upgrading older schemas, explicitly configure Meaco presets/schedule,
light curves/outside targets and TV timings as needed. Back up and replace private
files atomically, preserve saved flags and HA ownership, then restart the owner.
Isolation policy changes require the operator helper to remount policy.

Host requirements: ADB, Python 3.11+, rustup with Rust 1.95.0 and
`aarch64-linux-android` installed, and Android NDK **30.0.16248370**. Install the
Rust target with `rustup target add --toolchain 1.95.0 aarch64-linux-android`.
Set `HOMEWIRE_NDK` to the extracted NDK directory. Build scripts accept macOS
and Linux hosts and use Android API 28; the output is an Android/Bionic executable.

```sh
scripts/android/build.sh
python3 scripts/android/phone.py --config .local/home.json --serial "$HOMEWIRE_ADB_SERIAL" install
python3 scripts/android/phone.py --config .local/home.json --serial "$HOMEWIRE_ADB_SERIAL" connect-zeroclaw --agent "$HOMEWIRE_AGENT"
python3 scripts/android/phone.py --config .local/home.json --serial "$HOMEWIRE_ADB_SERIAL" verify
# Optional strict availability check (offline devices make this fail):
python3 scripts/android/phone.py --config .local/home.json --serial "$HOMEWIRE_ADB_SERIAL" verify --require-online
# Update the executable and enable MCP controls, preserving configs/state:
python3 scripts/android/phone.py --config .local/home.json --serial "$HOMEWIRE_ADB_SERIAL" install --allow-control
python3 scripts/android/phone.py --config .local/home.json --serial "$HOMEWIRE_ADB_SERIAL" rollback
```

Set `HOMEWIRE_ADB_SERIAL` to the target shown by `adb devices` and choose an existing
agent alias. Unlock Android before installation or verification after a reboot.
The host Python helper requires Termux to permit `run-as com.termux` and already have `termux-services` with a
running supervisor. It fails early without that access; arbitrary release Termux
installations may require a separate SSH/root installation route. Homewire itself
runs unprivileged as Termux's user. Root is not a runtime requirement.

Build output is `target/android-package/homewire` with `manifest.json`, recording
the binary SHA-256, source commit/dirty flag, Rust source/manifest tree hash, target,
API level and tool versions.
Installer compares host and phone hashes and checks the executable before
switching the active version. Executables live in `~/homewire/releases/HASH/`;
`current` and `previous` links select versions. Configs, state and locks stay in
`~/homewire/config/`, independently of executable updates.

Migration/transfers use `scripts/android/catalog.json`, checked against the Rust
catalog. Migrate optional `computer-routine.json` for computer sequencing.

`--config` imports one runtime `home.json` plus known certificate/key files;
the host-only deployment block is omitted. Legacy `--configs` imports known separate files;
it does not copy test configs, lock files, saved state or validation recordings.
Test/tunnel `connect` overrides, sample addresses and placeholders are rejected.
Existing phone configs are archived privately before explicit imports. An installed
API agent endpoint is retained when the imported API omits that generated endpoint;
its token and exact tool scope remain unchanged. An explicit endpoint overrides it,
and removal of the API service removes the frontend. Certificate
and TV child paths should be relative to the config directory. Subsequent installs
use a legacy deployment-only profile and omit `--configs` to retain target configs.
With unified `--config`, installation also updates runtime settings.

The installer writes an enabled `termux-services` service at
`$PREFIX/var/service/homewire`. It waits for shutdown, removes the stale socket,
starts the selected version and checks discovery/read replies. Unavailable devices
are reported and do not invalidate a working executable; `--require-online` makes
standalone verification strict. Each successful install marks its executable as
validated. Failed upgrades restore a previously validated executable when one
exists. Rollback restores only the executable; it never rewinds device state,
configs or attempt guards. Schema compatibility still matters when rolling back
future versions. A first install has no earlier validated version to restore.
Retained releases/backups can grow across installs; clean older versions deliberately.
Logs rotate at 64 KiB with three archived files in `~/homewire/log/`.

Each install preserves existing `service.args`. A new installation starts
read-only; `--allow-control` or `--no-allow-control` explicitly changes that grant.
Schedules, maintenance and monitoring start off on a new installation.
To enable them later, add one supported flag per line and restart the service;
first disable the corresponding HA rules and validate the desired controls.
Installed configs alone do not enable automations. Installer verification sends
no device controls. Socket is `~/homewire/run/homewire.sock`.

ZeroClaw integration defines server `homewire`, a matching MCP bundle, and adds
that bundle only to the selected existing agent. It preserves other config values,
compares the original config before writing, keeps a `config.toml.before-homewire`
backup and restarts ZeroClaw to pick up the grant. Repeating the command is a no-op
when already configured; differing existing Homewire definitions fail for review.
The server launches `bridge` with absolute paths and a 300-second tool timeout.
Tools appear as `homewire__entity_list`, etc.

To grant visual TV access, explicitly include `media_screen_status`,
`media_screenshot` and `media_navigate` in `deployment.agent.tools`; navigation
on Telegram also requires `media_navigate` in `telegram_control_tools`. The
policy update preserves channel enablement, peers and existing grants. With
screenshots granted, it sets a 1 MiB MCP response cap and selects API
`agent.image_output: "image_marker"` for the pinned ZeroClaw vision loader.
Other MCP clients receive native image blocks by default. The TV's separate
accessibility service must be enabled explicitly.

Termux:Boot can start the existing service supervisor from `~/.termux/boot/`.
Open the Boot app once after installation; its script can acquire a wake lock and
start `service-daemon`. The installer preserves existing boot configuration;
configure it explicitly on a new target. Android battery/background restrictions
and credential-unlock state
can affect boot/network access; screen-off, reconnect and reboot checks must be
made on the actual target. See [Termux:Boot](https://github.com/termux/termux-boot)
and [NDK tooling](https://developer.android.com/ndk/guides/other_build_systems).

Validate each deployment independently: discovery, fresh reads, exclusive device
ownership, update/rollback, screen-off operation, Wi-Fi reconnection and reboot.
Observe actual scheduled runs and alert/recovery delivery before transferring
automation ownership. Keep those results and any migration decisions in private
installation records.

Model inference requires a separately configured provider. Store its credentials
privately and use a dedicated key with an appropriate spending cap. A secret
manager on the same rooted device cannot protect a credential while the agent
uses it; keeping provider credentials off the device requires an external proxy.
Use paginated discovery or targeted status queries for clients with output limits.

## Battery guard

For a rooted, continuously powered Android host, `scripts/android/battery_guard.py`
installs a separate native guard through an existing managed SSH connection.
It requires the kernel's writable
`/sys/class/power_supply/battery/input_suspend` switch (1 inhibits, 0 permits) and readable
`temp` (tenths of °C), `capacity`, `voltage_now` (µV), `present`, `health` and
`status` attributes. This is an additional restriction on the charger; it never
changes OEM thermal limits, current/voltage limits or battery protection settings.
Unsupported hardware must not use this installer.

The fixed conservative policy is:

| Condition | Action |
| --- | --- |
| Battery reaches 70% | Pause charging until 50% or below |
| Battery reaches 40°C | Pause charging until 37°C or below |
| Battery reaches 0°C or below | Pause charging until 5°C or above |
| Missing/invalid sensor data, absent battery, unknown/non-Good health | Pause charging; require three good samples before resuming |
| Battery reaches 50°C, voltage reaches 4.50 V, or a serious reported battery fault | Persist a lockout before disabling charging; manual inspection required |
| Failed charging-switch verification | Keep attempting to inhibit charging; report a control fault; never enable during that worker run |
| Battery below 15% | Queue a low-battery alert |

These are software policy thresholds, not certified safe operating limits for an
arbitrary cell. Startup is conservative: temperature must be 5–37°C and charge
must reach 50% or below before a fresh guard permits charging. Resuming requires
three fresh kernel samples at least 30 seconds apart. A charge ceiling alone does
not emit a fault. Serious health states include dead battery, overvoltage,
overcurrent, unspecified failure and charger watchdog/safety timer expiry.
An initially disabled switch is preserved as `external-hold`; the guard does not
automatically undo another operator's charging restriction.

The root supervisor starts from Magisk `service.d` before credential unlock,
restarts a failed worker and leaves charging disabled if stopped. A
`CLOCK_BOOTTIME_ALARM` deadline and `EPOLLWAKEUP` sample every 30 seconds through
deep suspend; alarm/wakeup capabilities are mandatory. The supervisor blocks
waiting for its child and needs no periodic housekeeping wakeups. There are no
Homewire, agent or Internet dependencies for the charging cutoff. Local state is
bounded under root-only `/data/adb/homewire-battery`; critical lockouts survive
worker restarts and reboots. Readback verifies the switch and later polls check
that a held battery no longer reports `Charging` and its input has stayed suspended.
On startup the guard first asserts and verifies the cutoff again, because a reboot
can reset the kernel switch while its durable ownership marker remains present.
Some Android charger managers immediately undo `battery_charging_enabled` writes;
this guard deliberately uses input suspend instead. Verify actual charging current
with the intended charger connected before relying on a new installation.

Create an ignored, mode-600 profile containing references, never token values:

```json
{
  "ssh": "android-host",
  "su": "/system/bin/su",
  "prefix": "/data/data/com.termux/files/usr"
}
```

The existing Termux `clang` compiles the native sources as the app UID. Root
installs a versioned copy, verifies hashes, and preserves previous boot wrappers
and durable fault state. Staging reads sensors but does not switch charging.
Installation deliberately exercises the real charging cutoff during startup.

```sh
python3 scripts/android/battery_guard.py --profile .local/battery-guard.json stage
python3 scripts/android/battery_guard.py --profile .local/battery-guard.json install
python3 scripts/android/battery_guard.py --profile .local/battery-guard.json status
# Restore the previous installed wrapper; keep protection/lockout state:
python3 scripts/android/battery_guard.py --profile .local/battery-guard.json rollback
```

`stop` stops the verified supervisor and leaves charging inhibited; its boot
wrapper remains installed. Input suspend blocks the phone's use of USB input;
the charger and cable remain electrically connected. No automatic command clears
a critical lockout. After physical inspection
and repair, an operator can stop the guard, review/remove its root-only `latched`
marker, and restart the installed wrapper. Retain any intentional `external-hold`
unless its original owner authorizes release. Never restore charging solely to
silence an alert.

Optional Telegram delivery reuses one explicitly enabled bot from an existing
encrypted ZeroClaw configuration. Add `telegram_config` (absolute path),
`telegram_section` (for example `[channels.telegram.observer]`) and `chat_id`
(one explicit numeric recipient) together to the private profile before staging.
Credentials are decrypted in memory and passed to HTTPS curl through an anonymous
pipe. Tokens are never placed in arguments, logs or new credential files.
Disabled bot channels stay disabled. Each of the six exact message files
(`paused.txt`, `critical.txt`, `control.txt`, `recovery.txt`, `low.txt`, `test.txt`)
must be approved with its recipient before enabling delivery:

```sh
python3 scripts/android/battery_guard.py --profile .local/battery-guard.json enable-alerts --approved-messages .local/approved-battery-messages
python3 scripts/android/battery_guard.py --profile .local/battery-guard.json test-alert
```

Delivery starts disabled for each release. Approval is bound to that release's
recipient and exact message files; staging a different recipient cannot redirect
an active release's alerts. Persistent event/delivery records reconcile after
restarts, failed sends remain pending, attempts are at most once per minute and
active warnings repeat at most every 30 minutes per worker run. Telegram timeouts
can make delivery ambiguous, so duplicates remain possible. Recovery is sent only
after a previously delivered warning clears. Delivery is best effort and requires
network access and unlocked credential storage; airplane/deep-offline modes can
delay it. Use an independent powered host/monitor for loss-of-phone alerts, and
an external power cutoff for protection against a hung phone or charger failure.

Software cannot detect swelling, cable/connector damage or every internal cell
defect, and cannot guarantee fire prevention. Inspect the battery, use a sound
compatible charger/cable and a ventilated nonflammable resting place. Do not keep
using a swollen/damaged battery. Keep normal smoke detection in place. See
[Motorola charging safety](https://help.motorola.com/hc/apps/settings/legal/global/en-gb/T1507072757.html)
and [NIST's overheating guidance](https://pages.nist.gov/mobile-threat-catalogue/physical-threats/PHY-7.html).

## ZeroClaw isolation

Rooted ARM64 Android deployments can use an external native sandbox with ZeroClaw **0.8.5**. It is
separate from ZeroClaw's optional built-in sandbox: `security status` still reports
that built-in sandbox as inactive. The native launcher enforces the boundary.

- A signed, permission-only `eu.tsilva.homewire.agent` APK assigns a dedicated
  Android UID and INTERNET permission. It has no code, activity, service or app
  process. A package-specific battery exemption allows the native background UID
  to use Android DNS. Do not uninstall it while the sandbox is installed.
- A private mount namespace and `chroot` expose read-only native programs, Android
  libraries/properties and two networking helper sockets. `/proc` is deliberately empty, supporting
  kernels without PID namespaces. Homewire, Termux, device credentials, shared
  phone storage, root helpers and host process descriptors are absent.
- The launcher drops all capabilities, supplementary groups except Android's
  internet group, and inherited non-standard descriptors; sets `no_new_privs`;
  disables core dumps and limits open descriptors to 128. Static executables and
  unaudited ZeroClaw versions are refused at installation. An obligatory preload
  guard installs seccomp before worker threads start. It denies program execution,
  separate processes, ptrace/process-memory access, namespace/mount changes,
  privilege changes and kernel-management calls. Ordinary worker threads remain
  available. Setup failure stops the agent; there is no permissive fallback.
- Android's libc `realpath()` normally resolves paths through `/proc/self/fd`.
  The same preload library provides a fallback only for `/bin/web` dashboard
  paths when procfs is absent and libc returns `ENOENT`. It walks directory
  descriptors without following symlinks, accepts only root-owned regular files
  and directories on read-only mounts, and rejects traversal and path aliases.
  All other paths retain libc behavior. This keeps host procfs and descriptors
  hidden while allowing ZeroClaw's existing static-file containment checks.
  Dashboard assets must be installed under the jail's `root/bin/web`; the daemon
  detects them at startup. `dashboard-probe` checks positive paths and operator
  symlink fixtures alongside the existing negative sandbox probe.
- Only agent-owned state/temp files are writable. Configuration, its encryption
  key and `TOOLS.md` are read-only bind mounts. The parent state directory cannot
  be renamed to substitute a different configuration. Homewire's private Unix IPC
  and its settings stay outside the jail.
- The UID firewall permits the scoped MCP endpoint, local transcription service,
  the home resolver and public IPv4 HTTPS. Direct LAN/admin connections and IPv6
  are blocked. Established replies allow the loopback gateway to answer trusted
  clients. DNS uses Android's broker. Public HTTPS is not a destination allowlist.

The model receives only Homewire MCP tools; shell, filesystem, browser, arbitrary
HTTP, Google/photo/camera/microphone bundles, delegation and agent cron are removed.
The `homewire` risk profile is `supervised`: ZeroClaw 0.8.5 bypasses channel exclusions
under full autonomy ([pinned implementation](https://github.com/zeroclaw-labs/zeroclaw/blob/cb2b20a9fe59004b7ef03e123942d31f29236d15/crates/zeroclaw-channels/src/orchestrator/mod.rs#L6555)). Telegram excludes Homewire mutations unless the operator explicitly grants them
in `deployment.agent.telegram_control_tools`; its peers have
no agent-administration grant. Existing peer IDs, channel enablement, paired
clients and encrypted model keys are preserved. Installation does not enable
Telegram or send messages; validate delivery separately when it is enabled.

The assistant gateway exposes only tools selected by the operator, omits absent
adapters, and never grants `entity_configure`. Telegram reads include discovery,
status/options, routine previews, alerts/history, weather, shopping, radio/TTS
and wake discovery. Writes require explicit grants in both agent scope and
`telegram_control_tools`:

| Control group | Tools |
| --- | --- |
| Devices/routines | `dehumidifier_set`, `light_set`, `switch_set`, `media_power`, `routine_run`, `wake_on_lan` |
| Remote input | `remote_key`, `media_launch`, `media_text`, `media_activity`, `remote_hold`, `media_voice` |
| Playback/list | `media_play`, `media_command`, `shopping_update` |

Use only tools supported by the pinned policy. Whole-tool grants cover all
eligible configured entities; Homewire identity checks, ownership and attempt
guards still apply. Media launch/text/activity/holds/voice have separate grants;
see [Android TV controls](devices.md#android-tv-activities) for limits. Voice uses
supplied audio and grants no microphone access. Add tools to API/agent scopes
and, if desired, Telegram controls through an operator update. Disabled channels
and unrelated denials remain unchanged.

Generated instructions route TV power through `tv_on`/`tv_off` when discovery
reports routine ownership; direct media power applies to unowned devices.
TCL outlet sequencing uses the unified `routines.tv` configuration.
Generated instructions keep short follow-ups on the user's selected device,
avoid unrelated tasks/discovery and report structured verification results.
They request screen confirmation for unverified shutdown, attribute observations
to the user and retain the backend's separate status; the latest correction
replaces earlier reports. Guard expiry never initiates a retry. A new attempt
requires an explicit request and fresh state accepted by the adapter. Packet,
key or app delivery does not prove screen power or playback. TCL integrated wake
recovery remains within one `media_power` operation; agents must not duplicate it
after timeout. Expired verification means an incomplete request, without invented
recovery times or promises of unscheduled checks. These instructions add no TV
polling, screen capture, ADB or wake lock.

A compromised allowed Telegram identity could use the explicitly granted controls, read household state
and use the capped model account. This is a reduction in privileges, not protection
against a compromised root user or kernel. The provider key necessarily exists in
process memory; retain a dedicated provider-side spending cap to bound cost.

### Trusted Homewire MCP endpoint

The optional `agent` object in the private API config enables a second listener:

```json
"agent": {
  "listen": "127.0.0.1:42620",
  "token": "REPLACE_WITH_AN_INDEPENDENT_64_HEX_TOKEN",
  "tools": ["entity_list", "entity_status", "shopping_list"]
}
```

Use a token distinct from dashboard and companion tokens. Scope must contain
1–32 unique names. Only loopback is permitted; only authenticated `POST /mcp` is
served. The endpoint wraps requests into the existing private owner IPC with its
operator-controlled role and scope. Caller-supplied roles/scopes cannot widen
permissions, nor can the agent use dashboard or companion routes. MCP messages
are stateless JSON responses, with 202 for notifications. Requests are limited to
8 KiB and bounded deadlines; authenticated `media_voice` requests alone may use
up to 64 KiB for the bounded base64 PCM payload. Endpoint counters permit 30 authenticated MCP
requests per minute and 20 control attempts per hour, independently of ZeroClaw.
Discovery/reads count toward the minute limit; initialization and denied attempts
also count. Excess traffic receives HTTP 429 before owner dispatch. Counters reset
on Homewire restart; schedules and trusted dashboard/companion requests are
unaffected. A restart burst can exhaust the minute budget: wait for the next minute
and restart ZeroClaw if MCP initialization failed. Agent responses are capped at
64 KiB by ZeroClaw. The separate API worker uses a 256 KiB stack and fixed-size
counters; it creates no additional device manager, polling loop or scheduler.

### Install, verify and update

Requires an unlocked rooted ARM64 Android 15 target with mount namespaces and
seccomp, Magisk, debuggable Termux, a dynamic ARM64 ZeroClaw 0.8.5 binary and an
existing ZeroClaw agent named in the private deployment profile and Homewire HTTPS API/assistant configuration.
Configure the assistant gateway with `connect_assistant.py` **before** isolation.
Use the same SDK/platform/signing environment as the companion build; retain the
private persistent signing key for future APK updates.

```sh
# HOMEWIRE_NDK points to NDK 30.0.16248370.
sh scripts/android/sandbox/build.sh
# Set JAVA_HOME, HOMEWIRE_ANDROID_PLATFORM, HOMEWIRE_ANDROID_BUILD_TOOLS,
# HOMEWIRE_SIGNING_KEY and HOMEWIRE_SIGNING_PASSWORD_FILE as for the companion.
sh scripts/android/sandbox/build-identity.sh
python3 scripts/android/isolate.py --config .local/home.json --serial SERIAL install
python3 scripts/android/isolate.py --config .local/home.json --serial SERIAL verify
# Apply operator tool/channel policy without replacing native binaries or firewall scripts:
python3 scripts/android/isolate.py --config .local/home.json --serial SERIAL policy
# Rebuild native artifacts before updating them; policy/model keys remain intact.
python3 scripts/android/isolate.py --config .local/home.json --serial SERIAL update
python3 scripts/android/isolate.py --config .local/home.json --serial SERIAL restart
```

The installer checks the configured MCP port is free, takes a cold copy of agent state,
hash-verifies transfers, disables the
old runit agent and moves the legacy root/pre-unlock boot hook out of service.d.
Original ZeroClaw files remain a private recovery copy. Homewire keeps its saved
schedule/monitor/control flags. If adding the agent listener fails, the installer
restores Homewire's prior API configuration without re-enabling the old agent.
Do not run installation twice over an existing jail; use `update` for native guard
changes. Use `policy` for tool/channel grant changes; it validates the pinned
CLI configuration, stops the agent, saves a private policy backup, writes updated
configuration and tool instructions, runs the installed negative probe and
restarts the agent. It preserves native binaries and network scripts. Homewire
restarts only if its agent tool scope changes. Policy and native updates retain
the previous policy privately; native updates also retain previous native files and
stay stopped if their negative probe fails. They never restore older permissions
on failure. `verify` checks policy, the actual daemon's guard/capabilities/seccomp
and native file/execution/network denials;
perform a separate trusted read-only assistant request to check model connectivity.
No automated device-changing verification is performed.

The root boot supervisor waits for Android credential unlock, sets the UID
firewall before launch, holds a single-instance lock and restarts after failure.
A bounded native log writer keeps at most two 64 KiB log files. Restarting the
supervisor kills its child via a kernel parent-death signal. Agent credentials
remain in CE storage; Homewire's separate Direct Boot owner can run before unlock.
Legacy grant helpers refuse changes using the `zeroclaw-isolated` marker.
Operator-only policy files live under
`/data/data/com.termux/files/home/zeroclaw-sandbox/policy`. Stop the supervisor
before an operator policy edit, preserve private modes/UID ownership, and restart
it to remount replacements. Never edit the dormant `.zeroclaw` copy expecting to
change the active deployment. The agent cannot administer this procedure.

## Android Direct Boot and recovery

On a supported rooted Android device with Magisk,
`scripts/android/direct_boot.py --config .local/home.json --serial SERIAL install`
can move the native owner into `/data/user_de/0/com.termux/files/homewire` and
install a Magisk boot supervisor under `/data/adb/service.d`. It preserves the CE
copy, verifies the native executable as Termux's UID, keeps private modes/SELinux
context, stops the old owner before starting the new one, and rewrites only device
config paths and executable version links. Normal installs then update the DE
deployment and preserve flags,
state and version rollback. Re-run `connect-zeroclaw` to migrate the exact existing
managed MCP bridge paths; conflicting custom definitions still fail for review.

The device owner runs under Termux's unprivileged UID with the network group and
all Linux capabilities dropped. Magisk supplies the runtime SELinux domain; its
MCS categories match Termux so newly written private files remain accessible to
the normal bridge after unlock. Root is used by setup and supervision, without
changing the phone's SELinux policy. The AI key/config remains in
credential-encrypted storage, so
ZeroClaw still requires Android unlock. Direct Boot does not protect LAN device
keys from root: they must be readable to automate before unlock. The supervisor
rotates two small logs and restarts only the owner after failure. Do not treat an
installed boot script as proof of before-unlock operation; validate with a reboot.

See [historical validation](validation.md#android-15-sample) for a short Android
memory sample; measure the actual target workload. Updates and ZeroClaw
configuration still require Android unlock.

Before replacing another controller, validate exclusive scheduler ownership,
a full TV cycle, companion notifications/recovery, Cast controls, actual timed runs, power/network
recovery and sustained operation with the previous controller stopped. Back up
the phone's device config, state/history, media certificates and APK signing key outside the phone. Existing
Samsung pairing is still imported/backed up; fresh H-series pairing has no native
setup helper. `pair_digi.py` is an independent host-only PIN pairing helper for
Android TV and requires the displayed code and a reachable LAN endpoint.


## Offline ultrasleep

The optional root-only controller for Android user 0 parks the phone's configured workloads and
Termux services, releases the Termux wake lock, disables cellular/Wi-Fi/Bluetooth
and scanning, enables battery saver, switches the screen off and forces deep
Doze. It restores the saved radio settings, wake lock and previously running
services at the configured end time. Existing disabled services remain disabled. Homewire device
states and schedules are preserved; its scheduler skips missed jobs. Android
itself remains running and may briefly wake for essential system work.

The daily window and IANA timezone are explicit private-profile settings, including
daylight-saving changes. There is no household schedule built into the operator.
A native `CLOCK_REALTIME_ALARM` timer wakes the kernel; an
`EPOLLWAKEUP` event keeps it awake during each transition. The controller blocks
between deadlines instead of polling. No network, remote host, Termux wake lock,
or agent is needed to resume. An operator wake bypasses only the current night.

Requirements: rooted Android with Magisk, `CAP_WAKE_ALARM` and
`CAP_BLOCK_SUSPEND`, Termux clang/runit/curl and OpenSSL development files, managed SSH, a working ingress-guard
`check` command, NFC already off, and no pre-existing forced Doze. Host tests require a C compiler
and OpenSSL discoverable through `pkg-config`. Keep private
inventory under ignored `.local/`, with profile permissions `600`. For example:

```json
{
  "schedule": {"timezone": "Etc/UTC", "start": "21:30", "end": "06:15"},
  "ssh": "phone-admin",
  "su": "/debug_ramdisk/su",
  "prefix": "/data/data/com.termux/files/usr",
  "homewire": "/data/user_de/0/com.termux/files/homewire",
  "guard": "/data/adb/example-ingress/guard.sh",
  "units": [
    {
      "pidfile": "/data/adb/homewire-supervisor.pid",
      "marker": "/data/adb/service.d/homewire.sh",
      "launcher": "/data/adb/service.d/homewire.sh"
    },
    {
      "pidfile": "/data/adb/zeroclaw-sandbox.pid",
      "marker": "/data/adb/service.d/zeroclaw-sandbox.sh",
      "launcher": "/data/adb/service.d/zeroclaw-sandbox.sh"
    }
  ],
  "freeze_executables": [],
  "resume_services_first": ["tailscaled", "sshd", "ssh-unix-bridge"],
  "telegram_config": "/data/data/com.termux/files/home/zeroclaw-sandbox/policy/config.toml",
  "telegram_section": "[channels.telegram.synthetic]"
}
```

List every root workload supervisor, including any keep-unlocked helper.
`freeze_executables` optionally parks native workloads that lack a compatible
stop/start supervisor, preserving their in-memory state; resume verifies each
PID's executable and start time. The phone's ingress guards remain active.
All currently running Termux runit services are parked through their supervisors.
Entry rejects failed or unrecognized service-status queries before parking any
workloads; it never treats an unavailable supervisor as a stopped service. Each
service stop/start is followed by a status check, and root supervisor restoration
waits for its expected PID identity before claiming success.
The controller is outside those services. When `telegram_config` and `telegram_section` are explicitly configured, nightly
resume clears that enabled bot's pending Telegram updates before launching the
agent. Its token is read from the existing protected configuration, with authenticated
`enc2:` decryption using the adjacent protected `.secret_key`, and passed to curl
through an anonymous pipe. No credential is stored or placed in argv. The
five-minute test preserves the daytime queue. Other application queues remain
owned by those applications. Services listed in `resume_services_first` resume after the ingress check and
before queue clearing, allowing administration during recovery. Only services
that were previously running are restored. If queue clearing fails, the recovery
journal stays and the agent remains parked, preventing delayed command execution.

The queue clear uses Telegram's documented negative-offset/confirmation behavior
([getUpdates](https://core.telegram.org/bots/api#getupdates)). It clears the bot's
pending queue at resume, including any older unprocessed updates. It sends no
messages and changes no channel enablement.

```sh
python3 scripts/android/ultrasleep.py --profile .local/ultrasleep.json stage
python3 scripts/android/ultrasleep.py --profile .local/ultrasleep.json test
# Wait for the full five-minute offline interval and managed SSH reconnection.
python3 scripts/android/ultrasleep.py --profile .local/ultrasleep.json status
python3 scripts/android/ultrasleep.py --profile .local/ultrasleep.json enable
```

Staging builds a versioned native executable on the phone, verifies transferred
SHA-256 hashes and probes the wake alarm before touching services. `test` arms
an independent recovery alarm before entry, then measures 300 seconds using
`CLOCK_BOOTTIME_ALARM`. Status records actual kernel suspend time as elapsed
boottime minus monotonic time. Enabling requires a successful test with at least
180 seconds of actual suspend. Inspect restoration of the original services and
radios before enabling. A high-frequency wake alarm in another watchdog can
prevent kernel suspend; use its supported recovery or sleep-compatible mode
while preserving active ingress protection. Do not disable firewall rules to
pass the test. The nightly boot wrapper is installed only by `enable`.

Transitions persist a root-only recovery journal before changing services.
Failures and controller exit trigger restoration; a failed restoration retains
the journal for another attempt. Records and the status file are bounded and
contain no application credentials. Versions and the previous boot wrapper stay
under `/data/adb/homewire-ultrasleep` for operator rollback. Use the operator `recover` command to retry a failed restoration. Never remove a
recovery journal before restoring its saved state.
One completed journal is retained as `last-transaction`, including the observed
service states and transition timestamps, until the next successful restoration.
This bounded record survives a controller restart or phone reboot. A restore proof
confirms the saved settings and supervised processes, not remote network reachability.

While reachable, `wake` sends an absolute wake request. During full offline sleep,
use a local root terminal to send `kill -USR1` to the PID in
`/data/adb/homewire-ultrasleep/controller.pid`; waking the screen alone does not
resume the parked services. Use the operator `disable` command to remove the nightly boot wrapper and send
`SIGTERM` to the controller, then verify that the recovery journal clears. For rollback, stop the controller
first, restore the saved previous boot wrapper if present, and launch it. No
wireless debugging or security-policy relaxation is required.
