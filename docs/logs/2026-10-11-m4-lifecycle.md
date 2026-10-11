# 2026-10-11 — M4: `vmuc start` and `vmuc shell` over vsock

## What landed

- `vmuc-virtio`: virtio-vsock with a mio muxer. One thread owns the
  vsock queues and every host socket; host-initiated streams only
  (guest-initiated connections get RST). Credit flow control, zero-copy
  in both directions except for what a slow host socket cannot take.
- `vmuc _vmm NAME`: one process per instance. Fresh seed per boot, pid
  scoped to the host boot session, `vmm.sock` (JSON lines: `status`,
  `stop`, `vsock_connect`), `events.jsonl` (booting, requirements,
  running, degraded, exiting), console to `console.log`, readiness =
  agent hello, then sshd's banner through an agent tunnel. Signals ask
  the guest to power off; a second one forces it. A guest reset
  re-executes the process.
- `vmuc start | stop | restart | shell`, `ssh.config` with
  `ProxyCommand vmuc _proxy NAME 22` and a persistent master connection,
  `vmuc list` with the live phase.
- The seed starts the agent from `bootcmd` (unit with
  `DefaultDependencies=no`) instead of cloud-init's final stage.

## Numbers (M4 Pro, Ubuntu 24.04, 4 vCPUs, current kernel)

| | |
|---|---|
| `vmuc start` → running, repeat boots | 1.8–3.2 s (`tests/bench/boot-ssh`: 2.76 s median) |
| `vmuc start` → first ssh session | 3.17 s median |
| `vmuc start` → running, an instance's first boot | 3.4–3.7 s (e2e) |
| second `vmuc shell -- cmd` (master connection) | 16 ms |
| `vmuc stop` | 0.5–4.4 s typically; up to 32 s, see below |

The kernel in use still runs the RAID6 boot benchmark (~600 ms of the
655 ms kernel time `systemd-analyze` reports); the config change that
removes it is committed and waits for a kernel build.

## Findings

1. **The agent arrived 35 s into a first boot** because the per-boot
   hook runs in cloud-init's final stage. Starting it from `bootcmd`
   needs care: `bootcmd` runs before `write_files`, and inside
   `cloud-init.service`, which is ordered before `sysinit.target`, so the
   unit has no default dependencies and is started `--no-block`.
2. Moving the mount into `bootcmd` broke the later per-boot hook: its
   `chmod` of the mount point hit the read-only ISO and `set -e` stopped
   `boot.sh` before provisioning. Fixed; the e2e test checks the
   boot-done marker now.
3. **Snap seeding holds an instance's first boot.** `snapd.seeded.service`
   runs ~30 s on first boot without a network, and `multi-user.target`,
   hence cloud-init's final stage and `boot.sh` (provisioning), wait for
   it. `vmuc start` does not wait for provisioning, so ssh is up at
   ~3.5 s; provisioning finishes ~35 s in. Re-measure once M6 gives the
   guest a network.
4. **A stop requested right after boot can take ~30 s**: snapd, still
   seeding, takes that long to stop. Later stops take 0.5–4 s.
5. plan.md locks `vmm.sock` as JSON lines; the M1 protocol crate had
   used length-prefixed frames. `vmm.sock` now follows the plan.

## Not done here

`vmuc copy`, `edit`, `console.sock` attach, and guest addresses in
`vmuc list` (M6 brings the NIC).
