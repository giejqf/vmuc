# 2026-10-13 — E1: the host-pressure policy against real guest workloads

**Question.** The host-pressure responder (plan.md, "Memory manager")
drops the guest's caches on warn and, with `memoryPolicy.pressureBalloon`,
inflates the balloon on critical. It had never met pressure data, so the
balloon part stayed off. Does either give memory back to macOS while a
guest works, what does the guest pay, does it avoid ArcBox's incidents
(an idle guest collapsed to its floor; a balloon oscillating on a
pressured host; a workload starved), and can the balloon be on by
default?

**Method.** `tests/bench/mem-pressure/run.sh e1 WORKLOAD LEVEL` on one
instance (Ubuntu 24.04 from the template, 4 vCPUs, 4 GiB, the patched
kernel, guest RAM from tagged granules so footprints are billed once,
ADR 0003). The level is simulated for the run with `vmuc _pressure`
(system-wide simulation needs root, real pressure squeezes the whole
Mac). Workloads: idle; *cache*, re-reading a 3 GiB file (a hot working
set); *cold*, reading it once and idling; *anon*, 2.5 GiB bursts held
30 s, freed for 30 s; *build*, vmuc's kernel with `-j4`. Policies:

- **P0**, the plan's (commit d6ca13a): warn and critical drop caches and
  compact at most every 30 s; critical with the balloon allowed inflates
  by a quarter of the guest's available memory, never below
  `minAvailable` (256 MiB), at most every 60 s; deflate after 120 s of
  calm, or at once when guest memory PSI `some` passes 10 % (the rule
  plan.md had and the code lacked, added for E1).
- **P1**, a candidate (435a776): instead of dropping caches, the guest
  reclaims a quarter (warn) or half (critical) of its reclaimable memory
  through its own LRU (`memory.reclaim` of the root cgroup) and compacts;
  after a stall the balloon stays down until the host has been calm.

Both commits are kept on the unmerged branch `feat/e1-reclaim`; P0's
code is what landed with ADR 0003, plus the stall rule this entry adopts.
Rows in [mem-pressure](../benchmarks/mem-pressure.md); the four rows
from before the accounting fix are not used for footprints.

**Result.**

A hot working set (cache workload, 300 s each; footprint mean / max MiB):

| Level, balloon | P0 footprint | P0 guest cost | P1 footprint | P1 guest cost |
|---|---|---|---|---|
| normal | 3347 / 3376 | — (10 509 GiB read) | | |
| warn | 3343 / 3378 | 109 major faults, 10 437 GiB | 3352 / 3403 | 79 faults, 0.2 s stall, 10 359 GiB |
| critical, off | 3343 / 3393 | 111 faults, 10 446 GiB | 3351 / 3401 | 109 faults, 0.3 s stall, 10 452 GiB |
| critical, on | 3308 / 3473 | **10 balloon episodes, 13.0 s stall, 2926 faults, 8721 GiB (−17 %)** | 3342 / 3468 | 2 episodes, 2.7 s stall, 716 faults, 9474 GiB (−10 %) |
| pulse (3 min critical / 3 min calm, 720 s), on | 3355 / 3690 | **12 episodes, 14.8 s stall**, 3766 faults | 3360 / 3480 | 4 episodes, 5.8 s stall, 1315 faults |

The balloon elsewhere (P0 unless noted):

| Run | Footprint mean / max | Balloon max | Guest available, lowest | Stall | Major faults | Result |
|---|---|---|---|---|---|---|
| idle, critical 600 s | 308 / 333 | **3552 MiB** | **254 MiB** (the floor) | 0 | 96 | — |
| anon, critical 400 s | 1642 / 2858 | 1301 MiB | **85 MiB** | 4.7 s | 58 073 | 7 bursts, slowest 0.6 s, no OOM kill |
| anon, critical 400 s, P1 | 1650 / 2843 | 1299 MiB | 474 MiB | 3.1 s | 15 307 | 7 bursts, slowest 0.9 s |

Cache nobody needs any more (cold, warn, balloon off, 300 s):

| Policy | Footprint mean / max | Major faults |
|---|---|---|
| P0, drop caches | **521** / 3341 | 82 |
| P1, trim | 1430 / 3341 | 46 |

A real build (vmuc's kernel without debug info, `-j4`, about 170 s):

| Level, balloon | Footprint mean / max | Balloon max | Major faults | Build |
|---|---|---|---|---|
| normal | 1162 / 1407 | — | 118 | 170 s |
| warn (P0) | **918** / 1103 | — | 360 | 171 s |
| critical, on (P0) | 1440 / 1862 | 1940 MiB | 342 | 176 s |
| critical, on (P1) | 1513 / 2035 | 1983 MiB | 213 | 174 s |

With debug info and BTF the build was OOM-killed at normal pressure
(pahole outgrows a 4 GiB guest), so the workload drops them.

What the balloon can return, from `tests/bench/mem-pressure/balloon-release.sh`:
with 2 GiB of a file cached (read in order), a 3.2 GiB balloon target
lowered the footprint by 1365 and 1288 MiB in two runs, with the
released counter rising by as much.

**Verdict.**

1. **A working set cannot be taken back without the guest paying.**
   Under every level the hot workload's footprint stayed at its working
   set (3.3 GiB): dropped or trimmed cache was read back within a second
   (cheaply, from the host's own file cache).
2. **Dropping caches returns what can be returned.** Cache nobody
   uses comes back at once (521 MiB mean against 1430 for a gradual
   trim), and during a build warn's drop every 30 s cut the mean
   footprint by a fifth at no cost in build time. P1's trim was no
   gentler where it mattered (the hot workload refilled either way), so
   it is not adopted and the plan's warn rule stands.
3. **The balloon never did better than dropping caches, and pinned
   memory doing it.** It can release memory: cache read in order and
   evicted to fill it came back almost entirely. But on the idle guest
   it inflated to the floor (3.5 GiB) without moving the footprint, as
   free memory had already gone back through reporting; on the hot
   workload it pinned memory the guest needed (13 s of stall, −17 %
   throughput, ten episodes in five minutes: ArcBox's oscillation); with
   bursts of anonymous memory, inflation and allocation raced and the
   guest's available memory fell to 85 MiB, below the floor; and during
   the build the footprint was higher with the balloon (1440–1513 MiB
   mean) than without it (1162) or with warn alone (918). Why the build
   came out higher was not established; a churning workload frees
   scattered pages, and pages the balloon pins that do not fill whole
   128 KiB granules stay billed, which would explain it.
4. **Decisions.** `pressureBalloon` stays off by default; nothing
   measured argues for turning it on. P1's stall rule is adopted
   (episodes 10 → 2, stall 13 → 2.7 s on the hot workload, 12 → 4 under
   pulses) for anyone who does. Retiring host-driven balloon targets
   altogether is a question for an ADR; this entry is its evidence.
