# 2026-10-10 — M1 exit: Ubuntu cloud image boots on the vmuc kernel under QEMU

**Question.** Does the guest side of vmuc (6.18.55-vmuc kernel, cidata seed,
agent, ssh through the agent tunnel, free page reporting) work end to end
on a stock Ubuntu 24.04 arm64 cloud image, before any Hypervisor.framework
code exists?

**Method.** `tests/qemu/run.sh template:ubuntu` on the x86 Linux dev box:
`vmuc create` (591 MiB download, digest verified, qcow2 → sparse raw,
8 GiB disk), `vmuc _seed` with the musl agent inside, QEMU `-M virt`
TCG, 2 vCPUs, 2 GiB, virtio-mmio blk/blk(seed)/net/rng/balloon
(`free-page-reporting=on`), root by PARTUUID, agent reached over TCP via
the kernel command line `vmuc.agent.listen=`.

**Result: PASS, 174 s wall** (kept dir `/tmp/vmuc-qemu.CTITmh`).

| Check | Result |
|---|---|
| Agent `hello` | 129 s after QEMU start (TCG), kernel `6.18.55-vmuc`, hostname `vmuc-m1` |
| `boot.sh` done with the per-boot ID | yes, same instant |
| `mem_info` | total 1.98 GiB, available 1.79 GiB |
| virtio-balloon features | bit 5 (`VIRTIO_BALLOON_F_REPORTING`) negotiated |
| ssh via `vmuc _proxy … --via tcp:` | `uname -a` shows the vmuc kernel, user `vmuc` uid 1000 |
| power off from the agent | clean |

**Findings along the way.**
- QEMU enumerates virtio-mmio devices in reverse order; the first run
  mounted the seed as `/dev/vda` and panicked. Fixed by `root=PARTUUID=`.
- Linux 6.18 has no `page_reporting_delay_ms` parameter; the delay is
  `2*HZ`. The config knob was removed.
- The host user `ubuntu` is rejected as a guest name (Lima's rule: it
  collides with the image's own user), so the guest user is `vmuc`.

**Not proven here.** Anything about Hypervisor.framework, performance, or
host memory release. That starts in M2 on a Mac.
