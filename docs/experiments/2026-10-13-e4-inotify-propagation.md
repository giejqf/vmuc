# 2026-10-13 — E4: host changes and the guest's file watchers

**Question.** A program on the Mac changes a file in a share. Do file
watchers in the guest (dev servers, test runners, build watchers, all
inotify underneath) hear of it? If not, what makes them, how close to a
local change does it get, and what does it cost?

**Method.**

1. `tests/bench/inotify/matrix.py`: five watcher libraries in the guest
   (inotifywait from inotify-tools, watchfiles 1.3 on notify-rs, Ubuntu's
   watchdog, chokidar 4 as in vite and nodemon, Go fsnotify 1.8 used the
   usual way) and ten operations in a writable share: edit in place,
   same-size overwrite, atomic save (temp file renamed over), create,
   `mkdir` with a file, an edit in that new directory, rename, chmod,
   delete, `rm -rf` of a directory. Each was done in the guest (the
   reference), on the host, and on the host followed by a replay in the
   guest: `touch` (setting the modification time the host saw, through
   the share), `ioctl` (a patched guest kernel, below) and `agent` (the
   instance's own propagation, once built).
2. `tests/bench/inotify/cost.py`
   ([inotify](../benchmarks/inotify.md)): a host change reaching a guest
   `inotifywait` as a round trip, 50 lone creates and 50 lone appends;
   2000 creates at once; the VMM's CPU. FSEvents' own delay:
   `cargo test -p vmuc-fs --release -- --ignored fsevents_delivery`. Idle
   cost: the VMM's CPU over 120 s, idle, with main's binary (no watcher)
   and this one, four pairs on the same instance, kernel and agent.

M4 Pro, macOS 26.6.2, Ubuntu 24.04, 4 vCPUs, 4 GiB, guest kernel
6.18.55 with patch 0002 (`439c9e14cc4e…`).

**Result.**

Nothing made on the host reached any watcher, for any operation.
virtiofs has no way to say so: Linux never merged the virtio-fs
notification queue, and FUSE's invalidation notices raise no inotify
events anyway.

Replaying in the guest (what each watcher reported; "all" means all
five, the same as for the operation done in the guest):

| Host operation | `touch -m` replay | Kernel replay (`ioctl`, `agent`) |
|---|---|---|
| Edit, same-size overwrite | all: modified | all, as in the guest |
| Atomic save | all: a.txt modified; the temp file never seen | all, as in the guest (a move), less the temp file's creation, which FSEvents folds into its rename |
| Create | chokidar: added; the others: modified | all, as in the guest |
| `mkdir d2` + file | chokidar: both added; the others: d2 modified | as in the guest, except that from the agent watchfiles saw only d2 (below); inotifywait misses the file in the guest too |
| Edit in d2 | chokidar only; the other four never watched d2 | all |
| Rename | chokidar: added + removed; the others: the new name and the directory modified | all, as in the guest (a move) |
| chmod | four: a modification, where the guest's own chmod is an ATTRIB (chokidar reports neither) | all, as in the guest |
| Delete, `rm -rf d2` | chokidar: removed; the others: the directory modified | all; for `rm -rf` the directory, not each entry in it |

Setting the times from inside the guest is what Lima does (its agent
sets both times to the host event's, which raises ATTRIB rather than
the MODIFY measured here). It gets edits through and nothing structural,
and it writes to the host file: its ctime, and an older mtime if the
host changes the file again in between.

The kernel replay is guest kernel patch 0002: `FUSE_VMUC_IOC_REPLAY` on
a FUSE directory drops what the guest cached for the named entry and
raises the event a local change would have; nothing reaches the host but
the lookup of an entry that exists. The VMM watches the shares with
FSEvents (leaving out its own changes, which are the guest's writes),
classifies each batch by what is on disk now, keeps changes in
directories the guest has looked up, and sends them to the agent.

Two differences from a local change remain. A removed directory is
reported, its entries are not: the guest can no longer look inside. And
a burst that creates a directory and its contents replays them
microseconds apart, faster than some watchers add a watch on the new
directory: the same race a local `mkdir -p` runs, lost more often.

Cost:

| | |
|---|---|
| Host change to guest watcher and back, p50 / p95 (8 runs) | create 17.6–18.3 / 20.3–21.0 ms; append 17.3 / 19.4–19.6 ms |
| FSEvents' own delivery, p50 / p95 | 12.1 / 12.5 ms (whatever the stream latency, 1 to 50 ms) |
| 2000 creates at once | the last reaches the guest 0.08–0.11 s after the host's loop; VMM CPU 0.55–1.5 s, vCPUs included |
| 20000 creates at once | all delivered (3 runs) |
| Idle VMM CPU, without / with the watcher (4 pairs) | 0.31–0.36 % (and one 1.22 %) / 0.33–0.38 %, with the home share watched too |

Two failures found on the way:

1. FSEvents drops events under a burst: in about half of the cost
   probe's 2000-file bursts it flagged the share's root
   `MustScanSubDirs | UserDropped`, in a second process's stream at the
   same moment, and 86–110 creates never arrived. A client must rescan
   then; vmuc now does (what the guest knows there, checked against the
   disk), and with it no run lost an event.
2. The first rescan, of the home share, blocked in an `open` of a
   directory under `~` and stopped propagation for every share. Rescans
   now run on their own thread and list only directories the guest has
   looked inside, and only writable shares are watched, as Lima does: a
   read-only share can be the whole home directory, with its churn and
   privacy-protected folders, and the projects dev servers watch are
   shared writable.

**Verdict.**

1. Host changes are invisible to guest watchers without help; a guest
   `touch` gets edits through but not creations, deletions or renames.
2. Replaying in the guest kernel gets everything through as a local
   change would, about 18 ms after the change (12 ms of it FSEvents'),
   for nothing measurable at idle. vmuc ships it for writable shares:
   patch 0002, the agent's `fs_events`, FSEvents in the VMM
   ([ADR 0006](../adr/0006-replay-host-changes-to-guest-watchers.md)).
3. FSEvents drops under bursts and must be answered with a rescan; a
   rescan reads the host filesystem and must not hold up live changes.
