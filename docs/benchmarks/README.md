# Benchmarks against the v0.1 targets

Every number here has a probe in `tests/bench/` (or a test) and dated
rows in the page it links to; this page only summarises the latest. All
from one Mac mini M4 Pro, 24 GiB, macOS 26.6.2, Ubuntu 24.04 guest with 4
vCPUs unless noted.

| Metric | Target | vmuc | Lima 2.2.1 on VZ, same Mac | Status | Source |
|---|---|---|---|---|---|
| `vmuc start` → ssh prompt, warm cache | < 3 s | 3.25 s median (start returns at 3.04 s), with a vmnet NIC | 7.73 s | close | [boot-ssh](boot-ssh.md) |
| Kernel to `init` on the serial console | < 400 ms | 898 ms with the current kernel; 170 ms with the RAID6 boot benchmark skipped | — | met once the kernel with `CONFIG_RAID6_PQ_BENCHMARK=n` is built | [boot-serial](boot-serial.md) |
| Idle CPU, 4 vCPUs, Ubuntu idle | < 0.5 % | 0.33 % of one core | 0.60 % (VZ process + host agent) | met | [idle-cpu](idle-cpu.md) |
| Host footprint, idle guest after first reclaim | < 350 MiB | 467 MiB | 802 MiB idle; 2749 MiB once the guest has used and freed 2 GiB | not met (but well under VZ) | [mem-reclaim](mem-reclaim.md) |
| Freed guest memory back to the host | ≥ 90 % within 10 s | 77–100 % of 2 GiB within 10 s (90 % at 10.4 s in the latest run) | 0 %, even after the guest drops caches | borderline | [mem-reclaim](mem-reclaim.md) |
| Reclaim-induced vCPU stall | < 5 ms | +13 µs worst gap on an unrelated vCPU at saturation | — | met | [reclaim stall](../experiments/2026-10-11-reclaim-stall.md), `crates/vmuc-hv/tests/stall.rs` |
| Net single flow host↔guest | > 10 Gbit/s | 52.7 host→guest, 63.6 guest→host (vmnet with TSO, macOS 15.4+) | 71.9 / 108.1 (VZ NAT) | met; 59-73 % of VZ, see below | [net-iperf](net-iperf.md) |
| vsock RPC p50 | < 0.3 ms | 0.138 ms (p99 0.239 ms), new connection per call | — | met | [vsock-rpc](vsock-rpc.md) |
| virtiofs | ≥ VZ virtiofs on the same host | 3.2 GB/s write, 3.8 GB/s cold read, open+close 78 µs | 2.7 GB/s write, 4.5 GB/s cold read, open+close 72 µs | mixed: ahead on writes and cold stat (8×), behind on cold reads, create, unlink | [fs](fs.md) |

The Lima column comes from `tests/bench/lima-vz/run.sh`, which runs the
same measurements against a throwaway Lima instance (Ubuntu 24.04,
4 CPUs, 4 GiB, virtiofs, VZ NAT, containerd off) and appends its rows to
each page.

Re-run one with `make bench-local NAME=<probe>` on the Mac (or
`make bench NAME=` from the Linux box); each appends its row.

## Why the misses and the gaps

- **Footprint.** After reclaim an idle Ubuntu guest keeps ~490 MiB on
  the host: about 240 MiB is Ubuntu's own working set and about 95 MiB
  the guest kernel's reservations for a 4 GiB ceiling
  ([M5 log](../logs/2026-10-11-m5-memory.md)). Closing the gap is a
  snapd-free template and 16 KiB guest pages (experiment E3) more than
  VMM work.
- **Network against VZ.** VZ's device hands frames to the host in one
  hop through an entitlement vmuc cannot hold; vmuc's go through the root
  helper (two extra copies, one relay thread per direction at 88-100 %
  of a core) and use ~30 % more CPU per bit. Details and what would
  narrow it: [network log](../logs/2026-10-12-m6-network-on-vmnet.md).
- **Memory before reclaim.** 20 s after boot vmuc's guest holds 1352 MiB
  against VZ's 802 MiB, mostly page cache from boot; free page reporting
  only returns memory the guest frees, so it comes back on `vmuc reclaim`
  (467 MiB) and never on VZ.
- **Boot.** Most of the gap to 400 ms was the kernel benchmarking RAID6
  algorithms at boot; the config fix is committed and waits for the
  kernel workflow to build it.
