#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# mem-idle: what an idle guest costs the host once reclaim has run.
#
#   tests/bench/mem-idle/run.sh NAME [RUNS]      (default 2)
#
# Per run: start the instance, let it settle 60 s, then three rounds of
# `vmuc reclaim` and 15 s, reading the host footprint after each. Extra
# kernel parameters come from VMUC_KERNEL_CMDLINE (and another kernel
# from VMUC_KERNEL, named in the row by its sha256 prefix); MEM_IDLE_STOP names
# guest services to stop after settling (for experiments). Appends one
# dated row to docs/benchmarks/mem-idle.md. Target: < 350 MiB.
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)
name=${1:?usage: run.sh NAME [RUNS]}
runs=${2:-2}
(cd "$root" && cargo build -q --release -p vmuc-cli)
vmuc=$root/target/release/vmuc
codesign --force --sign - --entitlements "$root/vmuc.entitlements" "$vmuc" 2>/dev/null
"$vmuc" stop "$name" >/dev/null 2>&1 || true

python3 -I - "$vmuc" "$name" "$runs" "$root" "${VMUC_KERNEL_CMDLINE:-}${VMUC_KERNEL:+ (kernel $(shasum -a 256 "$VMUC_KERNEL" | cut -c1-12))}" "${MEM_IDLE_STOP:-}" <<'PY'
import datetime, json, statistics, subprocess, sys, time
vmuc, name, runs, root, extra, stop = sys.argv[1:]
extra = extra.strip()
MIB = 1 << 20


def footprint():
    out = subprocess.run([vmuc, "stat", name, "--json"], check=True, capture_output=True, text=True)
    return json.loads(out.stdout)["host_footprint"] // MIB


rounds = []
guest_used = []
for _ in range(int(runs)):
    subprocess.run([vmuc, "start", name], check=True, capture_output=True)
    time.sleep(60)
    if stop:
        subprocess.run([vmuc, "shell", name, "--", f"sudo systemctl stop {stop}"],
                       capture_output=True, stdin=subprocess.DEVNULL)
    r = []
    for _ in range(3):
        subprocess.run([vmuc, "reclaim", name], check=True, capture_output=True)
        time.sleep(15)
        r.append(footprint())
    meminfo = subprocess.run([vmuc, "shell", name, "--", "cat /proc/meminfo"], check=True,
                             capture_output=True, text=True, stdin=subprocess.DEVNULL).stdout
    kib = {l.split(":")[0]: int(l.split()[1]) for l in meminfo.splitlines() if l.endswith("kB")}
    guest_used.append((kib["MemTotal"] - kib["MemFree"]) // 1024)
    rounds.append(r)
    subprocess.run([vmuc, "stop", name], check=True, capture_output=True)

mac = subprocess.run(["sw_vers", "-productVersion"], capture_output=True, text=True).stdout.strip()
chip = subprocess.run(["sysctl", "-n", "machdep.cpu.brand_string"], capture_output=True, text=True).stdout.strip()
med = [statistics.median(r[i] for r in rounds) for i in range(3)]
row = (f"| {datetime.date.today()} | {chip}, macOS {mac} | {name} | `{extra or '-'}` | {stop or '-'} | "
       f"{runs} | {med[0]:.0f} | {med[1]:.0f} | {med[2]:.0f} | {statistics.median(guest_used):.0f} |")
with open(f"{root}/docs/benchmarks/mem-idle.md", "a") as f:
    f.write(row + "\n")
print(row)
print("runs:", rounds)
PY
