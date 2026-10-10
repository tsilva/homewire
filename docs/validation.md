# Historical validation

[Current implementation and measurement procedure](development.md#implementation-and-validation).
These records describe earlier builds and are not guarantees for the current executable.

The 2026-10-08 entity/routine refactor was checked with 158 synthetic tests,
formatting, Clippy and an optimized release build. Awair multi-sensor requests
and P110 relay/meter requests verify one physical snapshot per batch; a second
request fetches again. Standard switch controls verify the existing single-write
sequence. Tests also cover unknown targets before I/O, unavailable/null results,
MCP control gates, TV power ownership and deadline selection. One lock-reopen
test failed once under parallel execution; five subsequent plug-suite runs and
the final complete suite passed. No lock implementation was changed in this refactor.

On macOS ARM64, the same seven physical adapter families were measured before
(commit `ac7f16e`) and after the refactor. Each column is the median of three
runs of `python3 scripts/measure_memory.py --media`, with 1,000 metadata requests,
synthetic credentials and no device traffic, schedules or monitoring:

| Measurement | Before | Standard entity/routine interface |
| --- | --- | --- |
| Initialized idle RSS | 3,248 KiB | 3,360 KiB |
| Largest sampled RSS per run | 3,664 KiB | 3,632 KiB |
| Release executable | 2,670,592 bytes | 2,720,336 bytes |

The new architecture adds 112 KiB to median idle residency in this sample;
metadata-request residency remains around 3.5 MiB. Running the same new binary
with `--legacy-tools` measured 3,328 KiB median idle and 3,712 KiB median largest
sample. These are desktop samples, not true peaks or Android measurements.

The allocation probe compares both tool interfaces with controls enabled for
Meaco, lights, plugs, printers and Awair, excluding request construction:
legacy discovery uses 398 allocations / 49,805 requested bytes for 15 tools;
standard discovery uses 244 / 30,378 for seven tools (about 39% fewer allocations
and requested bytes). This measures transient allocation requests, not live heap.
Existing zero-allocation checks for unchanged faults, in-place KLAP decryption
and valid configuration remain intact. Whole-device status JSON moves into the
reply after scalar projections, avoiding a redundant whole-tree clone.

Background waits now follow enabled deadlines (normally a 30-second poll or a
minute clock boundary), replacing the one-second receiver timeout. Idle MCP
without background flags retains no periodic wakeup. No measured CPU percentage
or battery improvement is claimed; target CPU/RSS, screen-off and reboot testing
remain pending.

The 2026-10-08 modularization compared against `00f926f`: macOS ARM64 idle RSS
remained 3,152 KiB and the median largest sampled RSS remained 3,440 KiB across
three runs. The executable changed from 1,957,584 to 1,957,616 bytes (+32 bytes).
The full writable light-alias tool list still uses 398 allocations/49,805
requested bytes; configuration validation and unchanged-fault decisions still
allocate nothing. A synthetic comparison of 26,240 MCP tool-list/call responses
(including invalid arguments, disabled families, control permissions and aliases)
matched the previous implementation exactly. No real devices were queried or
controlled. These are desktop measurements, not Android guarantees.

The 2026-10-08 media build measured **3,248 KiB idle / 3,648 KiB median largest
sample** across three macOS ARM64 harness trials, with all seven device families
configured (`python3 scripts/measure_memory.py --media`, synthetic credentials,
1,000 tool-list requests, no device I/O). The release binary is 2,670,592 bytes,
up from 1,957,616 at `aadf986`; the added native TLS implementation accounts for
most of the added dependency surface. Cargo.lock keeps 23 new crates, including
target-specific/build dependencies, at versions older than seven days.

A separate live **TV workflow** process (Samsung, DIGI and one P110 manager)
measured 3,312 KiB idle, 3,408 KiB largest sampled RSS, and 3,280 KiB after 30
read-only MCP calls, sampling RSS every 50 ms. Neither these samples nor the
metadata harness prove a true peak or Android footprint. No media adapter retains
TLS configs, sockets or protobuf messages between operations. The deployment
now has a tested Android build and phone idle sample; real peaks and longer
background-operation validation remain to measure. See [Android deployment](android.md#android-deployment).

In the 2026-10-07 macOS ARM64 comparison against commit `51dd544`, the process
measurements were modestly lower. RSS rows below are medians of three runs;
the second row uses each run's largest sample:

| Measurement | Before | After |
| --- | --- | --- |
| Initialized idle RSS | 6,928 KiB | 6,880 KiB |
| Largest sampled RSS per run | 7,392 KiB | 7,264 KiB |
| Release executable size | 2,089,856 bytes | 2,073,344 bytes |

A further 2026-10-08 pass compared against commit `16efa93`, using the same
macOS ARM64 harness. RSS values are medians of three runs:

| Measurement | Before | After |
| --- | --- | --- |
| Initialized idle RSS | 6,880 KiB | 3,152 KiB |
| Largest sampled RSS per run | 7,248 KiB | 3,472 KiB |
| Release executable size | 2,073,344 bytes | 1,957,584 bytes |
| Full writable light-alias tool list: allocations | 555 | 398 |
| Full tool list: requested allocation bytes | 70,548 | 49,805 |

This idle comparison disables background jobs; those modes still need a reader
thread. Runtime memory includes OS/platform overhead and will differ on Android.
The release panic setting alone saved 115,856 bytes in an intermediate build.
Changing Chrono features removed 28 packages from the lockfile, mainly unused
host-timezone/platform dependencies, without adding dependencies. Validation of
all three example endpoint groups allocates nothing after deserialization; MCP
replies still allocate temporary JSON trees. The allocation probe checks both
paths alongside unchanged faults and KLAP frames.

All 129 tests pass, including both MCP input modes, invalid JSON, notification
silence, final messages without a newline, oversized/non-UTF-8 input, atomic
batch-save failures, watch/check event deduplication and unknown readings during
recovery. All 128 family/control/alias tool-list combinations were compared
against the prior implementation and matched exactly. Formatting and Clippy
checks also pass. No device was controlled during these synthetic measurements.

The probe asserts allocation counts, reporting requested allocation bytes;
these numbers are neither live heap usage nor RSS. On macOS ARM64 (2026-10-07),
an unchanged reported fault fell from 2 allocations/34 requested bytes to zero;
encrypting `get_device_info` fell from 4 allocations/180 requested bytes to
1 allocation/64 requested bytes. In-place decryption uses zero allocations.
Only the fault decision is allocation-free; network reads, decoded statuses and
MCP replies still allocate bounded temporary storage.
