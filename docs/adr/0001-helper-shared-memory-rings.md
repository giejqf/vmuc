# ADR 0001 — Frames between the VMM and the vmnet helper travel through shared-memory rings

**Status:** accepted, 2026-10-12; measured in
[the rings experiment](../experiments/2026-10-12-helper-rings.md).

## Context

The root helper owns the vmnet interface (`com.apple.vm.networking` is
restricted, so the VMM cannot); the VMM owns the virtio-net device. Until
now frames crossed between them as datagrams on a unix socketpair. With
TSO through vmnet's virtio-net headers the path reaches 52.7 Gbit/s
host-to-guest and 63.6 guest-to-host against VZ NAT's 71.9 and 108.1, and
the helper's relay thread is the limit at 88-100 % of a core
([network log](../logs/2026-10-12-m6-network-on-vmnet.md)). Each frame is
copied three times guest-to-host (guest memory → socket, socket → helper,
helper → vmnet) and four times host-to-guest (vmnet → helper, helper →
socket, socket → VMM, VMM → guest), with a send and a recv per frame.

## Decision

The VMM creates one shared memory region (`shm_open`, unlinked at once,
`MAP_SHARED`), passes its fd to the helper over the socketpair
(`SCM_RIGHTS`) once the helper's hello names the same ring layout version
(`ring_layout`), and both map it. It holds two single-producer, single-consumer byte rings of 4 MiB, each
behind a 16 KiB control page:
VMM → helper and helper → VMM. A record is an 8-byte header (frame
length, distance to the next record) followed by the frame, virtio-net
header included when offloads are on; records never wrap, a marker sends
the consumer back to the start.

- Guest to host: the VMM copies the frame from guest memory into the
  ring; the helper hands vmnet iovecs pointing into the ring and releases
  the space once `vmnet_write` returns. Two copies.
- Host to guest: the helper reserves room for a batch of maximum-size
  frames and lets `vmnet_read` fill the ring directly; the VMM copies each
  frame into the guest's buffers. Two copies.
- The socketpair stays: the hello, the fd, wake-ups and liveness. A
  consumer about to sleep sets a flag and re-checks; a producer that sees
  the flag after publishing sends a one-byte kick. A producer that finds
  the ring full sets its own flag and the consumer kicks it when it frees
  space. Under load nobody sleeps and no kick is sent.
- A helper whose hello names no or another layout (installed before this
  change, or before a layout change) keeps the datagram path; the VMM
  keeps both, and so does the helper for a VMM that sends frames.

## Consequences

- The helper now reads two things the unprivileged VMM writes: ring
  indices and record headers. It reads each once, bounds-checks it
  against the mapped size before use, never forms a reference into the
  shared region, and stops (closing its interface) on anything
  malformed. Frame bytes stay opaque; a VMM racing its own frame only
  garbles that frame, which vmnet copies before parsing.
- The helper still never touches guest memory: the rings are the VMM's
  own heap-like region, not guest RAM, which stays private and anonymous
  for reclaim.
- 8 MiB plus a page of shared memory per instance with a NIC, billed to
  the VMM once touched; `vmuc stat`'s host footprint includes it.
- Changing the helper means users reinstall it (`vmuc sudoers`); a
  changed layout needs a new `ring::LAYOUT`.
- The VMM reports a helper that exits while the VM runs, with what it
  printed: after the hello nothing else reads its stderr.
- Multi-queue virtio-net (several ring pairs and threads) remains the
  way to scale parallel flows; this ADR is about the per-flow ceiling.
