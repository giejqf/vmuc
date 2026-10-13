# 2026-10-13 — E6: which holes APFS keeps

**Question.** The GitHub macOS runners report full allocation
(`st_blocks`) for files that vmuc writes with holes (it seeks over runs
of zeros), where Linux reports only the data; three allocation
assertions in vmuc-image ran on Linux only. Does APFS really allocate
those holes, or only count them? Does `F_PUNCHHOLE` change it? Disk
images, decompressed images and, since ADR 0007, RAM images are written
this way.

**Method.** `tests/bench/apfs-sparse/run.py` on the Mac's own APFS data
volume: files written with holes of various sizes, in various orders,
flushed (`fsync` + `F_FULLFSYNC`); for each, `st_blocks`, the data
extents `SEEK_DATA`/`SEEK_HOLE` report, and for the large ones how far
the volume's free space fell. Then the same count of all-zero 64 KiB
runs inside a freshly converted Ubuntu disk image, and the image's
allocation through a guest workload. M4 Pro, macOS 26.6.2.

**Result.**

| Case | Before the flush | After it |
|---|---:|---:|
| 4 KiB in the middle of 1, 4, 16 MiB | 4 KiB | the whole file |
| … of 32 MiB | 4 KiB | 16 MiB (the second half) |
| … of 64 MiB, 1 GiB | 4 KiB | 16 KiB |
| 4 KiB writes after gaps 64K…16M, then 17M, 32M (one flush) | | gaps up to 16 MiB allocated, 17 and 32 MiB kept |
| 4 KiB at 0, flushed, then 4 KiB at 8 MiB, flushed | | [0, 8 MiB + 16 KiB) allocated |
| 1 MiB punched in written data, then 4 KiB written beside it and in it | | the hole stays (but for the 4 KiB) |

128 × 4 KiB 8 MiB apart over 1 GiB (512 KiB of data):

| Written | Allocated | Volume free space down |
|---|---:|---:|
| front to back | 1024 MiB | 1023 MiB |
| back to front | 1024 MiB | 1024 MiB |
| front to back, then the gaps punched | 2 MiB | 2 MiB |
| the last first and flushed, then the rest | 10 MiB | 10 MiB |

1. APFS really allocates: the volume loses the space. When it flushes a
   file, data written up to 16 MiB past the file's allocated end is
   joined to it by allocated zeros; a hole further away stays a hole.
   The order of the writes within one flush makes no difference (the
   flush goes front to back); a write below the allocated end, into a
   hole, fills nothing. Allocation is in 16 KiB units.
2. `F_PUNCHHOLE` on a flushed file deallocates for good: the hole stays
   when its neighbours or its middle are written later.
3. So the runners were right, and a small test file (≤ 16 MiB) is fully
   allocated by any writer that skips zeros.

What it cost vmuc: a converted Ubuntu cloud image had 1947 MiB
allocated of which 55 MiB were all-zero runs the converter had skipped
(3 %); a 4 GiB guest's RAM image, 745 MiB on disk for 717 MiB of pages.
A running guest's writes do not hit it: with the disk at 1929 MiB after
first boot, writing 512 MiB added 512 MiB, 200 files of 256 KiB added
50 MiB, and deleting them with `fstrim` brought it back to 1925 MiB
(discards punch holes).

**Verdict.**

1. Every writer that leaves holes (qcow2 conversion, decompression,
   raw copy, RAM images) records them and punches them once the file
   is flushed (`SparseFileExt::settle_holes`). After that the converted
   Ubuntu image is 1891 MiB with no all-zero runs, and a RAM image
   holds only its pages (747 MiB for an idle guest, was 779); a suspend
   takes 30–50 ms longer for it ([suspend](../benchmarks/suspend.md)).
2. The allocation assertions run on macOS as well.
3. The guest disk needs nothing more: discard already punches, and
   guest writes showed no fill.
