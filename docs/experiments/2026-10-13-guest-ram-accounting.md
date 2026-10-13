# 2026-10-13 — Guest RAM that both sides touch is billed twice

**Question.** In the first host-pressure runs (E1), a 4 GiB guest
re-reading a 3 GiB file showed a host footprint of 6.5 GiB, more than
its RAM. What is macOS billing, and can vmuc be billed only for what
it uses?

**Method.**

1. On a running guest: footprint before and after the guest reads a
   1 GiB file cold (`dd`, through virtio-blk), and after it drops its
   caches.
2. `crates/vmuc-hv/tests/accounting.rs` on bare Hypervisor.framework: a
   64 MiB region mapped into a VM is stored to by the host and loaded by
   a real vCPU (the virtio-blk read pattern), or stored to by the vCPU
   and loaded by the host (virtio-blk write), for memory from `mmap`, from `hv_vm_allocate`, and from
   128 KiB ledger-tagged granules; then a 16 MiB sub-range of
   guest-dirtied memory is released in each candidate way.
3. `hv_vm_allocate` disassembled (lldb, macOS 26.6.2) to see what makes
   its memory different.

M4 Pro, macOS 26.6.2.

**Result.**

A cold 1 GiB read on `ubuntu` raised the footprint by **2030 MiB**;
dropping the guest's caches (which releases the range) took all of it
back. On bare HV:

| Memory | 64 MiB: host stores, guest loads | guest stores, host loads |
|---|---|---|
| `mmap(MAP_ANON\|MAP_PRIVATE)` (vmuc until now) | **128.2 MiB billed** | **128.2** |
| `hv_vm_allocate` | 64.1 | 64.1 |
| 128 KiB ledger-tagged granules | 64.1 | 64.1 |

`hv_vm_allocate`
reserves the range and, per 128 MiB chunk, maps a memory entry made with
`MAP_MEM_NAMED_CREATE | MAP_MEM_LEDGER_TAGGED` over it
(`VM_FLAGS_FIXED | VM_FLAGS_OVERWRITE`); its header says the API
"enables accurate memory accounting". But a page of such an entry is
only freed with the whole entry:

| 16 of 64 guest-dirtied MiB, after unmap + … + map | Released | Pages zero |
|---|---|---|
| `mmap(MAP_FIXED)` on `mmap` memory (vmuc until now) | 16.0 | all |
| `madvise(MADV_DONTNEED)` on `hv_vm_allocate` memory | 0 | none |
| `madvise(MADV_FREE_REUSABLE)` | 0 | none |
| `madvise(MADV_ZERO)` | 0 | all (zeroed in place) |
| fresh tagged entries over part of an `hv_vm_allocate` chunk | 0 | all |
| fresh granules over granules | **16.0** | all |

`mmap(MAP_FIXED)` over `hv_vm_allocate` memory releases, but leaves plain
anonymous memory behind, billed twice again on the next shared touch.
Released granules are billed 64.1 for the next 64 MiB of shared touches.
Building 4 GiB as 32 768 granules takes 47–55 ms and one `hv_vm_map` over
all of them 7–8 ms.

On a running guest with granule RAM (same instance, same file):

| | `mmap` | granules |
|---|---|---|
| Idle after reclaim | 443–449 MiB | 288 MiB |
| Cold 1 GiB read | +2030 MiB | +1065 MiB |
| After the guest drops its caches | 459 MiB | 325 MiB |

The benchmark probes on `ubuntu` (4 vCPUs, 4 GiB, patched kernel), the
`mmap` column from the same day unless noted:

| Probe | `mmap` | granules |
|---|---|---|
| mem-idle: idle after three reclaims | 375 MiB (2026-10-12) | **283 MiB** (runs 273–295) |
| idle-cpu: footprint 60 s after boot, no reclaim | 1541 MB | 1260–1274 MB |
| idle-cpu: CPU | 0.37 % | 0.30–0.33 % |
| mem-reclaim: 90 % of a freed 2 GiB back after | 0.5–2.1 s (`memtest`) | 1.0–2.1 s |
| mem-reclaim: release p50 / p99 per range | 25 / 500 µs (`memtest`) | 250 / 1000 µs |
| stall probe: worst gap of an unrelated vCPU during releases | ~40 µs, quiet ~27 (2026-10-11) | 24–42 µs |
| boot-ssh: start to ssh, median | 2.58–2.63 s | 2.68 s |

**Verdict.**

1. Every page a device writes and the guest reads, or the guest writes
   and a device reads, was billed twice: page cache filled through
   virtio-blk and virtiofs, network buffers, the kernel image vmuc
   copies in. `vmuc stat`, Activity Monitor and jetsam all saw up to
   twice what the VM used, and some of the idle "unattributed" memory of
   the [idle-footprint experiment](2026-10-12-idle-footprint.md) was
   this.
2. Guest RAM is now 128 KiB ledger-tagged granules and the release
   replaces whole granules ([ADR 0003](../adr/0003-guest-ram-from-tagged-granules.md)):
   billed once, released as before, at a cost of one entry per granule
   and a release granule of 128 KiB instead of 16 KiB.
3. The probe asserts both halves and joins the macOS-update checklist
   with the contract and stage-2 probes.
