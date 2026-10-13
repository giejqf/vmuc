# Host pressure

Probe: `tests/bench/mem-pressure/run.sh NAME WORKLOAD LEVEL [SECONDS]`.
One guest workload while the VMM acts on a simulated macOS pressure
level (`vmuc _pressure`), after a 60 s settle and a reclaim. Footprint is
the VMM's `phys_footprint` sampled every second (mean / p95 / max);
balloon episodes count changes of direction; available is the guest's
lowest MemAvailable; CPU is the VMM's (vCPUs and devices), percent
of one core; stall is the guest's memory PSI `some` time during
the run; major faults and OOM kills from `/proc/vmstat`. Policy: whether
the instance lets the balloon inflate, and the commit. Ubuntu 24.04,
4 vCPUs, 4 GiB unless noted.

| Date | Host | Instance | Policy | Workload | Level | Seconds | Footprint MiB mean / p95 / max | Balloon max MiB | Balloon episodes | Available min MiB | CPU % | Stall s | Major faults | OOM kills | Result |
|------|------|----------|--------|----------|-------|---------|-------------------------------|-----------------|------------------|-------------------|-------|---------|--------------|-----------|--------|
| 2026-10-13 | Apple M4 Pro, macOS 26.6.2 | e1 | balloon on (53321b7) | cache | normal | 301 | 6513 / 6558 / 6559 | 0 | 0 | 3743 | 100 | 0.0 | 38 | 0 | 9873 GiB read |
| 2026-10-13 | Apple M4 Pro, macOS 26.6.2 | e1 | balloon on (53321b7) | cache | warn | 301 | 6512 / 6568 / 6599 | 0 | 0 | 3745 | 101 | 0.0 | 99 | 0 | 10431 GiB read |
| 2026-10-13 | Apple M4 Pro, macOS 26.6.2 | e1 | balloon on (53321b7) | cache | critical | 301 | 6468 / 6551 / 7008 | 944 | 10 | 2830 | 112 | 12.6 | 3136 | 0 | 8673 GiB read |
| 2026-10-13 | Apple M4 Pro, macOS 26.6.2 | e1 | balloon off (53321b7) | cache | critical | 301 | 6512 / 6563 / 6580 | 0 | 0 | 3753 | 101 | 0.0 | 109 | 0 | 10209 GiB read |
