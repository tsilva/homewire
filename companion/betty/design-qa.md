# Betty input design QA — 0.1.4

final result: passed

## Source, scope and evidence

The source is the user's current Android review screenshot, preserved privately as
`.local/betty-artifacts/0.1.4/qa/source.png` (738 × 1656 pixels). The requested change
is simplification of this existing production APK, not a clone or a web prototype.
Keep the dark palette and reviewed-text delivery into the pinned Telegram chat;
remove the central card, repeated headings, underline, waveform and verbose
instructions. Make recording again directly available beside Send.

Native emulator evidence lives in the same ignored QA directory:

- `recording.png`: plain live transcript with Listening/elapsed time and Stop.
- `review.png`: plain editable transcript, microphone retry and Send.
- `empty-review.png`: empty editor and disabled Send.
- `empty-keyboard.png`, `keyboard.png`: the bottom controls remain above the IME.
- `review-large-font.png`: the transcript wraps and Send remains accessible at 1.3×.
- `comparison-empty.png`: source and revised empty review together.
- `comparison-controls.png`: focused before/after control regions together.
- `ui-checks.json`, `instrumentation.json`: functional results using synthetic data.

Implementation captures are 1080 × 2424 pixels at 440 dpi, approximately
393 × 881 Android dp. For the combined comparison, the implementation is normalized
to the source's 738 × 1656 pixels; the aspect difference is pixel rounding. The
focused regions use equal 666 × 245 crops of the normalized frames, without
stretching individual controls. OS status/navigation indicators are system-owned
and excluded from design findings. The source is an empty review after a speech
failure; the preview comparison is an empty review after clearing synthetic text.
The short failure message itself is verified by production instrumentation rather
than presented as a matching live recognition failure in the preview.

## Findings and comparison history

The combined full-view and focused comparisons were opened together with the
recording, review, keyboard and larger-text captures. No actionable P0/P1/P2
issues remain. There was one visual review pass; no fixes were required after it.
Differences from the source below are intentional, authorized simplifications:

- The transcript starts near the top and uses the available area; there is no
  centered card or repeated review title.
- Send and recording retry are anchored at the bottom. The native keyboard
  resizes the text area and leaves both controls visible.
- Send is disabled for empty or whitespace-only text, and enabled after editing
  to a nonempty message. Replacing a nonempty draft requires confirmation.
- The microphone replaces the trash icon in review. Android Back retains its
  existing unsent-draft discard confirmation.
- Speech failure copy is a short inline message. The retry control is directly
  available; recording, authentication and Telegram delivery protections remain.

## Required fidelity surfaces

- **Fonts and typography:** retain native sans, a restrained 20sp header and 24sp
  transcript text with readable line spacing. No redundant title competes with
  the transcript. Text wraps at the checked font scale without hiding controls.
- **Spacing and layout:** 24dp horizontal content insets, a flexible scrollable
  transcript/editor, no panel or underline, and a compact bottom row. Secondary
  controls retain 48dp targets; the primary control is at least 56dp high.
- **Colors and tokens:** retain deep green background, off-white text, muted
  secondary text and mint primary action. Empty Send is visibly disabled. No
  new gradients, panels, shadows or decorative assets are introduced.
- **Image quality and icons:** use existing Material microphone, keyboard,
  cancel and menu assets. There are no raster assets to generate or approximate;
  the waveform implementation is removed. System chrome remains native.
- **Copy and content:** the transcript carries the main content. Recording shows
  Listening/elapsed time; normal review needs only Send. Failure and delivery
  uncertainty use short messages, with the existing delivery safeguards intact.

## Functional validation

Seven local contract tests pass, including cold-cache destination restoration,
identity pinning, password preservation, no implicit/repeated sending, and
receipt-gated handoff. Thirty-four production instrumentation checks pass on the
account-free disposable emulator. These include live/final transcript surfaces,
editing, an empty/failed transcript with disabled Send and direct retry, preserving
an unsent edit, background capture shutdown, uncertain sends, confirmed-delivery
handoff deferral, protected storage and a storage-failure guard.

Twelve offline preview interactions pass: Stop, final Send enablement, transcript
editing, no repeated title, local-only Send, empty-text protection, one-tap retry,
keyboard input and Send, protection of a nonempty draft, Keep, and larger text.
The preview has no Internet permission and no Telegram session. Required Rust
format/test/clippy, Android deployment tests and shell checks also pass.

## Acceptance limits and follow-up

Live recognition/model availability and a real send/navigation transition remain
phone acceptance checks. No live Telegram message was sent for QA. No P3 visual
polish is required for this deliberately minimal screen.
