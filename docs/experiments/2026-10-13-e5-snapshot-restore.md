# 2026-10-13 — E5: snapshot and restore

**Question.** Can vmuc save a running VM (vCPUs, Apple's in-kernel GIC,
the timers, devices, guest RAM) and resume it in a new process on
Hypervisor.framework? How long do a snapshot and a restore take, what
does a restored VM cost the host, and does the memory manager still
release a restored guest's freed memory? (plan.md expected restored RAM
to need excluding from reclaim, as mvm does: mvm maps it copy-on-write
from the snapshot and skips reporting on it.)

**Method.**

1. `crates/vmuc-hv/tests/snapshot.rs`, bare HV with a real vCPU:
   distinctive register values captured, clobbered and restored, read
   back by guest code; the GIC state and the vCPU restored into a new VM
   50 ms later, with the guest counter checked.
2. A prototype: vmuc-hv's `state` (the general, SIMD/FP and 45 system
   registers including pointer authentication keys and both timers, the
   GIC CPU interface registers, the vtimer offset; the GIC's device state
   through `hv_gic_state_*`), a pause in which every vCPU captures itself
   on its own thread, a snapshot directory (`state.json`, `gic.bin`,
   a sparse `ram.img` holding only resident non-zero pages), virtio-mmio
   transports saved and restored with their queues resuming at the used
   index in guest RAM, the PL011 and PL031, and `vmuc _boot --snapshot`
   / `--restore` with guest RAM either copied into fresh granules or
   mapped copy-on-write from `ram.img`.
3. `tests/bench/snapshot/run.py` ([snapshot](../benchmarks/snapshot.md)):
   a busybox guest (4 vCPUs, 2 GiB, the balloon, virtio-rng) fills 0,
   512 or 1024 MiB with random data and ticks every 100 ms; it is
   snapshotted 8 s in and restored in a new process in each RAM mode,
   from the page cache and from a copy written past it (`F_NOCACHE`, as
   after a host reboot). The restored guest then frees its data, writes
   it again and frees that; the VMM's `phys_footprint` is sampled every
   250 ms. Each restore is checked: the next tick follows, uptime
   continues, every CPU does work, virtio-rng answers.
4. `tests/e2e/tests/snapshot.rs`: the same in-process, both RAM modes.

M4 Pro, macOS 26.6.2, guest kernel 6.18.55.

**Result.**

On bare HV every listed register round-trips exactly (45 of 45 system
registers, 9 GIC CPU registers; the GIC state is 126 KB), and the
guest's counter moved 0.05 ms across a 50 ms gap: shifting the vtimer
offset by the time away keeps the guest's clock continuous.

Through the prototype (every restore passed every check):

| Data in RAM | `ram.img` written | Snapshot (vCPUs stopped) | Restore, copy | Restore, map |
|---|---|---|---|---|
| idle (89 MiB) | 89 MiB of a 2 GiB sparse file | 82 ms (0.27 ms) | 37 ms | 38 ms |
| 512 MiB | 603 MiB | 196–199 ms (0.27–0.32 ms) | 72 ms | 37–41 ms |
| 1 GiB | 1079 MiB | 352 ms (0.24 ms) | 113 ms; cold 231 ms | 39 ms; cold 31 ms |

Busybox boots to init in 120 ms on 4 vCPUs ([boot-serial](../benchmarks/boot-serial.md));
an Ubuntu instance takes 2.6 s to ssh. A restore's first tick comes
48–315 ms after the process starts, but that includes up to 100 ms of
the guest's own sleep between ticks.

What the restored VM costs, MiB of `phys_footprint` (1 GiB of data;
the booted VM was at 1119):

| | At start | Guest frees 1 GiB | Guest writes 1 GiB again | Frees it |
|---|---|---|---|---|
| Copy | 1116 | 139 | 1123 | 146 |
| Map (copy-on-write) | **40** | 62 | 1068 | 114 |

With 512 MiB the same: copy 639 → 138 → 642 → 140, map 40 → 53 → 577 → 77.

1. A snapshot costs the guest a fraction of a millisecond of stopped
   vCPUs; the rest is writing the pages it holds (about 3 GB/s into a
   sparse file).
2. Mapping restores in a constant ~40 ms, copying at the speed of
   reading the image (8–10 GB/s from the page cache, 4.7 GB/s from disk).
3. A mapped VM is billed for what the guest writes, not for what it
   holds: its untouched memory stays in macOS's file cache, which macOS
   can drop and refill from the file. Writes are billed once (no double
   billing on the copy-on-write pages).
4. Free page reporting works on mapped RAM: the reclaimer's granule
   refresh replaces copy-on-write pages of the private file mapping like
   any others, so vmuc does not need mvm's exclusion (mvm's release is a
   plain anonymous remap, and it skips file mappings).

Found on the way:

- A pause that only waited for its `hv_vcpus_exit` hung under console
  traffic: an exit for MMIO already on its way can come back instead of
  the cancel, and the vCPU re-enters the guest. vCPUs now check for a
  pause before every entry and the controller re-kicks the ones that
  have not reported. A stop had the same race (a vCPU left idling in the
  guest, teardown waiting on it), now handled the same way
  ([log](../logs/2026-10-13-lost-vcpu-cancels.md)).
- `hv_vcpu_set_simd_fp_reg` takes its vector by value in Q0, which
  stable Rust cannot declare (`simd_ffi`); it is called through a short
  `asm!` sequence.
- Three system register constants in vmuc-hv had wrong encodings (fixed,
  with a test deriving every constant from its encoding).
- A mapped VM sees later changes to the pages it has not written, so a
  snapshot must never be rewritten in place: files are written under a
  temporary name and renamed over the old.

Not covered: a full instance. The prototype restores what the busybox
guest uses (vCPUs, GIC, PL011, PL031, virtio-rng, the balloon). An
Ubuntu instance also needs its block devices drained before the pause,
its vsock connections reset (VIRTIO_VSOCK_EVENT_TRANSPORT_RESET) and its
NAT connections dropped, virtiofs's node table and open handles saved
and reopened, and a `vmuc suspend` that writes the snapshot into the
instance and a start that resumes from it.

**Verdict.**

1. Snapshot and restore work on Hypervisor.framework with its public
   macOS 15 API: a stopped vCPU, the GIC and the timers come back
   exactly, and the guest's clock continues.
2. Restore guest RAM by mapping the image copy-on-write: ~40 ms whatever
   the size, and the VM costs only what the guest writes after it.
   Reclaim needs no exception for it.
3. Suspending an instance (`vmuc suspend`, resumed by `vmuc start`) is
   feasible, and would make a suspended instance cost nothing and resume
   in well under its 2.6 s boot. It is out of scope for v0.1 in plan.md;
   building it is its own decision, with the device work listed above.

**Follow-up.** ADR 0007 builds instance suspend on this. Doing so found
two ordering faults in the prototype (a snapshot saved the GIC before
the devices had stopped; a restore put the GIC back after the devices,
lowering a line one had raised), both fixed
([log](../logs/2026-10-13-suspend-instances.md)); the numbers above
predate the fixes.
