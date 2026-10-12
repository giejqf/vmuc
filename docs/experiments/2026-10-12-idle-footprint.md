# 2026-10-12 — What an idle Ubuntu guest still costs after reclaim

**Question.** After `vmuc reclaim` an idle Ubuntu 24.04 guest (4 vCPUs,
4 GiB) kept 460–475 MiB on the host against the v0.1 target of
< 350 MiB. Where does it go, and which of it can vmuc stop paying for
without giving anything up?

**Method.** `tests/bench/mem-idle/run.sh NAME 2|3` (new): start, settle
60 s, then three rounds of `vmuc reclaim` + 15 s, reading the VMM's
`phys_footprint` after each; guest used is MemTotal − MemFree at the end.
Variants through `VMUC_KERNEL_CMDLINE` and `MEM_IDLE_STOP` (services
stopped after settling). One run with both levers was then itemized
from the guest (`/proc/meminfo`, `/proc/buddyinfo`, `/proc/zoneinfo`,
`ps`) and the host (`vmmap -summary`, `footprint`). Rows in
[mem-idle](../benchmarks/mem-idle.md). M4 Pro, macOS 26.6.2.

**Result.**

| Variant (instance `ubuntu`, stock kernel) | After reclaim 1 / 2 / 3 (MiB) | Guest used |
|---|---|---|
| as shipped | 475 / 462 / 460 | 220 |
| `swiotlb=noforce` | 394 / 380 / 375 | 207 |
| ModemManager, udisks2, multipathd, unattended-upgrades stopped | 436 / 436 / 436 | 114 |
| both | 364 / 342 / 370 | 124 |

1. **The bounce buffer.** The guest kernel logs `software IO TLB:
   mapped ... (64MB)`: arm64 sets aside a swiotlb once RAM reaches past
   4 GiB of physical address space, and vmuc's RAM starts at 1 GiB. No
   vmuc device offers `VIRTIO_F_ACCESS_PLATFORM`, so virtio never goes
   through the DMA API and nothing ever bounces; `swiotlb=noforce` gives
   back 85 MiB (the buffer, and the guest's MemTotal grows by 64 MiB).
2. **Idle daemons.** ModemManager (11 MiB RSS) and udisksd (12 MiB) serve
   modems and removable disks a VM does not have; multipathd serves SAN
   paths. Stopping them (with unattended-upgrades) dropped guest used by
   ~100 MiB but the host footprint by only 24 MiB. Only free blocks of
   128 KiB and up are reported, and what exiting processes free is
   mostly scattered pages (inferred, not measured block by block).
   `unattended-upgrade-shutdown` (19 MiB) is kept: it keeps a stop from
   interrupting dpkg mid-upgrade.
3. **Itemized, both levers, 393 MiB on the host:**

| Where | MiB |
|---|---|
| Guest MemTotal − MemFree (includes 21.5 MiB on the per-CPU free lists, 5509 pages) | 123 |
| Guest kernel reservations outside MemTotal (struct pages for 4 GiB = 64, kernel image ≈ 31, …) | 105 |
| Free guest memory in blocks below the reporting order (128 KiB), never reported | ~53 |
| The VMM process itself (12.5 MiB of empty `MALLOC_LARGE` libmalloc keeps dirty, stacks, data) | ~17 |
| Not attributed by these counters | ~95 |

The last row fits 4 KiB guest pages in use pinning whole 16 KiB host
pages, but that was not measured. The free fragments shrink to ~7 MiB
with the kernel patch of [ADR 0002](../adr/0002-guest-kernel-reports-everything-per-pass.md)
(one sample).

**Verdict.**

1. vmuc boots every guest with `swiotlb=noforce` (instance.rs).
2. The Ubuntu template disables ModemManager, udisks2 and multipathd on
   the first boot.
3. On a fresh instance from that template the idle footprint is
   **354–355 MiB** after the third reclaim (stock and patched kernel
   alike; runs 324–385), down from 460: not quite the 350 MiB target.
   What is left is the guest's working set, the struct pages of a 4 GiB
   ceiling and the VMM. The remaining levers each give something up:
   stopping `unattended-upgrades` (19 MiB, safety of upgrades across a
   stop), pinning `vm.percpu_pagelist_high_fraction` (~10 MiB, the guest
   allocator's per-CPU batching), 16 KiB guest pages (locked decision;
   a quarter of the struct-page cost and no sub-host-page pinning), or
   growing the ceiling on demand instead of reserving it at boot. The
   target is left as it is and reported as not met.
4. The struct pages scale with the ceiling: the default ceiling is
   `min(8 GiB, half the host's RAM)`, which costs another 64 MiB.
