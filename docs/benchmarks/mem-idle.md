# Idle footprint

Probe: `tests/bench/mem-idle/run.sh NAME [RUNS]`. What an idle guest costs
the host once reclaim has run: the instance settles 60 s after start,
then three rounds of `vmuc reclaim` and 15 s; the host footprint
(`phys_footprint` of the VMM, as `vmuc stat` reports it) after each
round, median over the runs. Guest used: MemTotal − MemFree after the
third round. Extra kernel parameters and stopped services are for
experiments.

Target: < 350 MiB.

| Date | Host | Instance | Extra cmdline | Stopped | Runs | After 1 (MiB) | After 2 (MiB) | After 3 (MiB) | Guest used (MiB) |
|------|------|----------|---------------|---------|-----:|--------------:|--------------:|--------------:|-----------------:|
| 2026-10-12 | Apple M4 Pro, macOS 26.6.2 | ubuntu | `-` | - | 2 | 475 | 462 | 460 | 220 |
| 2026-10-12 | Apple M4 Pro, macOS 26.6.2 | ubuntu | `swiotlb=noforce` | - | 2 | 394 | 380 | 375 | 207 |
| 2026-10-12 | Apple M4 Pro, macOS 26.6.2 | ubuntu | `-` | ModemManager udisks2 multipathd.socket multipathd unattended-upgrades | 2 | 436 | 436 | 436 | 114 |
| 2026-10-12 | Apple M4 Pro, macOS 26.6.2 | ubuntu | `swiotlb=noforce` | ModemManager udisks2 multipathd.socket multipathd unattended-upgrades | 2 | 364 | 342 | 370 | 124 |
| 2026-10-13 | Apple M4 Pro, macOS 26.6.2 | memtest | `(kernel 1af90d185cee)` | - | 3 | 374 | 355 | 355 | 185 |
| 2026-10-13 | Apple M4 Pro, macOS 26.6.2 | memtest | `(kernel 090cd98c03a0)` | - | 3 | 362 | 354 | 354 | 188 |
