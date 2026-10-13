#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# boot-ssh: `vmuc start NAME` until a first `vmuc shell NAME -- true`
# succeeds, i.e. power-on to a working ssh session, warm image cache.
#
#   tests/bench/boot-ssh/run.sh NAME [RUNS]      (default 5 runs)
#
# The instance must exist (`vmuc create`); it is stopped first and after.
# Appends a dated row to docs/benchmarks/boot-ssh.md. Target: < 3 s.
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)
name=${1:?usage: run.sh NAME [RUNS]}
runs=${2:-5}
(cd "$root" && cargo build -q --release -p vmuc-cli && \
    cargo build -q --release -p vmuc-agent --target aarch64-unknown-linux-musl)
vmuc=$root/target/release/vmuc
codesign --force --sign - --entitlements "$root/vmuc.entitlements" "$vmuc" 2>/dev/null
"$vmuc" stop "$name" >/dev/null 2>&1 || true

python3 -I - "$vmuc" "$name" "$runs" "$root" <<'PY'
import datetime, os, statistics, subprocess, sys, time
vmuc, name, runs, root = sys.argv[1:]
starts, totals = [], []
for _ in range(int(runs)):
    t0 = time.perf_counter()
    subprocess.run([vmuc, "start", name], check=True, capture_output=True)
    t1 = time.perf_counter()
    subprocess.run([vmuc, "shell", name, "--", "true"], check=True, stdin=subprocess.DEVNULL)
    t2 = time.perf_counter()
    starts.append((t1 - t0) * 1000)
    totals.append((t2 - t0) * 1000)
    subprocess.run([vmuc, "stop", name], check=True, capture_output=True)
mac = subprocess.run(["sw_vers", "-productVersion"], capture_output=True, text=True).stdout.strip()
chip = subprocess.run(["sysctl", "-n", "machdep.cpu.brand_string"], capture_output=True, text=True).stdout.strip()
image = os.environ.get("VMUC_KERNEL") or f"{root}/guest/kernel/out/Image"
kernel = subprocess.run(["shasum", "-a", "256", image], capture_output=True, text=True).stdout[:12]
row = (f"| {datetime.date.today()} | {chip}, macOS {mac} | {name} | kernel {kernel} | {runs} | "
       f"{statistics.median(starts):.0f} | {statistics.median(totals):.0f} | {max(totals):.0f} |")
with open(f"{root}/docs/benchmarks/boot-ssh.md", "a") as f:
    f.write(row + "\n")
print(row)
PY
