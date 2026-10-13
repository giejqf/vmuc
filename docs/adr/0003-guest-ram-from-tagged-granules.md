# ADR 0003 — Guest RAM is built from 128 KiB ledger-tagged granules

**Status:** accepted, 2026-10-13. It changes the "Memory" row of
plan.md's locked decisions and a memory-manager invariant in AGENTS.md
(both updated). Evidence:
[the guest RAM accounting experiment](../experiments/2026-10-13-guest-ram-accounting.md).

## Context

Guest RAM was one anonymous `mmap`, and a release was `hv_vm_unmap` →
`mmap(MAP_FIXED)` over the range → `hv_vm_map`. macOS bills a page of
such memory twice in `phys_footprint` once both the VMM and a vCPU have
touched it, and every device touches guest RAM from the host: page
cache read through virtio-blk or virtiofs, network buffers, the kernel
image vmuc copies in. A cold 1 GiB read cost 2 GiB of footprint, and
jetsam, Activity Monitor and `vmuc stat` all used the doubled number.

`hv_vm_allocate` is billed once: it maps named, ledger-tagged memory
entries. But a page of such an entry is freed only with the entry, and
`hv_vm_allocate` makes 128 MiB entries, so on its memory nothing vmuc
can do releases a reported range: `madvise`, `MADV_ZERO` and partial
overwrites all leave the pages billed, and `mmap(MAP_FIXED)` releases
by turning the range back into twice-billed memory.

## Decision

- Guest RAM is reserved like `hv_vm_allocate` reserves it, then backed
  by one ledger-tagged entry per 128 KiB granule
  (`vmuc_hv::guest_memory`). 128 KiB is the smallest block the guest
  reports free (reporting order 5).
- A release is `hv_vm_unmap` → fresh granules over the range →
  `hv_vm_map`, with reported ranges aligned inward to 128 KiB instead of
  16 KiB.
- `crates/vmuc-hv/tests/accounting.rs` asserts both properties and runs
  with the contract and stage-2 probes after every macOS update.

## Consequences

- Shared pages are billed once. An idle guest after reclaim fell from
  443–449 to 288 MiB on the same instance, and a 1 GiB read costs
  1 GiB (the experiment has the benchmark rows).
- A release now makes two Mach calls per granule: release latency per
  range rose (rows in mem-reclaim), still far below the guest's own
  pacing. Building RAM costs about 14 ms per GiB at start.
- Free memory in blocks below 128 KiB stays billed, as it already did
  since the guest reports from order 5; balloon inflation, which hands
  over single 4 KiB pages, now releases only whole 128 KiB runs.
- The VM map holds one entry per granule (32 768 for 4 GiB). If that
  ever matters, the granule can grow with the RAM size together with the
  reporting order.
- Rejected: `hv_vm_allocate` itself (no release); `mmap` plus a
  corrected number in `vmuc stat` (macOS itself would still see double);
  granules of 16 KiB (eight times the entries for blocks the guest never
  reports).

## Rule text

AGENTS.md's memory-manager invariants read:

- The only sequence that returns guest memory to macOS is
  `hv_vm_unmap(range)` → fresh ledger-tagged granules over the same host
  address (`vmuc_hv::guest_memory::refresh`) → `hv_vm_map(range)` →
  acknowledge the report. Guest RAM comes only from
  `guest_memory::allocate`: plain anonymous memory is billed twice for
  every page the host and the guest both touch. `madvise`, `MADV_ZERO`
  and `hv_vm_unmap` alone release nothing.
- Host pages are 16 KiB and release granules 128 KiB. Align reported
  ranges **inward** to the granule; anything smaller stays billed.

and plan.md's "Memory" row reads "ledger-tagged 128 KiB granules lazily
populated, mapped once; …".
