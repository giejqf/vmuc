# Snapshot and restore

Probe: `tests/bench/snapshot/run.py [FILL]`. A busybox guest (4 vCPUs,
2 GiB, the balloon) fills FILL MiB with random data and ticks every
100 ms; `vmuc _boot` snapshots it 8 s in (milliseconds to the snapshot
written, and in brackets until every vCPU had stopped), then restores it
in a new process with guest RAM copied in or mapped copy-on-write. The
guest then frees the data, writes it again and frees that. Footprints
are the VMM's `phys_footprint` in MiB: the booted VM just before the
snapshot, then the restored one at its first tick, before the free, at
the lowest within 5 s after it, refilled, and at the lowest after the
second free. "Checks": the next tick follows, uptime continues, every
CPU works again, virtio-rng answers. The snapshot is in the page cache
(just written).

| Date | Host | Commit | Fill MiB | ram.img on disk MiB | Snapshot ms (vCPUs) | RAM | Restored ms | First tick ms | Booted | Start | Before free | After free | Refilled | After free 2 | Checks |
|------|------|--------|---------:|--------------------:|--------------------:|-----|------------:|--------------:|-------:|------:|------------:|-----------:|---------:|-------------:|--------|
| 2026-10-13 | Apple M4 Pro, macOS 26.6.2 | ae4a021 | 512 | 603 | 196 (0.32) | copy | 72 | 79 | 642 | 639 | 640 | 138 | 644 | 140 | ok |
| 2026-10-13 | Apple M4 Pro, macOS 26.6.2 | ae4a021 | 512 | 603 | 196 (0.32) | map | 37 | 48 | 642 | 40 | 45 | 55 | 577 | 77 | ok |
| 2026-10-13 | Apple M4 Pro, macOS 26.6.2 | ae4a021 | 512 | 603 | 199 (0.27) | copy | 72 | 80 | 642 | 639 | 640 | 138 | 642 | 142 | ok |
| 2026-10-13 | Apple M4 Pro, macOS 26.6.2 | ae4a021 | 512 | 603 | 199 (0.27) | map | 41 | 53 | 642 | 40 | 44 | 53 | 577 | 86 | ok |
| 2026-10-13 | Apple M4 Pro, macOS 26.6.2 | ae4a021 | 0 | 89 | 82 (0.27) | copy | 37 | 134 | 131 | 125 | 126 | 126 | 128 | 128 | ok |
| 2026-10-13 | Apple M4 Pro, macOS 26.6.2 | ae4a021 | 0 | 89 | 82 (0.27) | map | 38 | 130 | 131 | 40 | 44 | 44 | 47 | 47 | ok |
| 2026-10-13 | Apple M4 Pro, macOS 26.6.2 | ae4a021 | 1024 | 1079 | 352 (0.24) | copy | 113 | 197 | 1119 | 1116 | 1117 | 139 | 1123 | 146 | ok |
| 2026-10-13 | Apple M4 Pro, macOS 26.6.2 | ae4a021 | 1024 | 1079 | 352 (0.24) | map | 39 | 130 | 1119 | 40 | 45 | 62 | 1068 | 114 | ok |
| 2026-10-13 | Apple M4 Pro, macOS 26.6.2 | ae4a021 | 1024 | 1079 | 352 (0.24) | copy (cold) | 231 | 315 | 1119 | 1132 | 1133 | 151 | 1126 | 149 | ok |
| 2026-10-13 | Apple M4 Pro, macOS 26.6.2 | ae4a021 | 1024 | 1079 | 352 (0.24) | map (cold) | 31 | 143 | 1119 | 39 | 45 | 63 | 1067 | 97 | ok |
