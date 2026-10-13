# mem-reclaim

lima#2789 on vmuc: after boot the guest settles for 20 s, fills 2 GiB of
tmpfs, deletes it, and the VMM's `phys_footprint` is sampled every
0.5 s; then `vmuc reclaim` drops the guest's caches and compacts, and the
footprint is read 10 s later. Targets: ≥ 90 % of the freed 2 GiB back
within 10 s; idle footprint after the first reclaim < 350 MiB.

Probe: `tests/bench/mem-reclaim/run.sh NAME` (extra kernel parameters
through `VMUC_KERNEL_CMDLINE`). Release latency is per reported range,
bucket upper bounds. Rows before the last two runs of order 5 kept a
31 MiB heap copy of the kernel image (fixed by 1f58d63). Releases arrive
in ~2 s batches (the guest's fixed reporting delay), so "back in 10 s"
moves by a batch from run to run.

| Date | Host | Instance | Extra cmdline | Settled (MiB) | Filled (MiB) | Back in 10 s (%) | 90 % back after (s) | Idle after reclaim (MiB) | Release p50 / p99 (µs) |
|------|------|----------|---------------|---------------|--------------|------------------|---------------------|--------------------------|------------------------|
| 2026-10-11 | Apple M4 Pro, macOS 26.6.2 | ubuntu | `-` (order 9, the kernel default before f3e0e31) | 961 | 3061 | 100 | 6.8 | 728 | 50 / 2500 |
| 2026-10-11 | Apple M4 Pro, macOS 26.6.2 | ubuntu | `page_reporting.page_reporting_order=5` | 983 | 3030 | 99 | 8.4 | 510 | 50 / 500 |
| 2026-10-11 | Apple M4 Pro, macOS 26.6.2 | ubuntu | `page_reporting.page_reporting_order=5` | 992 | 3082 | 100 | 6.2 | 503 | 25 / 1000 |
| 2026-10-11 | Apple M4 Pro, macOS 26.6.2 | ubuntu | `page_reporting.page_reporting_order=3` | 1010 | 3055 | 95 | 8.3 | 481 | 25 / 250 |
| 2026-10-11 | Apple M4 Pro, macOS 26.6.2 | ubuntu | `page_reporting.page_reporting_order=5` | 1040 | 3100 | 100 | 6.8 | 497 | 50 / 2500 |
| 2026-10-11 | Apple M4 Pro, macOS 26.6.2 | ubuntu | `page_reporting.page_reporting_order=5` | 1037 | 3117 | 87 | 10.4 | 487 | 50 / 1000 |
| 2026-10-11 | Apple M4 Pro, macOS 26.6.2 | ubuntu | `-` | 1049 | 3130 | 100 | 9.4 | 494 | 50 / 250 |
| 2026-10-12 | Apple M4 Pro, macOS 26.6.2 | Lima 2.2.1 vz | `-` | 802 | 2749 | 0 | >20 | 2749 | - |
| 2026-10-12 | Apple M4 Pro, macOS 26.6.2 | ubuntu | `-` | 1352 | 3372 | 77 | 10.4 | 467 | 50 / 1000 |
| 2026-10-13 | Apple M4 Pro, macOS 26.6.2 | memtest | `(kernel 1af90d185cee)` | 689 | 2916 | 99 | 6.8 | 384 | 50 / 500 |
| 2026-10-13 | Apple M4 Pro, macOS 26.6.2 | memtest | `(kernel 1af90d185cee)` | 708 | 2734 | 99 | 8.3 | 394 | 50 / 1000 |
| 2026-10-13 | Apple M4 Pro, macOS 26.6.2 | memtest | `(kernel 1af90d185cee)` | 732 | 2803 | 99 | 8.3 | 426 | 50 / 500 |
| 2026-10-13 | Apple M4 Pro, macOS 26.6.2 | memtest | `(kernel 090cd98c03a0)` | 739 | 2774 | 99 | 2.1 | 370 | 25 / 500 |
| 2026-10-13 | Apple M4 Pro, macOS 26.6.2 | memtest | `(kernel 090cd98c03a0)` | 757 | 2829 | 100 | 1.0 | 403 | 25 / 500 |
| 2026-10-13 | Apple M4 Pro, macOS 26.6.2 | memtest | `(kernel 090cd98c03a0)` | 762 | 2799 | 99 | 0.5 | 375 | 25 / 500 |
| 2026-10-13 | Apple M4 Pro, macOS 26.6.2 | ubuntu | `(kernel 090cd98c03a0)` | 1243 | 3274 | 99 | 2.1 | 295 | 250 / 1000 |
| 2026-10-13 | Apple M4 Pro, macOS 26.6.2 | ubuntu | `(kernel 090cd98c03a0)` | 1245 | 3284 | 99 | 1.5 | 293 | 250 / 1000 |
| 2026-10-13 | Apple M4 Pro, macOS 26.6.2 | ubuntu | `(kernel 090cd98c03a0)` | 1237 | 3272 | 99 | 1.0 | 281 | 250 / 1000 |
| 2026-10-13 | Apple M4 Pro, macOS 26.6.2 | p4 | `(kernel 2e58607f2620)` | 481 | 2510 | 99 | 2.1 | 278 | 100 / 500 |
| 2026-10-13 | Apple M4 Pro, macOS 26.6.2 | p4 | `(kernel 2e58607f2620)` | 477 | 2512 | 100 | 0.5 | 265 | 100 / 500 |
| 2026-10-13 | Apple M4 Pro, macOS 26.6.2 | p16 | `page_reporting.page_reporting_order=3 (kernel b7046fd6fd58)` | 520 | 2571 | 97 | 0.5 | 332 | 50 / 2500 |
| 2026-10-13 | Apple M4 Pro, macOS 26.6.2 | p16 | `page_reporting.page_reporting_order=3 (kernel b7046fd6fd58)` | 530 | 2570 | 97 | 1.0 | 340 | 50 / 2500 |
