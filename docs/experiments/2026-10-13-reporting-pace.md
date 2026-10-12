# 2026-10-13 — How fast freed guest memory reaches the host

**Question.** The v0.1 target is 90 % of freed guest memory back on the
host within 10 s. On the stock kernel the recorded runs took 6.2–10.4 s
to get 90 % of a freed 2 GiB back and two of nine missed. Is the limit
vmuc or the guest, and does reporting the whole free list per pass fix
it without costing anything at idle?

**Method.** Linux 6.18.55's `mm/page_reporting.c` read for the pacing:
a pass runs `PAGE_REPORTING_DELAY` (2 s) after the first free, and each
free list gets `DIV_ROUND_UP(nr_free, 32 * 16)` report calls of 32
blocks, plus one, before the pass requests another two seconds later.
The patched kernel
(`guest/kernel/patches/0001-mm-page_reporting-report-the-whole-free-list-per-pass.patch`,
the divisor without the 16) was built with `guest/kernel/build.sh`
inside a vmuc instance (10 vCPUs; sha256 `090cd98c03a0…`) and compared
with the CI kernel (`1af90d185cee…`) on one fresh instance from the
Ubuntu template (`memtest`: 4 vCPUs, 4 GiB, `swiotlb=noforce`, idle
daemons off), selected with `VMUC_KERNEL`:
`tests/bench/mem-reclaim/run.sh memtest` three times each,
`tests/bench/mem-idle/run.sh memtest 3` and
`tests/bench/idle-cpu/run.sh instance memtest` twice each. Rows in
[mem-reclaim](../benchmarks/mem-reclaim.md),
[mem-idle](../benchmarks/mem-idle.md) and
[idle-cpu](../benchmarks/idle-cpu.md). M4 Pro, macOS 26.6.2.

**Result.**

| Kernel | Back in 10 s | 90 % back after | Release p50 / p99 | Idle after 3 reclaims | Idle CPU (footprint, no reclaim) |
|---|---|---|---|---|---|
| CI (stock pacing) | 99 / 99 / 99 % | 6.8 / 8.3 / 8.3 s | 50 / 500–1000 µs | 355 MiB | 0.27 / 0.23 % (888 / 889 MB) |
| patched | 99 / 100 / 99 % | 2.1 / 1.0 / 0.5 s | 25 / 500 µs | 354 MiB | 0.23 / 0.27 % (907 / 901 MB) |

Freed tmpfs pages coalesce into 4 MiB blocks; with ~750 of them free in
the larger zone, upstream's budget is three calls of 32 blocks, 384 MiB
of that list per pass (the other zone's list has its own budget). That
is several two-second passes for 2 GiB, the 6–10 s of the stock rows.
Patched, the first pass reports everything that is free by then.

**Verdict.** The guest's pacing was the whole delay. vmuc's kernel
carries the patch
([ADR 0002](../adr/0002-guest-kernel-reports-everything-per-pass.md)):
90 % of a freed 2 GiB is back in about two seconds, with the release
latency no worse and idle CPU and the footprint after reclaim
unchanged (before any reclaim the patched runs sat ~16 MB higher, two
runs each, which these runs cannot tell from boot-to-boot variation). The
freed-memory target is met by a wide margin; it holds for vmuc's kernel,
not for an image that brings its own.
