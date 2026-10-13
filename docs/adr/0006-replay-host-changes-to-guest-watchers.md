# ADR 0006 — host changes replayed to guest watchers by the kernel

**Status:** accepted, 2026-10-13. It adds guest kernel patch 0002 (plan.md:
"as few patches as possible, each with its ADR") and the agent's
`fs_events` method. Evidence:
[E4](../experiments/2026-10-13-e4-inotify-propagation.md).

## Context

A file a host program changes in a share raises no inotify event in the
guest: virtiofs has no channel for the server to report changes (the
virtio-fs notification queue was never merged into Linux), and FUSE's
invalidation notices would not raise events if it had one. So a dev
server, test runner or build watcher in the guest never hears of an edit
made in an editor on the Mac. E4 measured five watcher libraries: none
saw any host change.

The mechanism that needs no kernel change is setting a file's times
again from inside the guest. Lima's guest agent sets both to the time of
the host event, which raises ATTRIB; E4 measured the better variant
(modification time only, the host's own, which raises MODIFY). It
reached every watcher for edits but nothing else: creations looked like
edits, deletions and renames like a change to the directory, and four
of the five watchers never watched a directory created on the host. It
also writes to the host file: its ctime, and, if the host changes the
file again in between, an older mtime.

## Decision

- Guest kernel patch 0002 adds `FUSE_VMUC_IOC_REPLAY`, an ioctl on a
  FUSE directory (CAP_SYS_ADMIN): it drops what the guest cached for the
  named entry (or its attributes and pages) and raises the event a local
  change would have (CREATE, DELETE, MODIFY, ATTRIB, CLOSE_WRITE,
  MOVED_FROM/TO with a cookie). Nothing reaches the host but the lookup
  of an entry that exists.
- The VMM watches the writable shares with one FSEvents stream that
  leaves out the VMM's own changes (`IgnoreSelf`: the guest's writes),
  classifies each batch by what is on disk now, keeps only changes in
  directories the guest has looked up, and sends them to the agent
  (`fs_events`), which replays them in order. No setting: idle costs
  nothing measurable (E4).
- Writable shares only, as in Lima: they hold the projects that dev
  servers and test runners watch, while a read-only share can be the
  whole home directory, with its churn and privacy-protected folders.
- When FSEvents drops changes (it does under bursts) a separate thread
  rescans what the guest knows there against the disk, listing only
  directories the guest has looked inside; a rescan can block and must
  not hold up live changes.
- A guest kernel without the patch answers ENOTTY; the VMM then stops
  watching and logs one warning event.

## Consequences

- Watchers in the guest see host edits as they see local ones, about
  18 ms after the change (12 ms of it FSEvents' own delay).
- Changes on the host to a read-only share stay invisible to watchers,
  as before; making one writable is the way to get them.
- The patch is not upstream: it is rebased with every kernel bump, and
  its ioctl is a private ABI between vmuc's kernel and agent, which ship
  together. A user-supplied kernel (`images[].kernel`) gets no
  propagation.
- Deleting a directory reports the directory, not each entry in it (the
  guest can no longer look inside), and a burst that creates a
  directory and its contents can outrun a watcher adding a watch on it,
  the same race a local `mkdir -p` has.
