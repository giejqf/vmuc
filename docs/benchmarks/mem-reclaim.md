# mem-reclaim

lima#2789 on vmuc: after boot the guest settles for 20 s, fills 2 GiB of
tmpfs, deletes it, and the VMM's `phys_footprint` is sampled every
0.5 s; then `vmuc reclaim` drops the guest's caches and compacts, and the
footprint is read 10 s later. Targets: ≥ 90 % of the freed 2 GiB back
within 10 s; idle footprint after the first reclaim < 350 MiB.

Probe: `tests/bench/mem-reclaim/run.sh NAME` (extra kernel parameters
through `VMUC_KERNEL_CMDLINE`). Release latency is per reported range,
bucket upper bounds. Rows before the last two runs of order 5 kept a
31 MiB heap copy of the kernel image (fixed by 759a556). Releases arrive
in ~2 s batches (the guest's fixed reporting delay), so "back in 10 s"
moves by a batch from run to run.

| Date | Host | Instance | Extra cmdline | Settled (MiB) | Filled (MiB) | Back in 10 s (%) | 90 % back after (s) | Idle after reclaim (MiB) | Release p50 / p99 (µs) |
|------|------|----------|---------------|---------------|--------------|------------------|---------------------|--------------------------|------------------------|
| 2026-10-11 | Apple M4 Pro, macOS 26.6.2 | ubuntu | `-` (order 9, the kernel default before d84d59c) | 961 | 3061 | 100 | 6.8 | 728 | 50 / 2500 |
| 2026-10-11 | Apple M4 Pro, macOS 26.6.2 | ubuntu | `page_reporting.page_reporting_order=5` | 983 | 3030 | 99 | 8.4 | 510 | 50 / 500 |
| 2026-10-11 | Apple M4 Pro, macOS 26.6.2 | ubuntu | `page_reporting.page_reporting_order=5` | 992 | 3082 | 100 | 6.2 | 503 | 25 / 1000 |
| 2026-10-11 | Apple M4 Pro, macOS 26.6.2 | ubuntu | `page_reporting.page_reporting_order=3` | 1010 | 3055 | 95 | 8.3 | 481 | 25 / 250 |
| 2026-10-11 | Apple M4 Pro, macOS 26.6.2 | ubuntu | `page_reporting.page_reporting_order=5` | 1040 | 3100 | 100 | 6.8 | 497 | 50 / 2500 |
| 2026-10-11 | Apple M4 Pro, macOS 26.6.2 | ubuntu | `page_reporting.page_reporting_order=5` | 1037 | 3117 | 87 | 10.4 | 487 | 50 / 1000 |
| 2026-10-11 | Apple M4 Pro, macOS 26.6.2 | ubuntu | `-` | 1049 | 3130 | 100 | 9.4 | 494 | 50 / 250 |
