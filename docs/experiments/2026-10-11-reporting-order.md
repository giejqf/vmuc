# 2026-10-11 — Free page reporting order, and where an idle guest's footprint goes

**Question.** Linux reports free memory from `page_reporting_order`,
by default the pageblock order (9: 2 MiB on a 4 KiB-page guest), which
plan.md assumed. Does a smaller order give more memory back to macOS
without slowing the bulk path, and what is left once an idle Ubuntu
guest has dropped its caches?

**Method.** `tests/bench/mem-reclaim/run.sh ubuntu` (Ubuntu 24.04,
4 vCPUs, 4 GiB, M4 Pro, macOS 26.6.2) with
`VMUC_KERNEL_CMDLINE='page_reporting.page_reporting_order=N'`: settle
20 s, fill 2 GiB of tmpfs, delete it, sample `phys_footprint` every
0.5 s for 20 s, then `vmuc reclaim` (drop caches, compact) and read the
footprint 10 s later. Rows in `docs/benchmarks/mem-reclaim.md`. The idle
footprint was then itemized with `footprint(1)`, `vmmap -summary` and
the guest's `/proc/meminfo` and `/proc/buddyinfo`.

**Result.**

| Order | Range | Freed 2 GiB back in 10 s | 90 % back after | Idle after reclaim | Release p99 |
|---|---|---|---|---|---|
| 9 | 2 MiB | 100 % | 6.8 s | 728 MiB | 2.5 ms |
| 5 | 128 KiB | 99–100 % | 6.2–8.4 s | 503–510 MiB | 0.5–1 ms |
| 3 | 32 KiB | 95 % | 8.3 s | 481 MiB | 0.25 ms |

At order 9, `/proc/buddyinfo` after the reclaim showed ~125 MiB of free
guest memory in blocks below 2 MiB, which order 9 never reports. The
guest still reports large free blocks at their own size at any order, so
the bulk path does not fragment; order 5 picks up most of the small
blocks, and order 3 adds little more while the bulk path gets slower.

Itemizing the ~490 MiB that remains at idle (order 5, after the kernel
image moved from a heap copy to a file mapping):

| Where | About |
|---|---|
| Ubuntu in use (`MemTotal - MemFree`: cached binaries 108 MiB, anon 48, slab 27, …) | 236 MiB |
| Guest kernel's own reservations (struct pages for 4 GiB ≈ 64 MiB, kernel image ≈ 31 MiB) | ~95 MiB |
| VMM heap (seed rendering buffers libmalloc keeps dirty) | ~13 MiB |
| Free guest memory in blocks below 128 KiB, stage-2 tables, other | the rest |

`footprint(1)` reports far less (193 MB) than `ri_phys_footprint`
(the number Activity Monitor and jetsam use) because it only counts
dirty pages visible in the process's own regions, not pages held through
the hypervisor's stage-2 mapping. vmuc reports `ri_phys_footprint`.

**Verdict.**

1. Report from order 5. Same bulk-path speed (≥ 90 % of a 2 GiB free
   back in 6–9 s), about 220 MiB less at idle than order 9, and lower
   release latency. Set on the kernel command line by default.
2. The plan's idle target (< 350 MiB after the first reclaim) is not met:
   idle Ubuntu holds ~240 MiB itself and the guest kernel reserves ~95 MiB
   for a 4 GiB ceiling (struct pages scale with the ceiling: ~256 MiB at
   16 GiB). Further levers, each its own change: drop snapd from the
   template (its daemon and seeded snaps' mapped binaries), 16 KiB guest
   pages (experiment E3: a quarter of the struct-page cost), or growing the
   ceiling on demand instead of reserving it at boot.
3. The freed-memory target (≥ 90 % back within 10 s) is met.
