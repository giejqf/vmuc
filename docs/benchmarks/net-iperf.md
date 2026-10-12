# Network throughput

iperf3 from the host to the guest's address, one TCP flow and four, both
directions, 8 s each, Gbit/s at the receiver. Probes:
`tests/bench/net-iperf/run.sh` (vmuc, vmnet shared mode through the root
helper) and `tests/bench/lima-vz/run.sh` (Lima on Virtualization.framework,
VZ NAT).

Target: > 10 Gbit/s single flow.

| Date | Host | VM | Network | host→guest | guest→host | host→guest ×4 | guest→host ×4 |
|------|------|----|---------|-----------:|-----------:|--------------:|--------------:|
| 2026-10-12 | Apple M4 Pro, macOS 26.6.2 | Lima 2.2.1 vz | VZ NAT | 71.9 | 108.1 | 80.3 | 138.3 |
| 2026-10-12 | Apple M4 Pro, macOS 26.6.2 | vmuc c107432 | vmnet shared, no offload | 8.3 | 4.2 | 8.3 | 4.4 |
| 2026-10-12 | Apple M4 Pro, macOS 26.6.2 | vmuc fa649f8 | vmnet shared, TSO | 52.7 | 63.6 | 49.3 | 77.8 |
| 2026-10-12 | Apple M4 Pro, macOS 26.6.2 | vmuc 50537e0 | vmnet shared, TSO, rings | 87.8 | 76.5 | 100.5 | 110.5 |
