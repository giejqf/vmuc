# 2026-10-12 — Shared-memory rings between the VMM and the vmnet helper

**Question.** ADR 0001 moved frames between the VMM and the root helper
from datagrams on a socketpair to two shared-memory rings, for two copies
per frame instead of three or four and no syscall per frame. Does it
raise the ceiling the helper's relay thread put on throughput, and what
does it cost in CPU?

**Method.** Mac mini M4 Pro, macOS 26.6.2; Ubuntu 24.04 guest, 4 vCPUs,
vmnet shared mode with TSO and virtio-net headers in both runs; iperf3
from the host to the guest's address, 8 s per run
(`tests/bench/net-iperf/run.sh`, which also records whether the VMM
maps the ring region). CPU: user + system time of the VMM and the helper
over 4 s of a run (`ps`); VZ from `tests/bench/lima-vz/run.sh`'s
instance. Integrity: 512 MiB of random data each way over HTTP, SHA-256
compared.

**Results.** Gbit/s (`docs/benchmarks/net-iperf.md`):

| | host→guest | guest→host | host→guest ×4 | guest→host ×4 |
|---|---:|---:|---:|---:|
| socketpair | 52.7 | 63.6 | 49.3 | 77.8 |
| rings | 87.8 | 76.5 | 100.5 | 110.5 |
| Lima on VZ, VZ NAT | 71.9 | 108.1 | 80.3 | 138.3 |

CPU, % of one core:

| | host→guest | guest→host |
|---|---|---|
| socketpair: VMM + helper | 168 + 88 at 51.7 Gbit/s (5.0 per Gbit/s) | 131 + 102 at 62.9 (3.7) |
| rings: VMM + helper | 239 + 106 at 87.9 (3.9) | 232 + 98 at 72.1 (4.6) |
| VZ | 270 at 68.4 (3.9) | 289 at 102.7 (2.8) |

Both 512 MiB transfers matched; no retransmits in single-flow runs.

**Verdict.** Adopted. Host to guest gains 67 % (single flow) and 104 %
(four), now above VZ, at VZ's CPU per bit. Guest to host gains 20-42 %
but stays at 71-80 % of VZ: the helper's single relay thread is still at
~100 %, and most of that is `vmnet_write` itself, which runs the host's
network stack on the calling thread. Scaling that direction needs more
than one relay thread, so more than one queue pair (multi-queue
virtio-net), not fewer copies.

Two failures on the way, both fixed with tests: macOS rounds a shared
memory object up to whole 16 KiB pages, so the helper must accept a
region at least, not exactly, the size it maps; and a helper that exits
must be reported, since its stderr is no longer read after the hello.
