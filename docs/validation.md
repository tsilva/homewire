# Historical validation

[Current checks and measurement procedure](development.md#implementation-and-validation).
These records describe earlier builds, not the current executable. macOS ARM64
RSS values below are medians of three harness runs, using synthetic credentials,
1,000 MCP metadata requests and no device traffic or background jobs. Largest
samples are not true peaks; allocation counts/bytes are transient requests,
not live heap. Android must be measured separately.

## Desktop comparisons

| Date / change | Baseline | Idle RSS, before → after (KiB) | Largest sampled RSS, before → after (KiB) | Executable bytes, before → after |
| --- | --- | --- | --- | --- |
| 2026-10-07 allocation pass | `51dd544` | 6,928 → 6,880 | 7,392 → 7,264 | 2,089,856 → 2,073,344 |
| 2026-10-08 idle/runtime pass | `16efa93` | 6,880 → 3,152 | 7,248 → 3,472 | 2,073,344 → 1,957,584 |
| 2026-10-08 modularization | `00f926f` | 3,152 → 3,152 | 3,440 → 3,440 | 1,957,584 → 1,957,616 |
| 2026-10-08 media adapters (`--media`) | Media build | 3,248 | 3,648 | 2,670,592 |
| 2026-10-08 entity/routine refactor (`--media`) | `ac7f16e` | 3,248 → 3,360 | 3,664 → 3,632 | 2,670,592 → 2,720,336 |

The entity/routine build in `--legacy-tools` mode measured 3,328 KiB idle and
3,712 KiB largest sample. A separate live TV workflow process sampled every
50 ms measured 3,312 KiB idle, 3,408 KiB largest sample and 3,280 KiB after
30 read-only MCP calls. None establishes a real workload peak or battery budget.

## Allocation and correctness checks

| Probe | Before | After |
| --- | --- | --- |
| Unchanged fault decision (2026-10-07) | 2 allocations / 34 bytes | 0 / 0 |
| KLAP `get_device_info` encryption (2026-10-07) | 4 / 180 | 1 / 64 |
| Writable light-alias tool list (2026-10-08) | 555 / 70,548 | 398 / 49,805 |
| Legacy → standard discovery in entity/routine refactor | 398 / 49,805 (15 tools) | 244 / 30,378 (7 tools) |

In-place KLAP decryption, valid endpoint configuration checks and unchanged-fault
decisions allocated nothing. Network reads and JSON replies still allocated
bounded temporary storage. The release panic setting saved 115,856 bytes in an
intermediate build; removing unused Chrono host-timezone features removed 28
lockfile packages.

The earlier optimization passed 129 tests, formatting and Clippy; 128 tool-list
family/control/alias combinations matched the prior implementation. Modularization
matched 26,240 synthetic tool-list/call responses, including invalid arguments,
disabled families, permissions and aliases.

The entity/routine refactor passed 158 synthetic tests, formatting, Clippy and a
release build. Coverage included one physical snapshot per batch, fresh later
requests, single-write controls, validation before I/O, unavailable/null results,
MCP grants, TV ownership and deadline selection. One lock-reopen test failed once
in parallel; five later plug-suite runs and the final full suite passed without
lock changes. These synthetic checks controlled no physical devices.

## Android 15 sample

A short ARM64 Android 15 observation with monitors enabled recorded:

| Component | Observation |
| --- | --- |
| Native owner | 5,260 KiB RSS; 5,852 KiB high-water RSS; 3 threads; 1 CPU tick in 10 idle seconds |
| ZeroClaw | 23,968 KiB RSS |
| Companion | 6,534 KiB proportional memory; about 78 MiB RSS including shared Android runtime pages |
| Artifacts | About 21 KiB companion APK; 3.4 MiB native executable |

These are platform observations, not worst-case memory/battery budgets or a
current installation inventory. Validate active workloads, screen-off operation,
network recovery and reboot on the actual target; keep deployment records private.
