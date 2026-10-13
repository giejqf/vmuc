# Reading files from a share: host memory

Probe: `tests/bench/fs-memory/run.sh` (`FS_DAX=1` for a DAX share). The
guest reads 2 GiB in 8 files of 256 MiB from a virtiofs share, then 2000
files of 64 KiB, then `vmuc reclaim` runs. Footprint is the VMM's
`phys_footprint` in MiB; guest cache growth is MemInfo Cached in MiB
after the large files. Ubuntu 24.04, 4 vCPUs, 4 GiB.

| Date | Host | Commit | Share | Footprint before | After 2 GiB read | Guest cache growth | 2 GiB read s | After small files | Small files s | After reclaim |
|------|------|--------|-------|-----------------:|-----------------:|-------------------:|-------------:|------------------:|--------------:|--------------:|
| 2026-10-13 | Apple M4 Pro, macOS 26.6.2 | ea89cec | no DAX | 313 | 2133 (+1820) | 1792 | 1.0 | 2232 | 0.6 | 287 |
| 2026-10-13 | Apple M4 Pro, macOS 26.6.2 | ea89cec | DAX | 307 | 307 (+0) | 0 | 3.0 | 308 | 0.6 | 299 |
| 2026-10-13 | Apple M4 Pro, macOS 26.6.2 | ea89cec | DAX, 4 GiB window (build changed for this run) | 362 | 363 (+1) | 0 | 2.7 | 365 | 0.6 | 356 |
| 2026-10-13 | Apple M4 Pro, macOS 26.6.2 | ea89cec | DAX, MADV_WILLNEED (build changed for this run) | 299 | 300 (+1) | 0 | 1.1 | 300 | 0.5 | 280 |
| 2026-10-13 | Apple M4 Pro, macOS 26.6.2 | 7d82ef4 | DAX | 322 | 323 (+1) | 0 | 1.1 | 324 | 0.5 | 311 |
| 2026-10-13 | Apple M4 Pro, macOS 26.6.2 | 7d82ef4 | DAX | 314 | 315 (+1) | 0 | 1.1 | 316 | 0.5 | 307 |
| 2026-10-13 | Apple M4 Pro, macOS 26.6.2 | 7d82ef4 | no DAX | 293 | 2321 (+2028) | 2048 | 1.0 | 2449 | 0.5 | 293 |
