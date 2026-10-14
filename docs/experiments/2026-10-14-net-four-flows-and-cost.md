# 2026-10-14 — Four flows from the guest, and what traffic costs against VZ

**Questions.** After the transmit watch
([transmit kicks](2026-10-14-net-transmit-kicks.md)) one flow from the
guest to the host ran at 135 Gbit/s, above VZ, but four flows stayed at
114–118 against VZ's 138. Why, and is it the guest's single transmit
queue? And the VMM spent 111 % of a core at a paced 10 Gbit/s while the
guest's sender used 6 %: where does that go, and does VZ spend less?

**Method.** An Ubuntu 24.04 instance (4 vCPUs, 4 GiB) and a Lima 2.2.1
instance on VZ NAT of the same shape, on the same Mac. iperf3 `-R` from
the host; kernel-paced senders from `tests/bench/net-cost` (now with
`--lima NAME` for VZ). During runs: each VMM thread's CPU by name
(`proc_pidinfo`), the helper's, `kernel_task`'s (`top`); in the guest,
per-CPU time, interrupts, softirqs, NIC counters, `ss -ti` (RTT, window,
what limits each flow), the qdisc backlog. A throwaway VMM build counted
helper-ring events (ring full, device asleep, helper woken).

**Result: four flows.**

- Not the transmit queue: VZ's NIC has one queue pair too (`ethtool -l`)
  and moves 138 Gbit/s through it with four flows.
- Each of vmuc's four flows ran at 1.14 ms RTT against VZ's 0.53, its
  window (4 MiB, the Mac's receive buffer cap) full: 4 MiB per 1.14 ms is
  29 Gbit/s a flow. The extra RTT was queue: 4.2 MB waited in the guest's
  qdisc, its transmit queue restarted 6,000 times a second (`NET_TX`
  softirqs; VZ's guest: 1). Disabling BQL or changing the qdisc changed
  nothing, nor did interrupting the guest after every transmit batch.
- No thread was saturated (busiest VMM thread 46 %, helper 62 %, guest
  vCPUs 20–33 %), but the helper-bound ring went full 12,700 times a
  second (the device then asleep until a kick) and empty 3,500 times (the
  helper asleep): 14.4 GB/s over 3,500 cycles is 4 MB a cycle, the whole
  4 MiB ring. The helper takes everything published, up to 64 frames,
  into one `vmnet_write` and frees nothing until it returns, and the
  device published a whole pass of copied frames at once: while the
  helper wrote, the device had nowhere to copy, the guest's queue
  stopped, and when the helper let go there was nothing ready.
- Looking again for space instead of sleeping on a full ring did not
  help (115 Gbit/s); publishing only at the end of a pass kept the
  batches whole. The fix: the device publishes frame by frame while the
  helper holds less than half the ring and keeps the rest copied but
  unpublished until it lets go, so one half is written out while the
  other fills.

| | Guest → host, four flows | one flow | host → guest, one / four |
|---|---:|---:|---:|
| before (38eac04) | 117.9 | 134.9 | 86.1 / 105.6 |
| after (a959254) | **142.9** | 136.4 | 87.3 / 102.6 |
| Lima on VZ | 138.3 | 108.1 | 71.9 / 80.3 |

**Result: what it costs** ([net-cost](../benchmarks/net-cost.md), % of
one core; vmuc's VMM + helper against VZ's one process):

| Guest → host | vmuc | VZ |
|---|---|---|
| as fast as one flow goes | 142 Gbit/s: 203 + 122 | 103 Gbit/s: 185 |
| 10 Gbit/s | 111 + 28 = 139 | 99 |
| 1 Gbit/s | 31 + 10 = 41 | 34 |
| 0.1 Gbit/s | 13 + 4 = 17 | 14 |
| idle | 0 | 1 |

At 10 Gbit/s both take one interrupt per frame each way (vmuc 36,700 a
second on its one line, VZ 19,000 + 19,000 on two MSI vectors), and both
guests kick about once per frame. Where vmuc spends more:

1. The helper: 28 %. Every frame crosses into the root helper, a copy
   into the ring and a wakeup each way; VZ calls vmnet in its own
   process, holding the restricted entitlement vmuc cannot (AGENTS.md).
2. The vCPU taking the interrupts: 50–54 % against VZ's 45 %. Part of
   it, by how much not measured, is that a virtio-mmio interrupt costs
   the guest two exits (it reads InterruptStatus and writes
   InterruptACK), where virtio-pci with MSI-X needs neither.

`kernel_task` ran at 40–56 % in every case, vmuc and VZ alike.

**Verdict.**

1. Adopted: half-ring publishing (`devices/net/rings.rs`). Four flows
   from the guest are above VZ now, one flow well above it.
2. At low rates vmuc costs about what VZ does; at 10 Gbit/s about 40 % of
   a core more, most of it the helper process boundary (28 %), the rest
   on the vCPU taking interrupts, virtio-mmio's exits likely among it. Removing either is a design change (an
   in-process vmnet is not available to vmuc; a virtio-pci transport
   with MSI would be a new transport), left for an ADR if it is wanted.
