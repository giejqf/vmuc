# virtiofs

Probe: `tests/bench/fs/run.sh` (workload in `tests/bench/fs/workload.py`).
One throwaway Ubuntu 24.04 instance, 4 vCPUs, 4 GiB, a writable share of
a directory under `target/`. The same workload runs on the share, on the
guest's own virtio-blk ext4 disk (`/var/tmp`) and on the host itself.

- Write: 1 GiB in 1 MiB writes, then fsync. Read: the same file, guest
  page cache dropped first (the host's stays warm).
- Small files: 5000 × 4 KiB in 50 directories: create, then stat every
  one and read every one with caches dropped, then unlink.
- Open+close: 20,000 times on one file, mean µs (on virtiofs an OPEN and
  a RELEASE round trip each).

Target: at least VZ virtiofs on the same host (not yet measured: Lima is
not installed on the benchmark Mac).

| Date | Host | Commit | Where | Write MB/s | Read MB/s | Create 5k ms | Stat 5k ms | Read 5k ms | Unlink 5k ms | Open+close µs |
|------|------|--------|-------|-----------:|----------:|-------------:|-----------:|-----------:|-------------:|--------------:|
| 2026-10-12 | Apple M4 Pro, macOS 26.6.2 | 51df71c | virtiofs | 2934 | 3390 | 1651 | 68 | 1266 | 815 | — |
| 2026-10-12 | Apple M4 Pro, macOS 26.6.2 | 51df71c | guest virtio-blk ext4 | 4406 | 9801 | 34 | 11 | 190 | 18 | — |
| 2026-10-12 | Apple M4 Pro, macOS 26.6.2 | 51df71c | host APFS (cache warm) | 5554 | 20985 | 285 | 13 | 93 | 189 | — |
| 2026-10-12 | Apple M4 Pro, macOS 26.6.2 | 1296538 | virtiofs | 3154 | 3555 | 1302 | 32 | 975 | 575 | 71.5 |
| 2026-10-12 | Apple M4 Pro, macOS 26.6.2 | 1296538 | guest virtio-blk ext4 | 2657 | 11084 | 34 | 12 | 195 | 18 | 0.7 |
| 2026-10-12 | Apple M4 Pro, macOS 26.6.2 | 1296538 | host APFS (cache warm) | 5723 | 21201 | 356 | 13 | 89 | 180 | 9.6 |
| 2026-10-12 | Apple M4 Pro, macOS 26.6.2 | Lima 2.2.1 vz | Lima vz virtiofs | 2728 | 4519 | 1076 | 234 | 957 | 434 | 72.3 |
| 2026-10-12 | Apple M4 Pro, macOS 26.6.2 | Lima 2.2.1 vz | Lima vz guest disk | 2365 | 11140 | 33 | 13 | 199 | 20 | 0.7 |
| 2026-10-12 | Apple M4 Pro, macOS 26.6.2 | c107432 | virtiofs | 3171 | 3811 | 1286 | 29 | 1002 | 603 | 77.7 |
| 2026-10-12 | Apple M4 Pro, macOS 26.6.2 | c107432 | guest virtio-blk ext4 | 2795 | 11512 | 35 | 13 | 168 | 18 | 0.7 |
| 2026-10-12 | Apple M4 Pro, macOS 26.6.2 | c107432 | host APFS (cache warm) | 5372 | 21622 | 321 | 14 | 94 | 256 | 9.9 |
| 2026-10-13 | Apple M4 Pro, macOS 26.6.2 | ea89cec | virtiofs | 3007 | 3810 | 1284 | 30 | 1002 | 571 | 73.9 |
| 2026-10-13 | Apple M4 Pro, macOS 26.6.2 | ea89cec | guest virtio-blk ext4 | 2769 | 12271 | 34 | 12 | 176 | 18 | 0.7 |
| 2026-10-13 | Apple M4 Pro, macOS 26.6.2 | ea89cec | host APFS (cache warm) | 5670 | 21543 | 284 | 13 | 93 | 185 | 9.7 |
| 2026-10-13 | Apple M4 Pro, macOS 26.6.2 | ea89cec | virtiofs DAX | 3583 | 6733 | 1252 | 31 | 1115 | 656 | 75.0 |
| 2026-10-13 | Apple M4 Pro, macOS 26.6.2 | 7d82ef4 | virtiofs DAX | 3606 | 5802 | 1315 | 31 | 1153 | 734 | 75.8 |
