# Where a virtio round trip's time goes

**Question.** A guest `open()`+`close()` on a virtiofs share took 110 µs
and a 4 KiB O_DIRECT read from virtio-blk takes 28 µs on an M4 Pro. How
much of that is ours (exits, wake-ups, host work), how much is the
guest, and what can be cut?

**Method.** Ubuntu 24.04 guest, 4 vCPUs, release build, macOS 26.6.2.

- Guest-visible latency: `dd if=/dev/vda of=/dev/null bs=4k count=100000
  iflag=direct` (QD1 block reads, host page cache warm) and the
  open+close loop now in `tests/bench/fs/workload.py`.
- Exit cost: `crates/vmuc-hv/tests/exits.rs` loops a vCPU on an MMIO load
  (data abort, PC moved by us) and on HVC, GIC present.
- Wake-up and interrupt timing: temporary instrumentation, kept as
  `2026-10-12-virtio-round-trip.patch` (apply to `crates/vmuc-virtio`):
  doorbell ring → worker running ("wake"), SPI asserted → driver's
  InterruptACK ("raise to ack"), and an optional spin before parking
  (`VMUC_SPIN_NS`). virtiofs per-opcode host times came from a similar
  throwaway patch on the request pool.

**Results.**

| Piece | Cost |
|-------|-----:|
| MMIO load exit, round trip | 0.99 µs |
| HVC exit, round trip | 0.76 µs |
| Doorbell → parked worker running | 4.2 µs (blk), 2.7 µs (fs) |
| SPI asserted → guest's InterruptACK (WFI wake, IRQ entry, status read exit, ack exit) | 7.5 µs |
| `hv_gic_set_spi` itself | 0.6 µs |
| virtiofs pool hop there and back | 1.7 + 1.6 µs |
| virtiofs OPEN on the host (before / after one-syscall opens) | 15 / ~7 µs |
| FLUSH, RELEASE on the host | < 1 µs |

A blk QD1 read: ~1 (kick exit) + 4.2 (wake) + ~2 (preadv from the host
page cache, used ring) + 7.5 (interrupt to ack) ≈ 15 µs is ours; the
other ~13 µs is guest software (block layer, scheduler, `dd` itself).

- Guest `idle=poll` (no WFI) changes nothing: 28.5 µs per read.
- Spinning 50 µs before parking removes the 4 µs wake: blk 28.4 →
  25.5 µs; virtiofs unchanged (its requests park on the pool instead).
- virtiofs open+close: three synchronous round trips (OPEN, FLUSH,
  RELEASE). Answering FLUSH with ENOSYS (the kernel then stops sending
  it) took it from 110 to 80 µs; one syscall for opens and lookups on
  macOS (`O_NOFOLLOW_ANY` / `AT_SYMLINK_NOFOLLOW_ANY` on the full path,
  instead of opening the parent and using `*at`) to 72 µs. The small-file
  workload improved 21–53% (`docs/benchmarks/fs.md`).

**Verdict.** Exits are cheap (~1 µs); none of ours dominates. A round
trip is roughly half our wake-up and interrupt path, half guest kernel.
For virtiofs the lever is fewer round trips and cheaper host calls, both
taken. A spin-before-park would buy ~3 µs (10%) per QD1 round trip at the
cost of CPU after every burst; not adopted while idle cost and battery
weigh more than QD1 latency. Revisit with a bounded, adaptive poll if a
benchmark target needs it.
