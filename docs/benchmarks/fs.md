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
| 2026-10-12 | Apple M4 Pro, macOS 26.6.2 | 0286027 | virtiofs | 2934 | 3390 | 1651 | 68 | 1266 | 815 | — |
| 2026-10-12 | Apple M4 Pro, macOS 26.6.2 | 0286027 | guest virtio-blk ext4 | 4406 | 9801 | 34 | 11 | 190 | 18 | — |
| 2026-10-12 | Apple M4 Pro, macOS 26.6.2 | 0286027 | host APFS (cache warm) | 5554 | 20985 | 285 | 13 | 93 | 189 | — |
| 2026-10-12 | Apple M4 Pro, macOS 26.6.2 | 54a2e68 | virtiofs | 3154 | 3555 | 1302 | 32 | 975 | 575 | 71.5 |
| 2026-10-12 | Apple M4 Pro, macOS 26.6.2 | 54a2e68 | guest virtio-blk ext4 | 2657 | 11084 | 34 | 12 | 195 | 18 | 0.7 |
| 2026-10-12 | Apple M4 Pro, macOS 26.6.2 | 54a2e68 | host APFS (cache warm) | 5723 | 21201 | 356 | 13 | 89 | 180 | 9.6 |
