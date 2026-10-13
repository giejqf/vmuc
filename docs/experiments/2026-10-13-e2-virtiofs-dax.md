# 2026-10-13 — E2: virtiofs DAX through per-file `hv_vm_map`

**Question.** With DAX the guest maps a shared file's pages from a window
in its physical address space instead of copying them into its page
cache. Can vmuc do that on Hypervisor.framework within its rules
(`hv_vm_map` refuses overlapping maps), what does it cost and save, and
what can go wrong?

**Method.**

1. `crates/vmuc-hv/tests/dax.rs` on bare HV with a real vCPU: a
   `MAP_SHARED` file mapping given to the VM; coherence both ways;
   footprint of guest loads and stores; reusing a window slot (unmap then
   map another file, also into a hole of a larger mapping, also for
   anonymous memory; map over a live mapping); a guest load past a host
   truncation; read-only and private mappings; the cost of a 2 MiB map +
   unmap.
2. A prototype (`mounts[].dax`): virtio-mmio shared memory regions, a
   1 GiB window per DAX share above guest RAM, FUSE SETUPMAPPING and
   REMOVEMAPPING in `vmuc-fs`, the guest mounting with `dax=inode`.
3. `tests/bench/fs/run.sh` (with `FS_DAX=1`) for throughput and metadata
   ([fs](../benchmarks/fs.md)), and `tests/bench/fs-memory/run.sh` for
   what reading 2 GiB of large files and 2000 small ones from a share
   costs the host ([fs-memory](../benchmarks/fs-memory.md)). M4 Pro,
   macOS 26.6.2, Ubuntu 24.04, 4 vCPUs, 4 GiB.

**Result.**

On bare HV:

| Question | Answer |
|---|---|
| A `MAP_SHARED` file range as guest memory | works; host and guest see each other's writes at once |
| Footprint of 64 MiB of guest loads and stores | 0.2 MiB: file pages are the file cache's, not the VMM's |
| Map another range into a freed slot, or a hole of a larger mapping | works, for files and for anonymous memory |
| Map over a live mapping | refused (`hv_vm_map` error) |
| Guest load past a host truncation | a data abort exit, which vmuc answers as unclaimed MMIO (reads zero) |
| Read-only shared host mapping into the VM | refused |
| Private writable mapping of a read-only fd | maps, but keeps showing old data after a host write |
| 2 MiB map + unmap | 0.7 µs |

Through the prototype (the rows at 7d82ef4, and ea89cec for the copy path's fs row):

| | No DAX | DAX |
|---|---|---|
| Host footprint after the guest reads 2 GiB from the share | +1820 to +2028 MiB | **+0 to +1 MiB** |
| Guest page cache growth | 1792 to 2048 MiB | 0 |
| Cold 2 GiB read (files written by the host) | 1.0 s | 1.1 s (3.0 s before read-ahead) |
| 2000 files of 64 KiB | 0.5 to 0.6 s | 0.5 s |
| fs: 1 GiB sequential write / read (MB/s) | 3007 / 3810 | 3606 / 5802 |
| fs: 5000 small files read / open+close | 1002 ms / 73.9 µs | 1153 ms / 75.8 µs |
| Idle after reclaim | 287 to 293 MiB | 307 to 311 MiB (the window's struct pages) |

A 4 GiB window instead of 1 GiB cost 55 MiB of guest memory for its
struct pages (idle 362 against 307 MiB) and read 2 GiB in 2.7 s against
3.0 s: the window was not the bottleneck. The cold-read cost was the
stage-2 fault on each 16 KiB page's first touch; `madvise(MADV_WILLNEED)`
on each new mapping, so the page is resident when the guest faults,
brought it from 3.0 s to the level of the copy path.

Two things the prototype had to get right, both found live:

1. Linux maps by node (`fh` is -1), so the server opens the node itself.
2. A shared mapping needs a writable descriptor even for a read mapping
   (row 6), and a private one goes stale (row 7). So DAX is per file:
   the server marks with `FUSE_ATTR_DAX` only regular files this user
   can write and that are not dataless placeholders (iCloud), whose first
   touch would block a vCPU inside `hv_vcpu_run` until downloaded, and
   the guest mounts with `dax=inode`. Everything else takes the copy
   path. Linux reads `HAS_INODE_DAX` only when the reply also carries
   `INIT_EXT`.

**Verdict.**

1. DAX is feasible on Hypervisor.framework within vmuc's rules: one
   `hv_vm_map` per range, never over a live one.
2. It removes shared-file data from the VM's memory altogether: what the
   guest reads stays in macOS's file cache, billed to no one and dropped
   by macOS under pressure, instead of being copied into guest RAM (and
   held twice, guest page cache plus host file cache).
3. With read-ahead, cold reads are as fast as the copy path (1.1 against
   1.0 s for 2 GiB) and sequential reads through mapped ranges faster
   (5.8 against 3.8 GB/s); small files read 15 % slower in the fs probe
   (a SETUPMAPPING per file) and the same in fs-memory; metadata is
   unchanged. A share with DAX costs ~16 MiB of guest memory for its
   window's struct pages.
4. AGENTS.md's rule that a sub-range may take back only the same host
   address is stricter than macOS 26.6.2: any range may go where nothing
   is mapped. Unverified on macOS 15, so the rule stays and DAX never
   needs more than "unmap first".
5. DAX ships opt-in (`mounts[].dax`, default off),
   [ADR 0005](../adr/0005-virtiofs-dax-opt-in.md). Its open costs: a
   guest touching a file on a slow or hung host filesystem waits in a
   vCPU (dataless files are excluded, other slow backends are not), and a
   page past a host truncation reads zero instead of raising SIGBUS.

**Follow-up.** ADR 0005 accepted on 2026-10-13, and AGENTS.md's
`hv_vm_map` rule now says what macOS 26.6.2 does (any range may go where
nothing is mapped), noted as unverified on macOS 15.
