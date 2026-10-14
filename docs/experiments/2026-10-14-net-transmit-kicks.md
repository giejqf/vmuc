# 2026-10-14 — Guest to host: kicks on the sending vCPU

**Question.** One TCP flow from the guest to the host reached 76.5 Gbit/s
against VZ's 108 ([net-iperf](../benchmarks/net-iperf.md)); the helper
rings experiment put it down to the helper's relay thread. What limits
it now, and can vmuc reach VZ?

**Method.** An Ubuntu 24.04 instance (4 vCPUs, 4 GiB) on vmnet's shared
network, iperf3 from the host with `-R` (the guest sends), 10 s
(`tests/bench/net-iperf` for the table). During
the run: per-thread CPU of the VMM, the helper and the host's iperf3
(`ps -M`), `mpstat -P ALL` and `ethtool -S eth0` (virtio-net's kick
counters) and `/proc/interrupts` in the guest, iperf3's own CPU report
for the sender. Then the change below, and `tests/bench/net-cost`
(rates paced by the guest kernel) for what it costs.

**Result.**

Before the change, guest to host at 77 Gbit/s:

- The guest's sender was the bottleneck: iperf3 at 97 % of one vCPU,
  almost all of it kernel time, with vCPU 0 at 44 % handling the NIC's
  interrupts (38,700 a second). The busiest VMM thread (96 %) was that
  vCPU; the helper's relay thread was at 68 %, not at its ceiling
  (`vmnet_write` had been the limit before the rings).
- TSO worked (frames averaged 52 KiB), but the guest kicked the device
  for 0.4 of them: 77,000 kicks a second, each an MMIO write and so a VM
  exit on the sending vCPU. The device copied each frame into the ring
  faster than the guest produced the next, found the queue empty and
  asked to be kicked again.
- Free page reporting was not involved (6 MiB released in the run).

The change: after a frame that comes within 50 µs of the previous one,
the device watches the transmit queue for 50 µs before asking for kicks
again; a stray frame asks at once.

| Guest → host | Before (48ff8bc) | After (38eac04) | VZ |
|---|---:|---:|---:|
| one flow, iperf3, Gbit/s | 76.5 | 134.9 | 108.1 |
| one flow, 1 MiB writes (net-cost), Gbit/s | 94.8 | 137.4 | |
| kicks a second | 77,000 | ~200 | |
| four flows, iperf3, Gbit/s | 110.5 | 117.9 | 138.3 |
| host → guest, one flow | 87.8 | 86.1 | 71.9 |

What it costs ([net-cost](../benchmarks/net-cost.md)), % of one core
for the VMM / the helper (the guest sends paced by its kernel; the VMM's
share includes the vCPUs):

| | Before | After |
|---|---|---|
| 10 Gbit/s | 111 / 29 | 112 / 28 |
| 1 Gbit/s | 31 / 10 | 33 / 11 |
| 0.1 Gbit/s | 18 / 6 | 15 / 5 |
| idle | 0 / 0 | 0 / 0 |

A variant that watched after every frame, not only after a stream's,
measured the same; the stream rule is kept so stray frames never spin.

With four flows nothing on the host is saturated (busiest VMM thread
46 %, helper 62 %) and the rate stays below one flow's. The likely
limit, not measured, is the four senders sharing the guest's one
transmit queue; multi-queue virtio-net would test that.

Not answered here: the VMM spends 111 % of a core at a paced 10 Gbit/s
(the guest's sender 6 %), before and after alike; where that goes, and
whether VZ spends less at that rate, was not measured.

The net-iperf probe's check that the rings are in use read `vmmap`,
which now walks for minutes (guest RAM is tens of thousands of regions
since ADR 0003); `vmuc stat` reports it instead.

**Verdict.** Adopted (`devices/net/rings.rs`). One flow from the guest
to the host is now 25 % above VZ, at no measurable cost at idle or at
moderate rates. Four flows stay below VZ; multi-queue virtio-net is the
candidate. (Follow-up: it was not the queue; half-ring publishing
lifted four flows above VZ,
[four flows](2026-10-14-net-four-flows-and-cost.md).)
