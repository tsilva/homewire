# Betty for Android

Hold the assistant shortcut, speak, review the transcription, and tap **Send**.
Betty sends text into your existing private Telegram bot conversation, then opens
that conversation in Telegram after delivery is confirmed. The native dark teal
screen puts a plain editable transcript above a small bottom control bar, without
a card, repeated headings or waveform. Conversation history and replies stay in Telegram.

This is an unofficial Telegram user client using TDLib. It does not start a
second bot, consume the bot's update stream, reset the assistant, or use a separate
gateway. Account, bot user and chat IDs are pinned after selection. Nothing is
sent on open; there is no automatic `/start`, test message or retransmission.

## Set up on a phone

1. Install the signed production APK, `eu.tsilva.homewire.betty` (Android 12+;
   ARM64 phones and x86-64 emulators). Keep the APK and receipt for recovery.
2. Enter an API ID/hash from [Telegram's development tools](https://my.telegram.org/apps)
   and the existing bot's username. These are app credentials, not a bot token.
   Enter credentials, phone numbers, login codes and passwords on the phone only.
3. Sign into your existing Telegram account and confirm the existing bot chat.
   A new/empty bot conversation is refused. Passwords are submitted exactly as
   entered; errors distinguish rejected passwords, expired checks and rate limits.
   Keep regular Telegram signed in while troubleshooting; do not reset the account.
4. Allow microphone access. Choose **Settings → Assistant settings** in Betty
   and select Betty as the digital assistant. On Pixel, also choose **Settings →
   System → Gestures → Press & hold power button → Digital assistant**.
5. Hold power with the phone unlocked. Speak, tap **Stop**, review or edit the
   transcription, then tap **Send**. Once Telegram confirms delivery, Betty opens
   the pinned conversation in the official Telegram app and closes its capture
   screen. The keyboard control can compose text without recording.

Dictation uses Android's explicitly on-device recognizer and the installed
language model. It never switches silently to cloud recognition. Partial text is
shown during recording; Send remains unavailable until final recognition finishes.
Empty or whitespace-only text keeps Send disabled. Tap the microphone beside
Send to record again, or edit the text directly. Back offers to discard an unsent
message. If recognition is unavailable, keyboard input remains available. Only text is sent;
raw voice notes and playback are not part of this flow.

Leaving the foreground stops capture into an unsent review. Opening again preserves
an existing unsent draft. Recording is capped at 120 seconds and text at 4096
characters. Drafts are not backed up or sent after process termination. If delivery
is uncertain, Betty shows a warning and offers to open Telegram without repeating
the message. Acknowledging the warning does not count as confirmed delivery.
A send confirmed in the background defers navigation until Betty is foregrounded.

## Session and delivery protection

- The Telegram user session has account-level access. The single-destination UI
  is an application boundary, not a Telegram-issued per-chat scope. Revoke the
  extra session at **Telegram → Settings → Devices** when needed.
- Android Keystore AES-GCM seals configuration and the TDLib database key.
  Session storage is app-private, encrypted and excluded from backup/transfer.
  Login fields disable saved view state/autofill and screenshots. TDLib logging
  is disabled. Never put credentials or personal destination identities in Git.
- Message/chat/file databases and secret chats are disabled. Betty does not fetch
  conversation history after setup, retain message bubbles, or mark chat messages
  read. A single-message history query verifies an existing chat during selection.
  On every start the bot is resolved through Telegram and checked against the
  saved chat/user IDs; reopening does not depend on a local chat cache. A changed
  username cannot silently replace the pinned destination.
- No raw audio file, microphone foreground service, boot receiver, accessibility
  service, notification reader, plaintext relay or analytics is introduced.
- A pending flag is saved before submission. Only a Telegram send-success result
  clears it as delivered. Uncertain results block another send until reconciled.
  TDLib may continue its original submission; Betty never resends it. Telegram
  acceptance does not prove that the receiving assistant has acted or replied.
- Navigation resolves the configured username and checks it still matches the
  pinned chat before opening the official Telegram package. Its deep link carries
  no draft text, `/start` command or second send. Navigation failure leaves the
  confirmed message sent and provides an Open Telegram action.

## Repeatable releases

Use [$homewire_betty-release](../../.agents/skills/homewire_betty-release/SKILL.md)
for future builds and verified updates. Its `scripts/artifact.py verify` command
checks an existing production APK without contacting a phone; `install` performs
one explicitly authorized, targeted USB update. The helper backs up the installed
APK, pins signing identity, verifies installed bytes and preserves assistant,
power-button and USB-debugging settings. It does not open Betty unless `--launch`
is selected. A private `.local/betty-device.json` binding selects the device and
previously verified signer; the skill documents its schema and command options.
An existing installation receipt blocks automatic replay after an uncertain result.

## Build

Builds use JDK 17, Android platform 35, build-tools 35.0.0 and the existing signing
identity through scoped native Infisical injection. `--signing-config` reads only
`toolchain` and `signing` bindings from an owner-only private configuration. The
TV binding format may be reused without invoking the TV release workflow.

```sh
python3 companion/betty/build.py \
  --signing-config .local/android-signing.json \
  --output .local/betty-artifacts/0.1.4/betty.apk
```

The binding supplies `HOMEWIRE_TV_SIGNING_KEY_B64` and
`HOMEWIRE_TV_SIGNING_PASSWORD`. The key is passed to `apksigner` over stdin;
no temporary keystore is written. Preserve that identity for updates. Without
private configuration, use the toolchain environment variables documented by the
[TV build](../tv/README.md), injecting signing variables only into the build.
`--unsigned` produces a validation artifact that cannot be installed.

`dependencies.lock.json` pins TDLib Java/native bytes from the Telegram X bundle,
the matching NDK C++ runtime, compile-only AndroidX annotations and Material icons.
Inputs are hash verified and cached in ignored `.local/betty-deps/`. Upgrades must
review the matching Java/native/runtime set together. Native libraries and APK
packaging support 16 KiB alignment. Receipts record version, dependency commit,
APK hash and a frozen source archive.

Builds refuse existing outputs. Verify the receipt and signer, preserve the old
APK, then update with `adb -s EXPLICIT_TARGET install -r APK`. Do not uninstall,
clear app data, change signing identity or blindly retry an uncertain installation.
The build does not deploy or enable debugging. Installation preserves the session
and assistant shortcut.

## Verify without sending messages

```sh
PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover \
  -s companion/betty/tests -p 'test_*.py'
python3 companion/betty/build.py --signing-config .local/android-signing.json \
  --preview --output .local/betty-artifacts/0.1.4/betty-preview.apk
python3 companion/betty/build.py --signing-config .local/android-signing.json \
  --checks --output .local/betty-artifacts/0.1.4/betty-checks.apk
```

The separate preview package (`eu.tsilva.homewire.betty.preview`) has synthetic
transcripts, no Internet permission and no Telegram client or sending capability.
The instrumentation APK targets production only on a disposable emulator. It
checks actual native loading, transcript review/edit/discard, foreground-only
capture state, deferred navigation from a synthetic delivery receipt, uncertain
send handling and Keystore encryption. It writes synthetic test state; never run
it on a personal phone or an emulator signed into an account.

```sh
adb -s EMULATOR_SERIAL shell am instrument -w \
  eu.tsilva.homewire.betty.tests/eu.tsilva.homewire.betty.tests.Checks
```

Account login, live speech recognition, the physical power shortcut and a real
send/navigation need user acceptance. Any agent-operated live message requires
approval of its exact draft and recipient before sending.

## Third-party notices

TDLib: Boost Software License 1.0. OpenSSL and Material icons: Apache 2.0.
Matching NDK runtime licensing and upstream notices are preserved in the APK.
AndroidX annotations are compile-only and are not packaged as application code.
