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
| 2026-10-13 | Apple M4 Pro, macOS 26.6.2 | e1 | balloon on (d6ca13a) | anon | critical | 425 | 1642 / 2830 / 2858 | 1301 | 3 | 85 | 6 | 4.7 | 58073 | 0 | 7 bursts, slowest 0.6 s |
| 2026-10-13 | Apple M4 Pro, macOS 26.6.2 | e1 | balloon on (435a776) | anon | critical | 426 | 1650 / 2840 / 2843 | 1299 | 2 | 474 | 4 | 3.1 | 15307 | 0 | 7 bursts, slowest 0.9 s |
| 2026-10-13 | Apple M4 Pro, macOS 26.6.2 | e1 | balloon off (d6ca13a) | build | normal | 222 | 1769 / 2323 / 4115 | 0 | 0 | 3 | 379 | 0.9 | 55303 | 1 | build exit 2 in 219 s |
| 2026-10-13 | Apple M4 Pro, macOS 26.6.2 | e1 | balloon off (d6ca13a) | build | normal | 173 | 1162 / 1390 / 1407 | 0 | 0 | 2923 | 387 | 0.0 | 118 | 0 | build exit 0 in 170 s |
| 2026-10-13 | Apple M4 Pro, macOS 26.6.2 | e1 | balloon off (d6ca13a) | build | warn | 174 | 918 / 1042 / 1103 | 0 | 0 | 3093 | 387 | 0.0 | 360 | 0 | build exit 0 in 171 s |
| 2026-10-13 | Apple M4 Pro, macOS 26.6.2 | e1 | balloon on (d6ca13a) | build | critical | 178 | 1440 / 1798 / 1862 | 1940 | 1 | 1249 | 386 | 0.0 | 342 | 0 | build exit 0 in 176 s |
| 2026-10-13 | Apple M4 Pro, macOS 26.6.2 | e1 | balloon on (435a776) | build | critical | 178 | 1513 / 1950 / 2035 | 1983 | 1 | 1136 | 385 | 0.0 | 213 | 0 | build exit 0 in 174 s |
| 2026-10-13 | Apple M4 Pro, macOS 26.6.2 | e1 | balloon on (d6ca13a) | cache | normal | 301 | 3347 / 3375 / 3376 | 0 | 0 | 3762 | 101 | 0.0 | 38 | 0 | 10509 GiB read |
| 2026-10-13 | Apple M4 Pro, macOS 26.6.2 | e1 | balloon on (d6ca13a) | cache | warn | 301 | 3343 / 3375 / 3378 | 0 | 0 | 3750 | 101 | 0.0 | 109 | 0 | 10437 GiB read |
| 2026-10-13 | Apple M4 Pro, macOS 26.6.2 | e1 | balloon off (d6ca13a) | cache | critical | 301 | 3343 / 3374 / 3393 | 0 | 0 | 3744 | 101 | 0.0 | 111 | 0 | 10446 GiB read |
| 2026-10-13 | Apple M4 Pro, macOS 26.6.2 | e1 | balloon on (d6ca13a) | cache | critical | 302 | 3308 / 3359 / 3473 | 945 | 10 | 2817 | 112 | 13.0 | 2926 | 0 | 8721 GiB read |
| 2026-10-13 | Apple M4 Pro, macOS 26.6.2 | e1 | balloon on (d6ca13a) | cache | pulse | 721 | 3355 / 3454 / 3690 | 946 | 12 | 2812 | 106 | 14.8 | 3766 | 0 | 23109 GiB read |
| 2026-10-13 | Apple M4 Pro, macOS 26.6.2 | e1 | balloon on (435a776) | cache | warn | 301 | 3352 / 3395 / 3403 | 0 | 0 | 3706 | 101 | 0.2 | 79 | 0 | 10359 GiB read |
| 2026-10-13 | Apple M4 Pro, macOS 26.6.2 | e1 | balloon off (435a776) | cache | critical | 301 | 3351 / 3388 / 3401 | 0 | 0 | 3740 | 101 | 0.3 | 109 | 0 | 10452 GiB read |
| 2026-10-13 | Apple M4 Pro, macOS 26.6.2 | e1 | balloon on (435a776) | cache | critical | 301 | 3342 / 3389 / 3468 | 945 | 2 | 2829 | 103 | 2.7 | 716 | 0 | 9474 GiB read |
| 2026-10-13 | Apple M4 Pro, macOS 26.6.2 | e1 | balloon on (435a776) | cache | pulse | 721 | 3360 / 3407 / 3480 | 946 | 4 | 2833 | 102 | 5.8 | 1315 | 0 | 22578 GiB read |
| 2026-10-13 | Apple M4 Pro, macOS 26.6.2 | e1 | balloon off (d6ca13a) | cold | warn | 302 | 521 / 3340 / 3341 | 0 | 0 | 3689 | 1 | 0.0 | 82 | 0 | exit 0 |
| 2026-10-13 | Apple M4 Pro, macOS 26.6.2 | e1 | balloon off (435a776) | cold | warn | 302 | 1430 / 3339 / 3341 | 0 | 0 | 3762 | 1 | 0.0 | 46 | 0 | exit 0 |
| 2026-10-13 | Apple M4 Pro, macOS 26.6.2 | e1 | balloon on (d6ca13a) | idle | critical | 601 | 308 / 314 / 333 | 3552 | 1 | 254 | 1 | 0.0 | 96 | 0 | exit 0 |
