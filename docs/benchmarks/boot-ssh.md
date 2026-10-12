# boot-ssh

`vmuc start NAME` until the first `vmuc shell NAME -- true` returns:
power-on to a working ssh session through the vsock tunnel, image
already converted. "start" is `vmuc start` alone (it returns once the
agent answers and sshd's banner arrives); "to ssh" adds the first ssh
session, handshake included. Target: < 3 s (Lima on vz ≈ 10 s+).

Probe: `tests/bench/boot-ssh/run.sh NAME [RUNS]`; the instance is
stopped between runs. Not first boots: cloud-init's first boot of an
instance also generates host keys and installs the agent.

| Date | Host | Instance | Kernel (Image sha256 prefix) | Runs | start (ms) | to ssh (ms) | to ssh max (ms) |
|------|------|----------|------------------------------|------|------------|-------------|-----------------|
| 2026-10-11 | Apple M4 Pro, macOS 26.6.2 | ubuntu | kernel c084b8a9a181 | 5 | 2757 | 3172 | 3739 |
| 2026-10-12 | Apple M4 Pro, macOS 26.6.2 | Lima 2.2.1 vz ubuntu-24.04 | Ubuntu stock | 3 | 7696 | 7730 | 7887 |
| 2026-10-12 | Apple M4 Pro, macOS 26.6.2 | ubuntu | kernel c084b8a9a181 | 5 | 3037 | 3254 | 3697 |
