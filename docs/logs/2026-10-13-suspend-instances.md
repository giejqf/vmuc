# 2026-10-13 — suspending instances: what E5's prototype did not cover

Building `vmuc suspend` (ADR 0007) on E5's snapshots took the instance's
other devices across a process boundary, and turned up faults in the
snapshot path that the busybox prototype could not show.

## A restore lowered interrupt lines behind the devices' backs

The first Ubuntu resume restored in 106 ms and then never became ready:
the guest ran (free page reporting released memory, the NIC carried
traffic, its counter process kept counting) but no vsock connection
reached it, and `ss --vsock` in the guest (over ssh by IP) still showed
the two connections from before the suspend.

`VmBuilder::restore` put the transports back, activating the devices,
and only then restored the GIC with `hv_gic_set_state`, which sets every
SPI's level as saved. A restored vsock device announces its transport
reset on activation, so it raised its SPI first; the GIC restore then
lowered it, while the device's InterruptStatus stayed set. `IrqStatus`
raises the line only on a 0 → 1 change of the status, so the line never
came up again and the device went silent for good. E5's busybox guest
had no interrupt pending at its snapshots and raised its first one after
the vCPUs ran, so it never met this.

Fix: the GIC first (it needs the vCPUs, so after they are created), then
the transports and platform devices, then the vCPUs resume.

## A snapshot saved the GIC before the devices had stopped

E5's `Vm::snapshot` paused the vCPUs, saved the GIC and the transports,
and then stopped the device workers. A request finishing in between
(a block read, a vsock packet) raised an interrupt that neither the
saved GIC nor the saved InterruptStatus had: after a restore the guest
would wait for a completion it was never told of. Snapshots now pause,
quiesce every device (stop taking buffers, finish those taken), and only
then save the transports, the devices' own state and, last, the GIC.

## virtio-fs completes out of order

A restored queue picks up at its used ring, taking again what the device
had taken and not finished. That is right only for a device that
finishes in the order it takes: virtio-fs runs requests on a pool, so a
quick LOOKUP can finish before an OPEN blocked in the host, and resuming
at the used ring would redo the LOOKUP and lose the OPEN. Its quiesce
waits for the pool (3 s at most; one blocked in the host fails the
suspend and the guest runs on).

## The first connection after a resume could die with the old ones

The first benchmark rows had a 1 GiB guest back at ssh only after 1.2 s,
of which the restore was 0.1 s and the agent's first answer 1.07 s. The
guest handles the reset event and arriving packets in separate work
items; when the VMM's first connection request came first, the guest
accepted it and the reset then closed it along with the old ones, with
nothing sent to the host, which waited out its 1 s read timeout. Linux
makes the event buffer available again once it has handled the event,
so the device now holds back what it delivers until the event queue's
avail index moves (1 s at most): 0.13–0.17 s to ssh since.

## Suspending a resumed guest again

`write_ram` wrote the pages `mincore` called resident. A resumed VM's
RAM is a private mapping of the old image, and a page the guest has not
written since is the file's: out of the page cache it is not resident,
and a second suspend would have written a hole over it. The old image's
data extents now count as data (a unit test evicts the image to show
it). On macOS a compressed or paged-out page was skipped the same way;
any `mincore` flag but "anonymous" alone now counts.

## What crosses the process boundary

- vsock: the host ends of the guest's connections are gone; the device
  tells the guest (`VIRTIO_VSOCK_EVENT_TRANSPORT_RESET`) and Linux resets
  every connected socket, keeping the agent's listener.
- virtio-fs: node IDs and file handles live in the server; they are the
  device's state now, files reopened by the path `F_GETPATH` reports
  (it follows host renames, and keeps an unlinked file's last path, hence
  the inode check on reopen).
- The guest clock continues from the pause (the vtimer offset is
  shifted, as in E5); the agent steps the wall clock as soon as it
  answers.
- Host changes to the writable shares while away: one rescan per share
  once the guest runs.

Verified by `tests/e2e/tests/suspend.rs`; numbers in
[suspend](../benchmarks/suspend.md).
