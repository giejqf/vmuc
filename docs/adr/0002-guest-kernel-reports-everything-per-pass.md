# ADR 0002 — The guest kernel reports its whole free list on every pass

**Status:** accepted, 2026-10-13; measured in
[the reporting-pace experiment](../experiments/2026-10-13-reporting-pace.md).

## Context

Free page reporting is how guest memory gets back to macOS (plan.md,
"Memory"), and the v0.1 target is 90 % of freed memory back within
10 s. Linux paces reporting itself: a pass runs two seconds after the
first free, and reports at most a sixteenth of each free list (plus one
batch of 32 blocks) before it hands the rest to the next pass, two
seconds later. The budget's comment gives the reason ("we avoid
processing any given list for too long") and the result: an idle
system fully reported in about 30 s.

On vmuc that pacing is the whole delay. In the nine recorded runs on
the stock kernel at order 5, 90 % of a freed 2 GiB took 6.2–10.4 s to
come back, and two runs missed the target (87 % and 77 % back at
10 s). The host side is not the limit: it releases a reported range in
25–50 µs at the median.

Neither knob that exists helps. `page_reporting_order` sets the
smallest block reported, not how much a pass may report, and Linux
6.18 has no parameter for the delay or the budget.

## Decision

vmuc's guest kernel carries one patch,
`guest/kernel/patches/0001-mm-page_reporting-report-the-whole-free-list-per-pass.patch`:
the per-list budget becomes the whole list plus one batch instead of a
sixteenth of it. The two-second delay that lets reports accumulate
stays. `guest/kernel/build.sh` applies `patches/*.patch` after
extraction. Patch files carry `GPL-2.0-only`, the license of the code
they change.

## Consequences

- 90 % of a freed 2 GiB is back in 0.5–2.1 s instead of 6.8–8.3 s on the
  same instance, and the release p50 falls from 50 to 25 µs. Idle CPU
  (0.23–0.27 %) and the footprint after reclaim (354 vs 355 MiB) do not
  move.
- A pass that finds a lot of free memory reports it all, one batch of
  32 ranges at a time with the zone lock dropped around each batch, as
  upstream's passes already do; only the number of batches per pass
  changes.
- Every kernel bump must re-apply the patch; one that stops applying is
  rebased or dropped in the bump's change (guest-kernel.md, "Bumping the
  version"). The target holds only for vmuc's kernel: an image that
  brings its own kernel (`images[].kernel`) gets upstream's pace.
- Rejected: a shorter delay (also a patch, and it adds wakeups instead
  of reporting more per wakeup); balloon free page hinting (hints are
  for migration and need dirty tracking to be safe to discard); and
  accepting the miss.
