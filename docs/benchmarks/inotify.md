# Host changes reaching watchers in the guest

Probe: `tests/bench/inotify/cost.py`. In a writable share the guest runs
`inotifywait` on a directory and answers each event with a file the host
polls for, so a row is a round trip with no clock sync: the host creates
a file, or appends to one the guest knows, 50 times 0.3 s apart (p50 /
p95 in ms); then creates 2000 files at once (time until the guest has
seen the last, and the VMM's CPU time for it, vCPUs included); then the
VMM's CPU while the guest idles for 60 s. Ubuntu 24.04, 4 vCPUs, 4 GiB.

| Date | Host | Commit | Instance | Create p50 / p95 | Modify p50 / p95 | 2000 creates | Idle VMM CPU |
|------|------|--------|----------|-----------------:|-----------------:|-------------:|-------------:|
