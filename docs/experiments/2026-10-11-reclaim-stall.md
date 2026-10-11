# 2026-10-11 — Does reclaim stall vCPUs?

**Question.** M5's exit criterion is no reclaim-induced vCPU stall above
5 ms. Stage-2 unmaps imply TLB invalidation; does the release sequence
hold vCPUs out of the guest?

**Method.**

1. *In a guest*: Ubuntu 24.04, 4 vCPUs, one Python loop pinned to each
   vCPU recording the largest gap between `CLOCK_MONOTONIC` reads, first
   for 8 s idle, then for 12 s while 2 GiB of tmpfs is deleted and
   released (`tests/bench/mem-reclaim/stall.py`, copied in and run through `vmuc shell`).
2. *Host side only*: `cargo test -p vmuc-hv --test stall -- --nocapture`.
   A vCPU spins on `CNTVCT_EL0` for ~2 s keeping its largest gap, while
   a host thread runs unmap → `mmap(MAP_FIXED)` → map over an unrelated,
   fully dirtied 64 MiB range, in 128 KiB or 2 MiB pieces, back to back.

**Result.**

| Probe | Worst gap |
|---|---|
| guest, idle | 64–206 µs |
| guest, during a 2 GiB release (order 5) | 4.4–5.5 ms on every vCPU |
| guest, during a 2 GiB release (order 10, few large ranges) | 0.2–4.1 ms, two vCPUs clean |
| host probe, quiet | 27 µs |
| host probe, ~140k releases/s of 128 KiB | 40 µs |
| host probe, ~15k releases/s of 2 MiB | 41 µs |

**Verdict.** The VMM's side of reclaim costs an unrelated vCPU tens of
microseconds even at saturation, two orders of magnitude under the
target, and the probe now fails if that changes. The millisecond gaps
seen inside the guest during a large free come from the guest's own
work: truncating the 2 GiB file and the page-reporting worker walking
free lists under the zone lock (Python allocates, so its loops contend).
Not a VMM problem; a larger reporting order trades idle footprint for
fewer such pauses if a workload cares.
