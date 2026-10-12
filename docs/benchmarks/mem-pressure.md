# Host pressure

Probe: `tests/bench/mem-pressure/run.sh NAME WORKLOAD LEVEL [SECONDS]`.
One guest workload while the VMM acts on a simulated macOS pressure
level (`vmuc _pressure`), after a 60 s settle and a reclaim. Footprint is
the VMM's `phys_footprint` sampled every second (mean / p95 / max);
balloon episodes count changes of direction; available is the guest's
lowest MemAvailable; CPU is the VMM's (vCPUs and devices), percent
of one core; stall is the guest's memory PSI `some` time during
the run; major faults and OOM kills from `/proc/vmstat`. Policy: whether
the instance lets the balloon inflate, and the commit. Ubuntu 24.04,
4 vCPUs, 4 GiB unless noted.

| Date | Host | Instance | Policy | Workload | Level | Seconds | Footprint MiB mean / p95 / max | Balloon max MiB | Balloon episodes | Available min MiB | CPU % | Stall s | Major faults | OOM kills | Result |
|------|------|----------|--------|----------|-------|---------|-------------------------------|-----------------|------------------|-------------------|-------|---------|--------------|-----------|--------|
