#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# mem-reclaim: lima#2789 on vmuc. Fill 2 GiB of tmpfs in the guest, free
# it, and watch the host footprint come back; then drop the guest's
# caches (`vmuc reclaim`) and record the idle footprint.
#
#   tests/bench/mem-reclaim/run.sh NAME            (instance must exist)
#   VMUC_KERNEL_CMDLINE='page_reporting.page_reporting_order=5' ...
#   VMUC_KERNEL=path/to/Image ...    (the row names its sha256 prefix)
#
# Appends a dated row to docs/benchmarks/mem-reclaim.md. Targets: >= 90 %
# of freed memory back within 10 s; idle footprint < 350 MiB.
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)
name=${1:?usage: run.sh NAME}
(cd "$root" && cargo build -q --release -p vmuc-cli && \
    cargo build -q --release -p vmuc-agent --target aarch64-unknown-linux-musl)
vmuc=$root/target/release/vmuc
codesign --force --sign - --entitlements "$root/vmuc.entitlements" "$vmuc" 2>/dev/null
"$vmuc" stop "$name" >/dev/null 2>&1 || true

python3 -I - "$vmuc" "$name" "$root" "${VMUC_KERNEL_CMDLINE:-}${VMUC_KERNEL:+ (kernel $(shasum -a 256 "$VMUC_KERNEL" | cut -c1-12))}" <<'PY'
import datetime, json, subprocess, sys, time
vmuc, name, root, extra = sys.argv[1:]
extra = extra.strip()
MIB = 1 << 20
def stat():
    return json.loads(subprocess.run([vmuc, "stat", name, "--json"], check=True,
                                     capture_output=True, text=True).stdout)
def shell(cmd):
    subprocess.run([vmuc, "shell", name, "--", cmd], check=True, stdin=subprocess.DEVNULL)
subprocess.run([vmuc, "start", name], check=True, capture_output=True)
time.sleep(20)
f0 = stat()["host_footprint"]
shell("sudo mkdir -p /mnt/vmuc-bench && sudo mount -t tmpfs -o size=3G tmpfs /mnt/vmuc-bench"
      " && sudo dd if=/dev/zero of=/mnt/vmuc-bench/fill bs=1M count=2048 status=none")
time.sleep(1)
f1 = stat()["host_footprint"]
shell("sudo rm /mnt/vmuc-bench/fill")
t0 = time.monotonic()
series = []
while time.monotonic() - t0 < 20:
    series.append((time.monotonic() - t0, stat()["host_footprint"]))
    time.sleep(0.5)
at10 = [f for t, f in series if t <= 10.0][-1]
back10 = (f1 - at10) / (2048 * MIB) * 100
first90 = next((t for t, f in series if f1 - f >= 0.9 * 2048 * MIB), None)
subprocess.run([vmuc, "reclaim", name], check=True, capture_output=True)
time.sleep(10)
idle = stat()
subprocess.run([vmuc, "stop", name], check=True, capture_output=True)
mac = subprocess.run(["sw_vers", "-productVersion"], capture_output=True, text=True).stdout.strip()
chip = subprocess.run(["sysctl", "-n", "machdep.cpu.brand_string"], capture_output=True, text=True).stdout.strip()
t90 = f"{first90:.1f}" if first90 is not None else ">20"
row = (f"| {datetime.date.today()} | {chip}, macOS {mac} | {name} | `{extra or '-'}` | "
       f"{f0 // MIB} | {f1 // MIB} | {back10:.0f} | {t90} | {idle['host_footprint'] // MIB} | "
       f"{idle['release_p50_us']} / {idle['release_p99_us']} |")
with open(f"{root}/docs/benchmarks/mem-reclaim.md", "a") as f:
    f.write(row + "\n")
print(row)
print("series:", " ".join(f"{t:.1f}s:{f // MIB}" for t, f in series))
PY
