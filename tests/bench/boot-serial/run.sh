#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# boot-serial: power-on to init on the serial console, and power-on to
# power-off of the whole VMM process, for the busybox initramfs.
#
#   tests/bench/boot-serial/run.sh [RUNS] [CPUS]     (defaults: 10, 4)
#
# Needs Apple silicon, a release build of vmuc (built and signed here),
# guest/kernel/out/Image and guest/kernel/initramfs/busybox.sh's cpio.
# Appends one dated row to docs/benchmarks/boot-serial.md.
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)
runs=${1:-10}
cpus=${2:-4}
kernel=$root/guest/kernel/out/Image
initrd=$root/guest/kernel/out/busybox-initramfs.cpio
extra=${VMUC_BENCH_CMDLINE:-}
cmdline="console=ttyAMA0 panic=-1 quiet vmuc.test=poweroff $extra"

[[ -f $initrd ]] || "$root/guest/kernel/initramfs/busybox.sh"
[[ -f $kernel ]] || { echo "missing $kernel" >&2; exit 1; }
(cd "$root" && cargo build -q --release -p vmuc-cli)
vmuc=$root/target/release/vmuc
codesign --force --sign - --entitlements "$root/vmuc.entitlements" "$vmuc" 2>/dev/null

python3 -I - "$vmuc" "$kernel" "$initrd" "$cmdline" "$runs" "$cpus" "$root" <<'PY'
import datetime, platform, re, statistics, subprocess, sys, time
vmuc, kernel, initrd, cmdline, runs, cpus, root = sys.argv[1:]
walls, inits = [], []
for _ in range(int(runs)):
    t0 = time.perf_counter()
    out = subprocess.run([vmuc, "_boot", "--kernel", kernel, "--initrd", initrd,
                          "--cmdline", cmdline, "--cpus", cpus, "--memory", "1GiB"],
                         stdin=subprocess.DEVNULL, capture_output=True, text=True, timeout=60)
    walls.append((time.perf_counter() - t0) * 1000)
    m = re.search(r"VMUC_INIT_OK uptime=([0-9.]+)", out.stdout)
    if not m or "PowerOff" not in out.stderr:
        sys.exit(f"boot failed:\n{out.stdout[-2000:]}\n{out.stderr[-2000:]}")
    inits.append(float(m.group(1)) * 1000)
kver = re.search(r"kernel=(\S+)", out.stdout).group(1)
mac = subprocess.run(["sw_vers", "-productVersion"], capture_output=True, text=True).stdout.strip()
chip = subprocess.run(["sysctl", "-n", "machdep.cpu.brand_string"], capture_output=True, text=True).stdout.strip()
extra = cmdline.split("vmuc.test=poweroff", 1)[1].strip() or "-"
row = (f"| {datetime.date.today()} | {chip}, macOS {mac} | {kver} | {cpus} | {runs} | "
       f"{statistics.median(inits):.0f} | {statistics.median(walls):.0f} | {max(walls):.0f} | `{extra}` |")
path = f"{root}/docs/benchmarks/boot-serial.md"
with open(path, "a") as f:
    f.write(row + "\n")
print(row)
PY
