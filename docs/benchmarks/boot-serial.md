# boot-serial

Power-on to init on the serial console, and power-on to power-off of the
whole `vmuc _boot` process, booting the busybox initramfs
(`guest/kernel/initramfs/busybox.sh`) with `vmuc.test=poweroff`.
Target: kernel to `init` < 400 ms.

Probe: `tests/bench/boot-serial/run.sh [RUNS] [CPUS]`. "Kernel → init" is
the guest's own `/proc/uptime` when init runs; "process" is wall time
from spawning `vmuc` to its exit, so it includes VM creation, loading
the 31 MiB kernel, boot, power-off and teardown. Medians unless noted.

| Date | Host | Kernel | vCPUs | Runs | Kernel → init (ms) | Process (ms) | Process max (ms) | Extra cmdline |
|------|------|--------|-------|------|--------------------|--------------|------------------|---------------|
| 2026-10-11 | Apple M4 Pro, macOS 26.6.2 | 6.18.55-vmuc | 4 | 10 | 620 | 898 | 1159 | `-` |
| 2026-10-11 | Apple M4 Pro, macOS 26.6.2 | 6.18.55-vmuc | 4 | 10 | 120 | 170 | 332 | `initcall_blacklist=raid6_select_algo` |
| 2026-10-11 | Apple M4 Pro, macOS 26.6.2 | 6.18.55-vmuc | 1 | 10 | 115 | 163 | 327 | `initcall_blacklist=raid6_select_algo` |
| 2026-10-12 | Apple M4 Pro, macOS 26.6.2 | 6.18.55-vmuc | 4 | 10 | 120 | 178 | 455 | `-` |
