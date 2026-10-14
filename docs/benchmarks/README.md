# Benchmarks against the v0.1 targets

Every number here has a probe in `tests/bench/` (or a test) and dated
rows in the page it links to; this page only summarises the latest. All
from one Mac mini M4 Pro, 24 GiB, macOS 26.6.2, Ubuntu 24.04 guest with 4
vCPUs unless noted.

| Metric | Target | vmuc | Lima 2.2.1 on VZ, same Mac | Status | Source |
|---|---|---|---|---|---|
| `vmuc start` → ssh prompt, warm cache | < 3 s | 2.58 s median (start returns at 2.35 s), with a vmnet NIC | 7.73 s | met | [boot-ssh](boot-ssh.md) |
| Kernel to `init` on the serial console | < 400 ms | 120 ms median, kernel built without the RAID6 boot benchmark | — | met | [boot-serial](boot-serial.md) |
| Idle CPU, 4 vCPUs, Ubuntu idle | < 0.5 % | 0.23–0.27 % of one core | 0.60 % (VZ process + host agent) | met | [idle-cpu](idle-cpu.md) |
| Host footprint, idle guest after first reclaim | < 350 MiB | 362–374 MiB after the first reclaim, 355 after the third (was 460–475) | 802 MiB idle; 2749 MiB once the guest has used and freed 2 GiB | not met, by 5–25 MiB (well under VZ) | [mem-idle](mem-idle.md) |
| Freed guest memory back to the host | ≥ 90 % within 10 s | 99–100 % of 2 GiB within 10 s, 90 % after 0.5–2.1 s (kernel patch, ADR 0002; stock pacing 6.2–10.4 s) | 0 %, even after the guest drops caches | met | [mem-reclaim](mem-reclaim.md) |
| Reclaim-induced vCPU stall | < 5 ms | +13 µs worst gap on an unrelated vCPU at saturation | — | met | [reclaim stall](../experiments/2026-10-11-reclaim-stall.md), `crates/vmuc-hv/tests/stall.rs` |
| Net single flow host↔guest | > 10 Gbit/s | 87.3 host→guest, 136.4 guest→host (vmnet with TSO, shared-memory rings); four flows 102.6 / 142.9 | 71.9 / 108.1 (VZ NAT); four flows 80.3 / 138.3 | met; above VZ both ways, one flow and four; costs more CPU than VZ at high rates | [net-iperf](net-iperf.md), [net-cost](net-cost.md) |
| vsock RPC p50 | < 0.3 ms | 0.138 ms (p99 0.239 ms), new connection per call | — | met | [vsock-rpc](vsock-rpc.md) |
| virtiofs | ≥ VZ virtiofs on the same host | 3.0–3.2 GB/s write, 3.8 GB/s cold read, open+close 74–78 µs; with DAX (opt-in, `mounts[].dax`): 3.6 GB/s write, 5.8 GB/s read, and reading costs the VM no memory | 2.7 GB/s write, 4.5 GB/s cold read, open+close 72 µs | mixed: ahead on writes and cold stat (8×), behind on cold reads (ahead with DAX), create, unlink | [fs](fs.md), [fs-memory](fs-memory.md) |

Beyond the targets: an instance suspended with `vmuc suspend` is back
at an ssh prompt 0.13–0.17 s after `vmuc start` (0.37–0.40 s with its
RAM image out of the page cache), against 2.2–3.2 s to boot, and costs
about 50 MiB right after; a suspended one costs none
([suspend](suspend.md), ADR 0007).

The Lima column comes from `tests/bench/lima-vz/run.sh`, which runs the
same measurements against a throwaway Lima instance (Ubuntu 24.04,
4 CPUs, 4 GiB, virtiofs, VZ NAT, containerd off) and appends its rows to
each page.

Re-run one with `make bench-local NAME=<probe>` on the Mac (or
`make bench NAME=` from the Linux box); each appends its row.

## Why the misses and the gaps

- **Footprint.** Booting without the unused swiotlb and disabling the
  modem and disk daemons took an idle guest from 460 to 355 MiB. What is
  left is Ubuntu's working set (~120 MiB), the guest kernel's
  reservations for a 4 GiB ceiling (~105 MiB, 64 of it struct pages),
  the VMM (~17 MiB) and memory that 4 KiB guest pages keep on 16 KiB host
  pages. The levers left give something up: unattended-upgrades'
  shutdown guard, the guest allocator's per-CPU lists, or 16 KiB guest
  pages, a locked decision
  ([idle-footprint experiment](../experiments/2026-10-12-idle-footprint.md)).
- **Network against VZ.** Above VZ both ways: the shared rings
  (ADR 0001) lifted host to guest, watching a busy transmit queue
  instead of taking a kick per frame lifted one flow from the guest from
  76.5 to 134.9 Gbit/s
  ([transmit kicks](../experiments/2026-10-14-net-transmit-kicks.md)),
  and filling one half of the helper's ring while it drains the other
  lifted four flows from 117.9 to 142.9
  ([four flows](../experiments/2026-10-14-net-four-flows-and-cost.md)).
  The cost is the gap left: about what VZ spends up to 1 Gbit/s, about
  40 % of a core more at 10 Gbit/s, most of it the helper process
  boundary, the rest on the vCPU taking interrupts.
- **Memory before reclaim.** 20 s after boot vmuc's guest holds 1352 MiB
  against VZ's 802 MiB, mostly page cache from boot; free page reporting
  only returns memory the guest frees, so it comes back on `vmuc reclaim`
  (355 MiB) and never on VZ.
