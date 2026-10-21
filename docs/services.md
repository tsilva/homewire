# Household services and companion

[Reference index](reference.md) · [First use](../README.md)

## Household services

In unified configuration, put hardware adapters under `adapters` and services
under `services`; see [configuration](configuration.md#unified-configuration).
Legacy files use `--adapter FAMILY=PATH`. All run inside the existing device owner
with bounded messages/state and adapter-owned deadlines.

| Config example / family | Required fields and defaults |
| --- | --- |
| `examples/brother.json` / `brother` | `name`, literal-IP `host`; community `public`, timeout 3 s; optional test `connect` |
| `examples/cast.json` / `cast` | `name`, literal-IP `host`, `server_cert_sha256`; port 8009, timeout 8 s; optional test `connect` |
| `examples/dlna.json` / `dlna` | `name`, `description_url`, exact `udn`; timeout 8 s; service endpoints must stay on the description origin |
| `examples/mobile.json` / `mobile` | Up to 8 `devices`, home `latitude`/`longitude`; optional `persons` mapping name to device, radius 150 m, freshness 600 s |
| `examples/alerts.json` / `alerts` | Up to 8 companion names and 4 delivery channels; channels specify name/HTTPS URL, optional authorization/push registration; timeout 8 s |
| `examples/history.json` / `history` | 1–32 numeric entity IDs; interval 300 s (60–3600), max points 4096 (96–16384) per entity |
| `examples/weather.json` / `weather` | `latitude`, `longitude`; current MET compact measurements and 12 forecast entries |
| `examples/shopping.json` / `shopping` | Empty object; durable household list |
| `examples/radio.json` / `radio` | Optional HTTPS server origin, default `https://de1.api.radio-browser.info`; timeout 8 s |
| `examples/speech.json` / `speech` | Optional language, default `en`; Google Translate public speech URLs |

Brother SNMP sends only GET, verifies HL-L2350DW identity, and exposes page,
duplex, drum count/life, remaining drum pages and toner percentage. It complements
IPP; supply percentages are separate warnings, never inferred hardware faults.
Cast requires its own observed certificate pin; do not assume the remote port 6466
certificate also identifies port 8009. Explicit CLI `certificate-sha256 IP:PORT`
inspects first-use identity without saving or trusting it in any adapter. Verify
the endpoint before copying its pin; normal adapters reject changed certificates.
Cast launches the standard media receiver when needed. Controls are never replayed
on a timeout; results distinguish sent from confirmed playback.

### Typed settings

Discover schemas with `entity_list {"device":"FAMILY.NAME","details":true}`;
follow `next_offset`. Discovery defaults to 8 compact descriptors or 1 detailed
descriptor, accepts a limit of 1–16, and bounds a page to approximately 6 KiB.
Category tools validate the whole request before dispatching to the adapter.
Adapters prepare read-modify-write payloads before sending changes, preserve
unrelated settings, and verify after writes. A later failure can leave earlier
changes applied; no blind replay or fake transaction rollback occurs.

- Meaco: `power`, `target_humidity` 35–70 in steps of 5, `mode` manual/laundry/
  purify/sleep, `child_lock`, on/off timers cancel or 1h–24h. Power-off cannot be
  combined with mode. Sleep retains its one-send toggle semantics.
- Plugs: `auto_update`, `auto_off`, `auto_off_minutes` 1–1440, `indicator_led`
  always/never/night_mode, and P110 `protection_w` 0–3680 subject to live maximum (0 disables protection and
  requires maintenance).
  TV-owned outlet auto-off changes require maintenance. Firmware acceptance is
  checked live; no protection threshold is inferred from consumption.
- Lights: `auto_update`, `smooth_transitions`, separate fade on/off seconds where
  supported, `preset` 1–16, and `effect_enabled`. Live options include bounded
  preset pages and transition ranges. Effect activation restores only the effect
  already stored on the device; arbitrary factory effect programs and music
  configuration are not implemented. Apply preset/effect requests separately.
- DIGI: HTTP(S) `app_link` or `text` up to 512 bytes, one action per request.
  Text requires negotiated IME support and live counters from a focused field.

### Companion battery monitoring

The existing mobile adapter can optionally monitor battery measurements from
its authenticated Android companions. Add `battery_monitor` to `services.mobile`
(or the legacy mobile config):

```json
"battery_monitor": {
  "devices": ["phone"],
  "low_percent": 15,
  "recovery_percent": 20,
  "poll_seconds": 60
}
```

`devices` is a nonempty subset of the configured paired devices, at most 8 unique
names. The percentage and interval fields default to the values shown. Require
`1 <= low_percent < recovery_percent <= 100`; polling supports 15–3600 seconds.
Omitting `battery_monitor` disables the job. It uses existing companion telemetry
and the serve/daemon event loop.

The first check runs at startup, then every `poll_seconds`; an accepted report
from a monitored device also brings its check deadline forward. A fresh percentage
strictly below `low_percent` starts confirmation. Another received report at least
3 seconds later confirms the warning. An active warning stays active until
fresh reports at or above `recovery_percent` confirm recovery for 30 seconds.
Repeated checks of the same stored report cannot confirm either transition.
Missing, future-dated or stale reports cancel pending confirmation and never
establish recovery; `max_age_seconds` defines freshness. Charging does not
suppress a low warning; its state remains available through the existing sensor.

Confirmed state persists in `mobile.battery.state.json`. Configure `services.alerts`
for delivery under `battery.NAME`; include that key in any channel allowlist.
Messages stay constant within an episode, so changing percentages/restarts do not
resend unchanged warnings. Suspension, stopped reporting and network loss can
delay detection/delivery. Ledger retry/receipt semantics apply below.

### Alerts, history and convenience tools

`--watch-meaco`, `--watch-printers`, `--watch-lights` and `--watch-plugs` enable
independent debounced monitoring. Meaco tracks tank-full/problem, printer reasons
track hardware faults, and printer supplies at <=15% emit separate low-supply
warnings. Faults need 3-second confirmation; recovery needs 30 seconds.
Missing readings do not clear confirmed faults. Schedule and standard control
failures also enter the optional ledger. Events still go to stderr.

`alert_list {}` returns durable cards. The private ledger holds at most 64 alerts
and 64 KiB, acknowledges each channel independently, retries failed delivery no
sooner than 30 seconds, and keeps recovery updates pending until configured
companions acknowledge their current revision. Unchanged alerts cause no writes.
Delivery is at least once; receipt persistence prevents routine repeats. A full
ledger retains pending alerts rather than silently evicting them.

Channels default to `delivery: "webhook"`, preserving existing payloads and
acknowledgements. Optional `keys` is an exact allowlist of at most 64 alert keys;
omitting it or using an empty list routes all alerts. Filtered-out cards do not
become pending deliveries for that channel.

For Telegram, use `delivery: "telegram"`, `chat_id` (a numeric chat ID string or
`@channel_username`), and `url` in the form
`https://api.telegram.org/bot<BOT_TOKEN>/sendMessage`. Only that official HTTPS
endpoint is accepted. No push registration or authorization header is allowed
for this delivery type. Keep the URL in protected private configuration because
it contains the bot token. A changed recipient invalidates the previous receipt.
Telegram delivery requires both HTTP success and `ok: true` with a message ID;
response bodies and token-bearing URLs are never included in delivery errors.
Messages use plain text. `tank_full` becomes a request to empty the dehumidifier
tank, and confirmed recovery sends an explicit resolved message. This is outbound
notification only; it does not enable an agent's Telegram input channel. Recipient reading and phone display are not guaranteed.
See [Telegram's sendMessage contract](https://core.telegram.org/bots/api#sendmessage).

`history_series {}` lists configured entities/retention. `history_query` accepts
entity, optional Unix start/end, aggregation raw/hour/day and limit 1–96. Each
fixed 32-byte CRC record lives in an on-disk ring; maximum disk storage is
32 × 16384 × 32 bytes (16 MiB). It retains counters rather than JSON snapshots in
RAM. Raw samples and UTC hourly/daily min/max/mean are available. The dashboard
plots raw values or bucket maxima. Missing periods are not interpolated, and a
daily energy counter's observed maximum may miss use before midnight. HA statistics are not imported; preserve that archive before retiring its host.

`shopping_list {}` and `shopping_update` support add (name), check (id/complete),
remove (id) and clear_completed. Names are limited to 128 bytes; 64 items fit a
private 32 KiB state file. Duplicate incomplete names are suppressed.
`weather_status {}` returns MET measurements/forecast with timestamps.
`radio_search {"name":"..."}` returns up to 8 resolved station URLs.
`tts_media {"text":"..."}` accepts 1–200 characters and returns a public MP3 URL
for `media_play`. Google receives that text when playback fetches the URL. These public services require Internet and are best effort.

## Native Android companion

The platform Java app in `companion/android` has no third-party app dependencies. Build with JDK 17,
Android API 35 platform and build-tools 35, setting `HOMEWIRE_ANDROID_PLATFORM`,
`HOMEWIRE_ANDROID_BUILD_TOOLS`, `HOMEWIRE_SIGNING_KEY` and
`HOMEWIRE_SIGNING_PASSWORD_FILE`, then run `companion/android/build.sh`.
Keep the signing key/password private and back them up: updates require the same
signer. The output is `companion/android/build/homewire-companion.apk`.

Enable the daemon's frontend with `--api PATH`. `examples/api.json` documents the
schema: LAN listen address, a random 64-hex-character dashboard token, up to 8
clients with distinct name/token, certificate PEM and private-key PEM paths
(relative to config). LAN access requires TLS; unencrypted HTTP is restricted to
loopback. Generate independent tokens (e.g. `openssl rand -hex 32`), replace all
example values, and protect config/key files with mode 600. The frontend handles one bounded connection at a time on a 256 KiB-stack thread
and forwards device operations to the existing IPC owner.

Install the signed APK on a compatible Android phone and enter the server HTTPS
URL, certificate SHA-256
and **that phone's scoped token**, and press Save and start. Grant notifications;
location is optional. Companion tokens can only report their own telemetry and
read/acknowledge alerts, unless explicitly granted assistant access. They cannot
use the dashboard control API. Dashboard credentials stay separate and in browser
memory only. Android notification delivery may be delayed by network or power
management; polling is normally once per minute. Location uses passive/network
updates, and remains unknown when stale or its accuracy overlaps the home radius.
After a reboot, alerts/battery resume once Android unlocks; open the app to resume
optional location collection. There is no background-location permission request.

Optional voice uses Android's installed speech provider (preferring offline),
shows the transcript for review and sends it to ZeroClaw. Text is also available.
To configure the local assistant proxy, run on the host:

```sh
python3 scripts/android/connect_assistant.py --config .local/home.json --serial SERIAL --agent "$HOMEWIRE_AGENT"
```

It adds a dedicated gateway token while preserving model keys and existing paired
clients. Set `assistant:true` only for clients allowed to use that agent, then
import the updated API config using the normal installer. The frontend proxies
only its configured loopback ZeroClaw webhook. Agent calls are sent once, even after
an uncertain response. A 90-second wait can delay other frontend connections;
the independent device owner/scheduler continues. TV audio uses the separate [media_voice](devices.md#android-tv-extended-input)
tool; companion voice does not capture or forward it.

Remote access can use a Cloudflare One client and a tunnel private IP route
to the Homewire server's HTTPS port. Include that IP in WARP routing and
restrict network access to your identity/port. The pinned phone certificate works
with a private TCP route; publishing an edge-terminated HTTPS hostname requires a
different certificate trust strategy. A stable LAN address/DHCP reservation is
required. Keep the tunnel connector independent of any controller you plan to
retire so removing that controller does not remove remote access. Tailscale is
optional; this project installs neither.
