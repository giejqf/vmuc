# Changelog

## 0.1.0 — 2026-10-16

The first release.

- **Instances.** `vmuc create` from templates (Ubuntu 24.04, Alpine,
  Docker, your own YAML) with stock cloud images, verified and converted
  to sparse raw disks; `start`, `stop`, `restart`, `shell`, `copy`,
  `edit`, `delete`, `list`, `info`. Start to an ssh prompt in about
  2.6 s on an M4 Pro.
- **Suspend, resume, duplicate.** `vmuc suspend` saves a running guest
  and frees all its host memory; `vmuc resume` brings it back in about
  0.15 s with its programs, open files and network as they were.
  `vmuc duplicate` copies a stopped instance in milliseconds, its disk
  an APFS clone, with its own MAC, address, hostname, host keys and
  machine ID.
- **Memory.** Free page reporting is always on: memory the guest frees
  is back in macOS within seconds (90 % of 2 GiB in about 2 s), guest RAM
  is billed once, and `vmuc stat` accounts for every byte. Host memory
  pressure makes guests drop caches and compact.
- **Files.** vmuc's own virtiofs shares your home directory read-only by
  default; writable shares replay changes made on the Mac to the guest's
  file watchers; DAX per share, opt-in.
- **Network.** macOS's vmnet NAT (shared or bridged) through a small root
  helper with shared-memory rings and TSO: 87 Gbit/s host to guest, 136
  guest to host on one flow. Guest ports appear on localhost by
  themselves, over vsock.
- **TUI.** `vmuc tui` watches every instance and starts, stops, suspends,
  resumes, duplicates and opens shells.
- **Scripts and agents.** `--json` everywhere, exit codes by kind of
  failure, idempotent `start`, `stop` and `suspend`, `vmuc exec` with
  arguments as given, `vmuc edit --set` without an editor.
- **Guest kernel.** Linux 6.18 with three patches: whole free lists
  reported per pass, host changes replayed to inotify watchers, and no
  SMSTOP when SME has nothing to stop (8–43 % faster syscall-heavy work
  on M4).

Requires an Apple silicon Mac with macOS 15 or newer; developed and
measured on macOS 26.
