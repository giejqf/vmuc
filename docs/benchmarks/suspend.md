# Suspend and resume

Probe: `tests/bench/suspend/run.py [FILL] [MODE...]` (ADR 0007). An
Ubuntu instance (4 vCPUs, 4 GiB) boots ("Boot s": `vmuc start` until ssh
works, the reference), finishes cloud-init and writes FILL MiB of random
data to `/dev/shm`; "Footprint" is the VMM's then. `vmuc suspend` saves
it: seconds for the command, and in brackets the milliseconds the guest
was stopped (pause, devices quiesced, RAM image and disk flushed,
state committed); "ram.img MiB" is the image on disk. `vmuc start`
resumes it: seconds until ssh works, and in brackets milliseconds from
the VMM's start until the guest ran again. "warm" resumes with the image
in the page cache (just written), "cold" from a copy written past it, as
after a host reboot. Footprints in MiB, 2 s after the resume and after
the guest has read its data back (guest RAM is mapped copy-on-write
from the image, so reading bills nothing).

Rows from 5744a65 show resumes of 1.2 s with 1 GiB in use: the agent's
first call after a resume could be reset along with the guest's old
connections, and waited out its 1 s timeout (fixed in b064b1e; see
[log](../logs/2026-10-13-suspend-instances.md)). Suspending a guest that
was itself resumed takes longer than the first time (about 1.1 s
stopped with 1.5 GiB of image instead of 0.5 s): the pages it has not
touched since are read from the old image.

| Date | Host | Commit | Fill MiB | Mode | Boot s | Footprint | Suspend s (stopped ms) | ram.img MiB | Resume s (restored ms) | Footprint after | After reading |
|------|------|--------|---------:|------|-------:|----------:|-----------------------:|------------:|-----------------------:|----------------:|--------------:|
| 2026-10-13 | Apple M4 Pro, macOS 26.6.2 | 5744a65 | 0 | warm | 3.1 | 785 | 0.44 (355) | 770 | 0.14 (101) | 56 | 76 |
| 2026-10-13 | Apple M4 Pro, macOS 26.6.2 | 5744a65 | 0 | cold | 2.5 | 515 | 0.32 (270) | 478 | 0.35 (91) | 49 | 61 |
| 2026-10-13 | Apple M4 Pro, macOS 26.6.2 | 5744a65 | 1024 | warm | 2.9 | 1759 | 0.92 (803) | 1756 | 1.22 (100) | 48 | 84 |
| 2026-10-13 | Apple M4 Pro, macOS 26.6.2 | 5744a65 | 1024 | cold | 2.4 | 1494 | 0.66 (593) | 1494 | 1.25 (101) | 49 | 80 |
| 2026-10-13 | Apple M4 Pro, macOS 26.6.2 | b064b1e | 0 | warm | 3.2 | 793 | 0.45 (358) | 779 | 0.13 (103) | 57 | 76 |
| 2026-10-13 | Apple M4 Pro, macOS 26.6.2 | b064b1e | 0 | cold | 2.2 | 510 | 0.32 (270) | 476 | 0.37 (97) | 52 | 66 |
| 2026-10-13 | Apple M4 Pro, macOS 26.6.2 | b064b1e | 1024 | warm | 3.1 | 1761 | 0.70 (597) | 1755 | 0.17 (96) | 49 | 85 |
| 2026-10-13 | Apple M4 Pro, macOS 26.6.2 | b064b1e | 1024 | cold | 2.6 | 1508 | 0.73 (654) | 1540 | 0.40 (102) | 49 | 80 |
| 2026-10-13 | Apple M4 Pro, macOS 26.6.2 | 92e7d24 | 0 | warm | 2.4 | 793 | 0.48 (391) | 747 | 0.14 (101) | 58 | 78 |
| 2026-10-13 | Apple M4 Pro, macOS 26.6.2 | 92e7d24 | 1024 | warm | 3.0 | 1780 | 0.75 (645) | 1734 | 0.19 (100) | 49 | 84 |
