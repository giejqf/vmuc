# ADR 0004 — No host-driven balloon targets

**Status:** accepted, 2026-10-13. Evidence:
[E1](../experiments/2026-10-13-e1-pressure-policy.md).

## Context

vmuc's memory manager had two ways to answer macOS memory pressure: ask
the guest to drop its caches and compact (always on), and, with
`memoryPolicy.pressureBalloon`, inflate the balloon on critical pressure
by a quarter of the guest's available memory at a time, down to
`minAvailable`. The second was off by default because similar policies
had collapsed idle guests and oscillated elsewhere (feasibility.md), and
it waited for data.

E1 measured both against five guest workloads on guest RAM that is
billed once (ADR 0003). Dropping caches returned cache nobody needed at
once and cut a build's footprint by a fifth at no cost. The balloon
never returned more than that. It can release memory (cache read in
order and evicted to fill it), but it inflated an idle guest to its
floor without moving the footprint, pinned a busy guest's working set
into stalls and an oscillation, let bursts of allocation push the guest
below its floor, and left a build's footprint higher than no policy at
all. The pressure rules for it (step, floor, spacing, calm, PSI, stall
back-off) were complexity guarding a mechanism that did not pay.

## Decision

- vmuc never sets a balloon target. `memoryPolicy.pressureBalloon` and
  `memoryPolicy.minAvailable` are gone from the configuration, the
  `balloon_target` control request and the balloon line of `vmuc stat`
  are gone, and the responder only drops caches and compacts on warn or
  critical pressure, at most every 30 s.
- The virtio-balloon device stays: it carries free page reporting and
  the guest's memory statistics. `num_pages` is always 0 and the device
  no longer offers `VIRTIO_BALLOON_F_DEFLATE_ON_OOM`.
- `vmuc _pressure` stays, for measuring the responder.

## Consequences

- Less to configure and nothing that can pin guest memory. An instance
  config that still names `memoryPolicy` is rejected as an unknown key
  (no instance had one when this landed).
- Under sustained pressure a busy guest keeps what it uses; vmuc returns
  only what the guest frees or what dropping caches frees. That is all
  E1 found returnable.
- The probe that measured balloon releases, `balloon-release.sh`, is
  removed with the request it used; it remains in history at the commit
  that recorded E1.
- Bringing any host-driven shrink back needs an experiment in which it
  beats dropping caches on `tests/bench/mem-pressure` (AGENTS.md).
