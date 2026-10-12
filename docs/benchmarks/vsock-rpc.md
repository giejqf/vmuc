# vsock RPC

Probe: `tests/bench/vsock-rpc/run.sh NAME [CALLS]`. Round trips of a
guest-agent `mem_info` call as vmuc makes every agent call: connect to
the instance's control socket, open a vsock connection to the agent
(one per call), send the request, read the reply. Measured from the
host CLI, so it includes the unix-socket hop and the agent's thread
start.

Target: p50 < 0.3 ms (ArcBox's HV backend: 0.30 ms).

| Date | Host | Instance | Call | Calls | p50 µs | p90 µs | p99 µs | max µs |
|------|------|----------|------|------:|-------:|-------:|-------:|-------:|
| 2026-10-12 | Apple M4 Pro, macOS 26.6.2 | ubuntu | mem_info | 2000 | 138 | 176 | 239 | 632 |
