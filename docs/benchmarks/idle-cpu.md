# idle-cpu

Host CPU used by an idle guest, as a percentage of one core, and the
VMM process's `phys_footprint` at the end of the sample. Target: < 0.5 %
with 4 vCPUs on an idle Ubuntu guest (ArcBox measured 6.7 % with 18 vCPUs
at HZ=1000).

Probe: `tests/bench/idle-cpu/run.sh busybox [CPUS]` or
`tests/bench/idle-cpu/run.sh instance NAME [CPUS]`. The guest settles for
the stated time after boot, then the process's CPU time is sampled. The
footprint before M5 includes everything first boot touched; nothing is
reclaimed yet.

| Date | Host | Guest | vCPUs | Settle | Sample | CPU % of one core | Footprint |
|------|------|-------|-------|--------|--------|-------------------|-----------|
| 2026-10-11 | Apple M4 Pro, macOS 26.6.2 | busybox | 4 | 5s | 30s | 0.17 | 131 MB |
| 2026-10-11 | Apple M4 Pro, macOS 26.6.2 | instance ubuntu | 4 | 60s | 30s | 0.33 | 867 MB |
| 2026-10-12 | Apple M4 Pro, macOS 26.6.2 | Lima 2.2.1 vz (VZ + hostagent) | 4 | 60s | 30s | 0.60 | 802 MiB |
| 2026-10-13 | Apple M4 Pro, macOS 26.6.2 | instance memtest (kernel 1af90d185cee) | 4 | 60s | 30s | 0.27 | 888 MB |
| 2026-10-13 | Apple M4 Pro, macOS 26.6.2 | instance memtest (kernel 1af90d185cee) | 4 | 60s | 30s | 0.23 | 889 MB |
| 2026-10-13 | Apple M4 Pro, macOS 26.6.2 | instance memtest (kernel 090cd98c03a0) | 4 | 60s | 30s | 0.23 | 907 MB |
| 2026-10-13 | Apple M4 Pro, macOS 26.6.2 | instance memtest (kernel 090cd98c03a0) | 4 | 60s | 30s | 0.27 | 901 MB |
| 2026-10-13 | Apple M4 Pro, macOS 26.6.2 | instance ubuntu (kernel 090cd98c03a0) | 4 | 60s | 30s | 0.30 | 1274 MB |
| 2026-10-13 | Apple M4 Pro, macOS 26.6.2 | instance ubuntu (kernel 090cd98c03a0) | 4 | 60s | 30s | 0.33 | 1260 MB |
| 2026-10-13 | Apple M4 Pro, macOS 26.6.2 | instance ubuntu, mmap RAM for comparison (53321b7) | 4 | 60s | 30s | 0.37 | 1541 MB |
| 2026-10-13 | Apple M4 Pro, macOS 26.6.2 | instance p4 (kernel 2e58607f2620) | 4 | 60s | 30s | 0.23 | 564 MB |
| 2026-10-13 | Apple M4 Pro, macOS 26.6.2 | instance p4 (kernel 2e58607f2620) | 4 | 60s | 30s | 0.23 | 605 MB |
| 2026-10-13 | Apple M4 Pro, macOS 26.6.2 | instance p16 (kernel b7046fd6fd58) | 4 | 60s | 30s | 0.17 | 636 MB |
| 2026-10-13 | Apple M4 Pro, macOS 26.6.2 | instance p16 (kernel b7046fd6fd58) | 4 | 60s | 30s | 0.20 | 656 MB |
