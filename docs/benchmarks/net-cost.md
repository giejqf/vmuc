# Network cost by rate

Probe: `tests/bench/net-cost/run.py`. An Ubuntu instance (4 vCPUs,
4 GiB) on vmnet's shared network sends to a sink on the host through its
gateway for 10 s: as fast as one TCP flow goes, then paced by the guest
kernel (`SO_MAX_PACING_RATE`) at 10, 1 and 0.1 Gbit/s; then it idles for
10 s. Each cell is the CPU of the VMM / the vmnet helper / the guest's
sending process, in % of one core (the VMM's includes the vCPUs); the
first also gives the rate reached, Gbit/s.

| Date | Host | Commit | Max: Gbit/s: VMM / helper / guest | 10 Gbit/s | 1 Gbit/s | 0.1 Gbit/s | Idle: VMM / helper |
|------|------|--------|---|---|---|---|---|
| 2026-10-14 | Apple M4 Pro, macOS 26.6.2 | 48ff8bc | 94.8: 227 / 103 / 98 | 111 / 29 / 6 | 31 / 10 / 2 | 18 / 6 / 1 | 0 / 0 |
| 2026-10-14 | Apple M4 Pro, macOS 26.6.2 | 38eac04 | 137.4: 202 / 105 / 90 | 112 / 28 / 6 | 33 / 11 / 3 | 15 / 5 / 1 | 0 / 0 |
