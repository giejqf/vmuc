# ADR 0007 — suspend and resume instances

**Status:** accepted, 2026-10-13. It moves one use of snapshots into
v0.1 (plan.md listed snapshots as out of scope) at the user's request
after E5. Evidence:
[E5](../experiments/2026-10-13-e5-snapshot-restore.md).

## Context

Stopping an instance powers the guest off: its programs (dev servers,
shells in tmux, a database warmed up) are gone, and the next start
boots again, 2.6 s to ssh for Ubuntu. Keeping it running costs host
memory and wakeups. E5 showed that a VM saves and restores exactly
through Hypervisor.framework's public API, that a restore with guest
RAM mapped copy-on-write from its image takes ~40 ms whatever the size,
and that the restored VM is billed only for what the guest writes
afterwards, with free page reporting working as before. E5 restored
only what a busybox guest uses; an instance also has disks, vsock
connections to host processes, virtiofs shares and a NIC behind a
helper process, none of which survive the VMM process.

## Decision

- `vmuc suspend NAME` writes the running instance to
  `~/.vmuc/NAME/suspend/` and ends its VMM; `vmuc start NAME` resumes
  it. One suspended state per instance: no named snapshots, clones or
  migration (those stay out of scope).
- Before pausing, the agent syncs the guest's filesystems (bounded
  wait, best effort), so the disk alone is a consistent image if the
  suspended state is ever discarded.
- A snapshot pauses every vCPU, then quiesces every device: it stops
  taking buffers and finishes those it took. Most devices finish
  synchronously; virtio-fs waits for the requests running on its pool,
  up to a few seconds, and one blocked in the host (a privacy prompt, a
  FIFO) fails the suspend. Only then are the transports, the devices'
  own state and, last, the GIC saved, so an interrupt raised by a
  finishing request is in the saved state. A failure anywhere before the
  files are complete resumes the guest where it was and reports why.
- `suspend.json`, written last after the RAM image, the GIC state and
  the disk are flushed to stable storage (`F_FULLFSYNC`), marks a
  complete suspended state.
- Resume rebuilds the machine from the instance's configuration and
  refuses, naming the difference, if its shape changed since the
  suspend (vCPUs, memory, devices, shares, MAC address). Guest RAM is
  mapped copy-on-write from the image. Before any vCPU runs,
  `suspend.json` and the files are removed (the mapping keeps the image
  alive until the VMM exits), so a suspended state is resumed at most
  once: the disk moves on as soon as the guest does.
- Across the process boundary:
  - vsock tells the guest its transport was reset
    (`VIRTIO_VSOCK_EVENT_TRANSPORT_RESET`): connections to the host are
    gone, listeners (the agent's) stay;
  - virtio-fs saves its node table and open handles and reopens files
    by path; a handle whose file is gone stays gone (EBADF in the
    guest);
  - the FSEvents watcher starts from the event ID recorded at suspend,
    so host changes made meanwhile reach guest watchers;
  - the NIC comes back on a new helper with the same MAC address;
    guest TCP connections survive if their peers kept them;
  - the guest's wall clock is stepped by the agent (`time.sync`) as
    soon as it answers; its monotonic clock continues from the pause.
- `vmuc stop --force NAME` discards a suspended state (the next start
  boots); a plain `stop` refuses and says so. Shares with DAX cannot be
  suspended yet (their window mappings would need re-creating); suspend
  refuses them.

## Consequences

- A suspended instance costs disk only: its RAM image holds the pages
  the guest had in use, nothing else.
- The state is tied to this macOS (the GIC state is opaque and
  versioned by the framework) and to this vmuc's snapshot format. A
  resume that fails before the guest runs leaves the state in place and
  says how to discard it.
- The disk of a suspended instance must not change behind it; vmuc
  itself never touches it between suspend and resume.
- Editing the machine's shape while suspended blocks the resume until
  the edit is reverted or the state discarded.
