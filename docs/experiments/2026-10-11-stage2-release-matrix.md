# 2026-10-11 — Stage-2 release matrix on an M4 Pro (macOS 26.6.2)

**Question.** Which host operations give back memory a vCPU has dirtied
through its stage-2 mapping? The memory manager (M5) is built on
feasibility.md's answer, which came from ArcBox's `stage2_matrix` on
macOS 26.4 and from mvm; this re-checks it on our hardware and keeps
the probe in tree.

**Method.** `cargo test -p vmuc-hv --test stage2 -- --nocapture`
(`crates/vmuc-hv/tests/stage2.rs`). For each method, a fresh anonymous
64 MiB mapping is mapped into the VM, a real vCPU stores to every 4 KiB
page (hand-assembled loop, MMU off), the method is applied, and the
process's `phys_footprint` (`proc_pid_rusage`, `RUSAGE_INFO_V4`) is read
right away and 200 ms later. Each method gets its own mapping because
`MADV_FREE_REUSABLE` leaves sticky page state that otherwise leaks into
the next row (a first, shared-mapping version of the probe produced
nonsense for exactly that reason).

**Result.**

| Method | Footprint released (of 64 MiB) | Guest data afterwards |
|---|---|---|
| nothing (control) | 0.0 | kept |
| `madvise(MADV_DONTNEED)` | 0.0 | kept |
| `madvise(MADV_FREE_REUSABLE)` | 0.0 | (not checked) |
| `hv_vm_unmap` + `hv_vm_map`, same host range | **64.0 until the host touches the pages, then 0.0** | **all 16384 pages kept** |
| `hv_vm_unmap` + `mmap(MAP_FIXED\|MAP_ANON\|MAP_PRIVATE)` + `hv_vm_map` | 64.0 | gone (reads as zero) |
| guest re-dirties after the release | billed 64.1 again | — |

**Verdict.**

1. The invariant in AGENTS.md holds: only unmap → refresh (`mmap`
   `MAP_FIXED` over the same address) → map releases guest-dirtied
   memory, the released pages read as zero, and the guest is billed
   honestly when it touches them again.
2. **`phys_footprint` can lie.** A bare unmap/map makes the footprint
   drop by the whole range while the data is still there; the drop
   disappears once the host reads the pages. Any measurement of reclaim
   must check that the data is really gone (or re-touch the pages)
   before trusting a footprint drop. The probe now does both, and
   fails if a bare unmap/map ever starts losing data or if the real
   sequence stops releasing.
3. `madvise` releases nothing for guest-dirtied pages, as ArcBox found.

The probe runs in about a second; it belongs in the macOS-update
checklist next to the contract probe.
