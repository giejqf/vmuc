# Benchmarks against the v0.1 targets

Every number here has a probe in `tests/bench/` (or a test) and dated
rows in the page it links to; this page only summarises the latest. All
from one Mac mini M4 Pro, 24 GiB, macOS 26.6.2, Ubuntu 24.04 guest with 4
vCPUs unless noted.

| Metric | Target | Latest | Status | Source |
|---|---|---|---|---|
| `vmuc start` → ssh prompt, warm cache | < 3 s | 3.17 s median (`start` itself returns at 2.76 s) | close; the CI kernel predates the RAID6-benchmark fix below | [boot-ssh](boot-ssh.md) |
| Kernel to `init` on the serial console | < 400 ms | 898 ms with the current kernel; 170 ms with the RAID6 boot benchmark skipped | met once the kernel with `CONFIG_RAID6_PQ_BENCHMARK=n` is built | [boot-serial](boot-serial.md) |
| Idle CPU, 4 vCPUs, Ubuntu idle | < 0.5 % | 0.33 % of one core | met | [idle-cpu](idle-cpu.md) |
| Host footprint, idle guest after first reclaim | < 350 MiB | ~490 MiB | not met | [mem-reclaim](mem-reclaim.md) |
| Freed guest memory back to the host | ≥ 90 % within 10 s | 87–100 % of 2 GiB within 10 s | met in 2 of 3 runs | [mem-reclaim](mem-reclaim.md) |
| Reclaim-induced vCPU stall | < 5 ms | +13 µs worst gap on an unrelated vCPU at saturation | met | [reclaim stall](../experiments/2026-10-11-reclaim-stall.md), `crates/vmuc-hv/tests/stall.rs` |
| Net single flow host↔guest | > 10 Gbit/s | not measured: needs the root vmnet helper installed | open | — |
| vsock RPC p50 | < 0.3 ms | not measured as such; a virtio round trip is ~28 µs | open | [round trip](../experiments/2026-10-12-virtio-round-trip.md) |
| virtiofs | ≥ VZ virtiofs on the same host | 3.2 GB/s write, 3.6 GB/s cold read, open+close 72 µs | VZ reference not measured yet (Lima not installed) | [fs](fs.md) |

Re-run one with `make bench-local NAME=<probe>` on the Mac (or
`make bench NAME=` from the Linux box); each appends its row.

## Why the misses miss

- **Footprint.** After reclaim an idle Ubuntu guest keeps ~490 MiB on
  the host: about 240 MiB is Ubuntu's own working set and about 95 MiB
  the guest kernel's reservations for a 4 GiB ceiling
  ([M5 log](../logs/2026-10-11-m5-memory.md)). Closing the gap is a
  snapd-free template and 16 KiB guest pages (experiment E3) more than
  VMM work.
- **Boot.** Most of the gap to 400 ms was the kernel benchmarking RAID6
  algorithms at boot; the config fix is committed and waits for the
  kernel workflow to build it.
