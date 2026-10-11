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
