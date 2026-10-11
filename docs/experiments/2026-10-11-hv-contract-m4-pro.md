# 2026-10-11 — Hypervisor.framework contract on an M4 Pro (macOS 26.6.2)

**Question.** Do the exit-handling rules in AGENTS.md, which came from
ArcBox's experience, hold on this machine, and where can the in-kernel
GICv3 frames go in guest physical space?

**Method.** `cargo test -p vmuc-hv --test contract -- --nocapture`
(`crates/vmuc-hv/tests/contract.rs`). One VM with the in-kernel GIC,
one vCPU at EL1h with interrupts masked, running a 16 KiB page of
hand-assembled instructions, each followed by `brk #0`. For each probe
the test records the exit and how far PC moved. A second thread calls
`hv_vcpus_exit` after 300 ms to end the WFI probe. Before creating the
GIC it queries the framework's GIC geometry, the reserved INTIDs and the
vCPU limit. Host: Mac mini, Apple M4 Pro, 24 GiB, macOS 26.6.2 (25G83),
binary ad-hoc signed with `com.apple.security.hypervisor`.

**Result.**

| Probe | Exit | PC after | AGENTS.md |
|---|---|---|---|
| `hvc #0` | EC 0x16 | +4 (framework advanced it) | agrees |
| `smc #0` | EC 0x17 | **+0** | **disagrees**: it says HV advances PC for SMC |
| `str w1, [x2]` to unmapped IPA | EC 0x24, ISV=1, write, 4 bytes, Rt=1 | +0 | agrees |
| `ldr w3, [x2]` from unmapped IPA | EC 0x24, ISV=1, read, 4 bytes, Rt=3 | +0 | agrees |
| `msr osdlr_el1, x0` | EC 0x18, op0=2 op1=0 CRn=1 CRm=3 op2=4 | +0 | agrees (VMM does RAZ/WI and PC += 4) |
| `wfi` with the GIC present | nothing until `hv_vcpus_exit`, then `CANCELED` after 301 ms | +4 (WFI retired) | agrees: the framework idles inside `hv_vcpu_run` |

| Geometry | Value |
|---|---|
| Max vCPUs per VM | 64 |
| IPA size, default / max | 36 / 40 bits |
| Distributor | 64 KiB, 64 KiB alignment |
| Redistributor region | **32 MiB** (128 KiB per vCPU frame pair), 64 KiB alignment |
| MSI region | 64 KiB, 64 KiB alignment |
| SPIs | INTID 32, 988 of them |
| Reserved INTIDs | EL1 virtual timer 27, EL1 physical timer 30, PMU 23 |
| EL2 (nested) supported | yes |

**Verdict.**

1. **SMC does not advance PC.** A trapped SMC's preferred return address
   is the SMC instruction itself (ARM ARM, and QEMU's HVF backend advances
   PC for SMC but not HVC). The VMM advances PC after handling an SMC.
   Linux never issues one here because the FDT declares PSCI
   `method = "hvc"`, but following the old rule would spin a vCPU on a
   stray SMC forever. AGENTS.md's HVC/SMC line needs a human-approved edit.
2. **The redistributor region does not fit at `0x080A_0000`.** The
   framework sizes the region for every vCPU it supports, not for the
   ones created, so its 32 MiB would cover the PL011 (`0x0900_0000`),
   PL031 and the virtio-mmio window (`0x0A00_0000`). The guest physical
   map moves the redistributors to `0x0C00_0000`, with a 64 MiB window
   that ends before `0x1000_0000`. The contract test fails if a future
   macOS grows the region past that.
3. WFI idles inside `hv_vcpu_run` with the in-kernel GIC, as ArcBox
   found: no WFI exit handling is needed on the hot path, and an
   asserted interrupt (or `hv_vcpus_exit`) is the only way back out.

The test is the probe that `feasibility.md`'s risk register asks for;
rerun it after every macOS update.
