#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# fs-memory: what reading files from a share costs the host, with and
# without virtiofs DAX. A throwaway instance (temp VMUC_HOME, shared image
# cache) gets a writable share holding 8 files of 256 MiB and 2000 of
# 64 KiB; after a settle and a reclaim the guest reads the large files,
# then the small ones, then reclaims again. Footprint is the VMM's
# phys_footprint; guest cache is MemInfo Cached. Appends one row to
# docs/benchmarks/fs-memory.md.
#
#   tests/bench/fs-memory/run.sh          FS_DAX=1 for the DAX share
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)
(cd "$root" && cargo build -q --release -p vmuc-cli && \
    cargo build -q --release -p vmuc-agent --target aarch64-unknown-linux-musl)
vmuc=$root/target/release/vmuc
codesign --force --sign - --entitlements "$root/vmuc.entitlements" "$vmuc" 2>/dev/null

home=$(mktemp -d /tmp/vmuc-fsm.XXXX)
share=$(mktemp -d "$root/target/bench-fsm.XXXX")
cleanup() {
    VMUC_HOME=$home "$vmuc" stop --force fsm >/dev/null 2>&1 || true
    rm -rf "$home" "$share"
}
trap cleanup EXIT
export VMUC_HOME=$home
dax=${FS_DAX:-0}
mkdir -p "$share/big" "$share/small"
for i in $(seq 8); do head -c $((256 << 20)) /dev/urandom >"$share/big/$i"; done
head -c $((2000 * 65536)) /dev/urandom | split -a 4 -b 65536 - "$share/small/f"
"$vmuc" create ubuntu --name fsm --cpus 4 --memory 4GiB \
    --set "mounts=[{location: $share, writable: true, dax: $([[ $dax == 1 ]] && echo true || echo false)}]" >/dev/null
"$vmuc" start fsm --timeout 300 >/dev/null
"$vmuc" shell fsm -- 'for i in $(seq 180); do test -e /run/vmuc-boot-done && break; sleep 1; done' \
    </dev/null

python3 -I - "$vmuc" "$share" "$root" "$dax" <<'PY'
import datetime, json, subprocess, sys, time
vmuc, share, root, dax = sys.argv[1:]
MIB = 1 << 20

def fp():
    s = json.loads(subprocess.run([vmuc, "stat", "fsm", "--json"], check=True,
                                  capture_output=True, text=True).stdout)
    return s["host_footprint"] // MIB

def shell(cmd):
    return subprocess.run([vmuc, "shell", "fsm", "--workdir", "/", "--", cmd], check=True,
                          capture_output=True, text=True, stdin=subprocess.DEVNULL).stdout

def cached():
    return int(shell("awk '/^Cached/ {print $2}' /proc/meminfo")) // 1024

time.sleep(30)
subprocess.run([vmuc, "reclaim", "fsm"], check=True, capture_output=True)
time.sleep(15)
f0, c0 = fp(), cached()
t = time.time()
shell(f"cat {share}/big/* >/dev/null")
big_s = time.time() - t
time.sleep(5)
f1, c1 = fp(), cached()
t = time.time()
shell(f"cat {share}/small/* >/dev/null")
small_s = time.time() - t
time.sleep(5)
f2 = fp()
subprocess.run([vmuc, "reclaim", "fsm"], check=True, capture_output=True)
time.sleep(15)
f3 = fp()
mac = subprocess.run(["sw_vers", "-productVersion"], capture_output=True, text=True).stdout.strip()
chip = subprocess.run(["sysctl", "-n", "machdep.cpu.brand_string"], capture_output=True, text=True).stdout.strip()
rev = subprocess.run(["git", "-C", root, "rev-parse", "--short", "HEAD"], capture_output=True, text=True).stdout.strip()
row = (f"| {datetime.date.today()} | {chip}, macOS {mac} | {rev} | {'DAX' if dax == '1' else 'no DAX'} | "
       f"{f0} | {f1} (+{f1 - f0}) | {c1 - c0} | {big_s:.1f} | {f2} | {small_s:.1f} | {f3} |")
with open(f"{root}/docs/benchmarks/fs-memory.md", "a") as f:
    f.write(row + "\n")
print(row)
PY
