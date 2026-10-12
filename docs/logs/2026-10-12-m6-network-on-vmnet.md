# 2026-10-12 — M6 networking, run against vmnet

The vmnet helper and its sudoers rule installed (`vmuc sudoers`), the
network path ran end to end for the first time and needed five fixes
before it was both correct and fast.

## Verified on HV (M4 Pro, macOS 26.6.2)

| What | Result |
|---|---|
| DHCP, DNS, `curl https://…` from the guest | address from vmnet's DHCP in shared mode, 200 OK |
| `ssh user@<guest-ip>` from the host | works |
| 512 MiB of random data each way over HTTP | SHA-256 matches both ways (TSO and checksum offload active) |
| IPv6 | SLAAC address, gateway ping 0.25 ms |
| `vmuc stop`, and SIGKILL of the VMM | helper and its vmenet interface gone within 1 s |
| e2e suite with a real NIC | passes; first-boot lifecycle 46 s → 15 s (snap seeding no longer waits for a network) |

## Throughput, step by step (iperf3, one flow, Gbit/s)

| State | host→guest | guest→host |
|---|---:|---:|
| First run | 0.04 | 0.41 |
| Socketpair buffers 4 KiB → 4 MiB | 8.3 | 4.2 |
| TSO + virtio-net headers through vmnet | 7.1 | 63.2 |
| Stale helpers gone, so the bridge offers TSO | 35.5 (4041 retransmits) | 63.5 |
| Frames wait for receive buffers; 1024-entry ring | 52.7 (0 retransmits) | 63.6 |
| Lima on VZ, VZ NAT (reference) | 71.9 | 108.1 |

Four flows: 49.3 / 77.8 against VZ's 80.3 / 138.3. Target (> 10) met;
rows in `docs/benchmarks/net-iperf.md`.

## Findings

1. **A unix datagram socket on macOS holds 4 KiB and caps a datagram at
   2 KiB**, and a send into a full one fails with ENOBUFS instead of
   waiting. Two frames fit; everything else in a burst was dropped.
2. **vmnet speaks virtio-net headers from macOS 15.4**
   (`vmnet_enable_virtio_header_key`, with `vmnet_enable_tso_key`), so
   the guest's 64 KiB segments and partial checksums pass through
   untouched. Looked up at run time; older systems get bare frames.
3. **A blocked recv on a datagram socket never learns its peer closed**,
   and the ECONNRESET the peer leaves goes to whichever call looks first.
   Every helper outlived its VMM (17 root processes had piled up), and
   after a fix based on ECONNRESET alone, still after a SIGKILL when the
   vmnet thread's send took the error. The helper now waits at most a
   second per recv and asks `getpeername`, which keeps saying ENOTCONN.
4. **A bridge offers the host stack only the offloads every member has**:
   the stale, offload-less vmenet interfaces kept host-to-guest traffic in
   1500-byte frames even after TSO was on.
5. **Dropping a 64 KiB segment costs ~44 retransmitted TCP segments.** A
   frame that does not fit the guest's posted buffers now waits for more;
   back-pressure reaches vmnet through the socket and the helper.
6. **With a NIC, boot waited 1.3 s for DHCP** (Ubuntu orders cloud-init,
   and so ssh, after network-online). The NIC is now optional for
   network-online; provisioning waits for it instead. Start to ssh 3.25 s.

## The gap to VZ

CPU while iperf3 runs, % of one core:

| | host→guest | guest→host |
|---|---|---|
| vmuc: VMM + helper | 168 + 88 = 256 at 51.7 Gbit/s | 131 + 102 = 233 at 62.9 Gbit/s |
| VZ process | 270 at 68.4 Gbit/s | 289 at 102.7 Gbit/s |

vmuc spends ~30 % more CPU per bit and is capped by single-threaded
stages: the helper's relay thread is at 88-100 %, the VMM's virtio-net
worker at 50-79 %. The structure is the reason: VZ's device sits in the
process that owns the vCPUs and hands frames to the host through Apple's
entitled path (`com.apple.vm.networking`), one hop. vmuc cannot hold that
entitlement (locked decision), so frames cross a second process: guest
memory → unix socket → root helper → vmnet, two extra copies and a
process hop per batch, one thread per direction. More flows do not help
because the NIC has one queue pair.

What would narrow it, not done: multi-queue virtio-net with a socket and
helper thread per queue (parallel flows), and a shared-memory ring
between VMM and helper in place of the socket (one copy fewer). Sharing
guest RAM with the helper is out: reclaim needs it private and anonymous.
