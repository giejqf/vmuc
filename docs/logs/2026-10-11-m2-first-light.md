# 2026-10-11 — M2: first light on Hypervisor.framework

The first session on the Apple silicon Mac (M4 Pro, 24 GiB, macOS
26.6.2). Everything below ran on that machine.

## What landed

- `vmuc-hv`: SDK-accurate GIC signatures (`hv_gic_get_intid` takes a
  `uint16_t`), config objects released with `os_release`, GIC geometry
  queries; `hv_vcpus_exit` and `hv_gic_set_spi` callable without the VM
  handle. A contract probe (`crates/vmuc-hv/tests/contract.rs`) checks
  the exit-handling rules and GIC geometry on hardware.
- `vmuc-virtio`: `GuestMemory` (raw-pointer view of guest RAM with
  atomics for ring indices, Arc-owned backing), `Interrupt`, `BusDevice`.
- `vmuc-vmm`: guest physical map, FDT generator, arm64 Image loader,
  PSCI 1.0 subset, PL011 and PL031, a batching console writer, guest RAM
  mapping, MMIO bus, machine state with PSCI power transitions, vCPU
  threads and exit dispatch, VM builder and contract-order teardown.
- `vmuc _boot`: direct boot in the foreground, terminal as serial console.
- Busybox initramfs builder, `tests/e2e` boot tests, `tests/bench/boot-serial`.

## Findings

1. **SMC does not advance PC** (AGENTS.md says it does). The VMM
   advances it. Linux never issues SMC here (PSCI `method = "hvc"`), so
   this only matters for a stray SMC, which would otherwise spin. The
   AGENTS.md line needs a human-approved edit.
   (`docs/experiments/2026-10-11-hv-contract-m4-pro.md`)
2. **The redistributor region is 32 MiB**, sized for every vCPU the
   framework supports. At QEMU's `0x080A_0000` it would cover the PL011,
   PL031 and virtio-mmio windows; it moved to `0x0C00_0000`.
3. **WFI idles inside `hv_vcpu_run`** with the in-kernel GIC, and the
   vtimer is delivered through the GIC (INTID 27) without vtimer exits.
   An idle 4-vCPU busybox guest used 0.02 s of host CPU in 10 s (0.2 %).
4. **RAID6's boot benchmark cost ~720 ms** of a 760 ms kernel-to-init.
   btrfs selects `RAID6_PQ`; `CONFIG_RAID6_PQ_BENCHMARK` is now off in the
   fragment (not yet rebuilt: the kernel builds on Linux/CI). Measured with
   `initcall_blacklist=raid6_select_algo` as a stand-in: 120 ms to init
   with 4 vCPUs, 170 ms for the whole process.
5. The next-largest cost (~100 ms) is tracing setup (`initcall_debug`
   blames `init_kprobe_trace`, but skipping it moves the wait to the next
   initcall, so it is the trace-event work behind a shared lock). Under
   target; left alone.
6. Alpine's busybox-static has no `cttyhack`; init opens the active
   console itself to get a controlling terminal.

## Exit criteria (plan M2)

| Criterion | Status |
|---|---|
| Boots the M1 kernel with a busybox initramfs to a serial shell | yes, interactive, job control works |
| SMP: 4 vCPUs through PSCI CPU_ON | yes, all up in < 1 ms |
| Teardown contract and `hv_vcpus_exit` stop | yes; host stop returns in well under 1 s (e2e) |
| `bench/boot-serial` < 400 ms | 120 ms with the RAID6 change; 620 ms with the current kernel build |
| Reboot and poweroff work | yes; `_boot` reboots into a fresh VM in the same process |

## Not done here

The kernel with the RAID6 change has not been built: there is no cross
toolchain on the Mac. The kernel workflow builds it on push.
