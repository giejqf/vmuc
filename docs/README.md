# vmuc documentation

For using vmuc, start with the [quick start](quickstart.md); every
setting is in the [configuration reference](config.md), generated from
the schema. The rest of this directory is how vmuc was built and why,
kept as it was written.

| Where | What | When it is written |
|---|---|---|
| [plan.md](plan.md) | The product, the locked decisions, the architecture, the CLI, the milestones | Changed in the same commit as any decision it records |
| [feasibility.md](feasibility.md) | The study before the code: what Hypervisor.framework, vmnet and the guest kernel allow, with the evidence | Once, before M0 |
| [architecture/](architecture/) | How a part works where the code alone does not say it (the guest kernel and its patches) | When a part needs it |
| [adr/](adr/) | Decision records: retiring a mechanism, choosing between designs, a rule a later change must not undo | Before the change they allow |
| [experiments/](experiments/) | A question answered with data: question, method, result, verdict | Dated, one per question |
| [benchmarks/](benchmarks/README.md) | Measurements that are repeated, a dated row per run, each with its probe in `tests/bench/` | Every run appends |
| [logs/](logs/) | Root-cause hunts, incidents and milestones as they happened | Dated |

The decision records so far:

| ADR | Decision |
|---|---|
| [0001](adr/0001-helper-shared-memory-rings.md) | Frames between the VMM and the vmnet helper go through shared-memory rings |
| [0002](adr/0002-guest-kernel-reports-everything-per-pass.md) | The guest kernel reports all its free memory each pass (patch 0001) |
| [0003](adr/0003-guest-ram-from-tagged-granules.md) | Guest RAM comes from ledger-tagged granules, so the host bills each page once |
| [0004](adr/0004-no-host-driven-balloon-targets.md) | No host-driven balloon targets; pressure drops caches and compacts |
| [0005](adr/0005-virtiofs-dax-opt-in.md) | virtiofs DAX is opt-in per share |
| [0006](adr/0006-replay-host-changes-to-guest-watchers.md) | Host changes in a share are replayed to the guest's file watchers (patch 0002) |
| [0007](adr/0007-suspend-instances.md) | Instances can be suspended and resumed |
| [0008](adr/0008-guest-kernel-smstop-only-when-needed.md) | The guest kernel issues SMSTOP only when something is on (patch 0003) |
| [0009](adr/0009-duplicate-stopped-instances.md) | A stopped instance can be duplicated |
| [0010](adr/0010-cli-contract-for-scripts-and-agents.md) | The CLI is a contract for scripts and agents |

Contributors and coding agents: the working rules are in
[AGENTS.md](../AGENTS.md).
