# 2026-10-11 — M3: a stock Ubuntu cloud image boots on our virtio

## What landed

- `vmuc-virtio`: split virtqueue (batched pop, release-published used
  ring, EVENT_IDX with the lost-wakeup recheck), `IrqStatus`, a lock-free
  doorbell and one long-lived worker thread per device, the virtio-mmio
  v2 transport, and the console, blk and rng devices.
- `vmuc-vmm`: virtio-mmio slots on the MMIO bus with their SPIs; device
  transports shut down after the vCPUs join and before the VM is destroyed.
- `vmuc _boot --disk PATH[:ro] --hvc`.
- `vmuc-image`: downloads behind a default `fetch` feature (device models
  use the sparse-file helpers without a TLS stack), and a macOS TLS fix.

## Result

`vmuc create ubuntu` (591 MiB download, digest check, qcow2 → sparse raw,
20 GiB disk holding 1951 MiB on APFS) took 8.9 s on a warm network. Then
`vmuc _boot` with the instance's disk and cloud-init seed, 4 vCPUs,
4 GiB, console on hvc0:

| Milestone | Guest uptime |
|---|---|
| cloud-init local stage | 0.62 s |
| cloud-init network stage | 1.47 s |
| `vmuc-ubuntu login:` on hvc0 | ~2.7 s |
| cloud-init final stage starts (agent installed here) | 35.2 s |

Idle afterwards (`tests/bench/idle-cpu`): 0.33 % of one core with
4 vCPUs, footprint 867 MB with nothing reclaimed yet (M5).

## Findings

1. **The cloud-init final stage waits ~32 s** after the config stage, on
   something that stalls with no NIC (snap seeding or network-online; to
   be pinned with `systemd-analyze` once the agent is reachable). The seed
   installs and starts the agent in that stage, so the agent appears 35 s
   into boot. M4 must start the agent from an earlier stage to meet the
   < 3 s start-to-ssh target.
2. `multipathd.service` fails: the kernel has no `DM_MULTIPATH`. Harmless;
   either build it in or mask the unit from the seed.
3. ureq defaults to the rustls provider even when only native-tls is
   compiled in; macOS downloads panicked until the provider was set.
4. APFS keeps the converted disk image sparse (1951 MiB allocated of a
   20 GiB file); input for experiment E6.

## Not done here

`bench/boot-ssh` needs ssh over vsock (M4).
