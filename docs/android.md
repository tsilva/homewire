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

The profile is bounded to 8192 bytes and read by host-side setup tools. Generated
launch scripts, firewall rules, MCP URL and native probe arguments use the same
validated values. Homewire still loads only its own typed adapter settings once
at startup. Device credentials stay in their existing configs, not this profile.
On the phone the resolved profile is retained as a root-only policy snapshot.
Private profiles and automation settings must not be committed to Git.

Install and update validate the prepared policy through the pinned ZeroClaw CLI
before changing the active policy; an update fails before stopping the agent if
validation changes its declared scope.

An isolation update requires the profile to match the operator-managed API and
assistant endpoints before stopping anything. To change ports or agent identity,
coordinate those private configs first; mismatches fail instead of silently
switching or restoring permissive grants. Changing DNS/provider/tool subsets
uses the normal sandbox update. Alias changes require the selected agent to
already exist in the active policy, and its workspace/mount is prepared by update.

For existing configs, explicitly add Meaco presets/schedule, curve/outside target
to any enabled light automation, and TV timings before installing the new binary.
Preserve current schedule flags and HA ownership. Back up settings, replace private
files atomically and restart the owner to load changes; no hot reload or control
replay occurs. Restart isolation through its operator helper to remount policy.


Host requirements: ADB, Python 3.11+, rustup with Rust 1.95.0 and
`aarch64-linux-android` installed, and Android NDK **30.0.16248370**. Install the
Rust target with `rustup target add --toolchain 1.95.0 aarch64-linux-android`.
Set `HOMEWIRE_NDK` to the extracted NDK directory. Build scripts accept macOS
and Linux hosts and use Android API 28; this is an Android/Bionic executable,
not a generic Linux or macOS binary. NDK host tools run only on the build host.

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
The Python script runs on the host, never on the deployed runtime. It requires the installed
Termux app to permit `run-as com.termux` and already have `termux-services` with a
running supervisor. It fails early without that access; arbitrary release Termux
installations may require a separate SSH/root installation route. Homewire itself
runs unprivileged as Termux's user. Root is not a runtime requirement.

Build output is `target/android-package/homewire` with `manifest.json`, recording
the binary SHA-256, source commit/dirty flag, Rust source/manifest tree hash, target,
API level and tool versions.
Installer compares host and phone hashes and checks the executable before
switching the active version. Executables live in `~/homewire/releases/HASH/`;
`current` and `previous` links select versions. Configs, state and locks stay in
`~/homewire/config/`, independently of executable updates. This is a local build
and installation procedure, not a package publication or GitHub Release workflow.

The migration helper, file transfer allowlist and legacy launcher share
`scripts/android/catalog.json`; Rust tests check it against the compiled catalog.
Legacy hardware inputs include `wol.json` and `computer.json`. Migrate optional
`computer-routine.json` into unified configuration to enable computer sequencing.

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
With unified `--config`, installation also updates runtime settings. A TV config owns its Samsung, DIGI and entire plug
manager, so the generated service does not load those separately.

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
The server launches the same executable as `bridge` with absolute paths and a
300-second tool timeout. Tools appear as `homewire__entity_list`, etc. A working
model provider is separately required for agent inference.

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

The trusted local assistant gateway allows explicitly selected Homewire tools; discovery
omits tools whose adapters are absent. It cannot call
`entity_configure`. Telegram can use these read tools: `entity_list`,
`entity_status`, `entity_options`, `routine_list`, `routine_plan`, `alert_list`,
`history_series`, `history_query`, `weather_status`, `shopping_list`, `radio_search`,
`tts_media` and `wake_list`. With no control grant it cannot change devices, run routines,
play media or edit shopping items. Supported opt-in write tools are `light_set`,
`switch_set`, `media_power`, `remote_key`, `media_launch`, `media_text`, `routine_run`, `media_play`,
`media_command`, `shopping_update` and `wake_on_lan`; each must also be in the agent's tool scope.
Grants apply to whole tools, so `switch_set` permits all eligible configured
switches, with Homewire's identity checks and device guards still enforced.
`media_launch` and `media_text` are limited to media remotes advertising app/text
support. They do not grant `entity_configure`. Link/text delivery is unverified;
never automatically replay a failed or uncertain control or claim playback from
delivery alone. Generated instructions ask for screen confirmation after an
unverified shutdown and attribute visual confirmation to the user, retaining
the backend's separate verification status and attempt guard. The user's latest
correction replaces earlier visual reports. Guard expiry never triggers an
automatic retry; another attempt requires a new explicit request and fresh state
accepted by the adapter. Unreachable readings remain unknown, and unrelated
remembered tasks are not new requests. Short follow-ups retain the last device
selected by the user; an unsolicited assistant mention does not switch focus.
Generated instructions avoid repeated permission for an already authorized
control, unrelated discovery and unsupported tool arguments. Replies report the
outcome briefly. No extra TV polling, screen capture, ADB
or wake lock is added.
Android TV extensions use separate `media_activity`, `remote_hold` and
`media_voice` write grants. Activities send one named app/input/navigation action
and distinguish fresh app reports from unverified playback or screen changes.
Holds are limited to 2000 ms. Voice accepts at most 3 seconds of supplied base64
PCM and does not add microphone access to the agent. Configured Cast entities
use the existing `media_play` and `media_command` grants. Operator updates must
add requested tools to the API and agent scopes and, when desired, the explicit
Telegram controls; unrelated denials and disabled channels remain in force.
Network wake uses separate `wake_list` discovery and opt-in `wake_on_lan` control
grants. Generic targets send one packet without verifying wake; generated agent
instructions require a fresh media status before claiming the TV is on and
prohibit automatic replay or substituting wake after an uncertain power command.
Configured TCL integrated recovery stays inside one `media_power` operation.
The agent inspects its structured verification result instead of treating a
successful tool call as a successful power change. Expired verification is an
incomplete request; instructions prohibit invented recovery times and promises
of unscheduled background checks. Experimental Cast wake is disabled unless
the operator explicitly configures it after firmware validation.
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

An ARM64 Android 15 sample with monitors enabled measured 5,260 KiB owner RSS /
5,852 KiB process high-water RSS, three threads and one CPU tick over ten idle
seconds. ZeroClaw separately measured 23,968 KiB RSS. The companion measured
6,534 KiB proportional memory and about 78 MiB RSS, including shared Android
runtime pages; its APK was about 21 KiB. The native executable was about 3.4 MiB.
These are short platform observations, not worst-case memory or battery budgets
and not a statement about any installation. Measure the target workload yourself.
Updates and ZeroClaw configuration still require Android unlock.

Before replacing another controller, validate exclusive scheduler ownership,
a full TV cycle, companion notifications/recovery, Cast controls, actual timed runs, power/network
recovery and sustained operation with the previous controller stopped. Back up
the phone's device config, state/history, media certificates and APK signing key outside the phone. Existing
Samsung pairing is still imported/backed up; fresh H-series pairing has no native
setup helper. `pair_digi.py` is an independent host-only PIN pairing helper for
Android TV and requires the displayed code and a reachable LAN endpoint.
