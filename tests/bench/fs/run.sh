#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# fs: virtiofs throughput and metadata speed against the guest's own
# virtio-blk disk and the host itself, with the same workload
# (workload.py: 1 GiB sequential write and cold read, 5000 small files
# created, stat'ed cold, read cold and unlinked, open+close latency).
#
#   tests/bench/fs/run.sh
#   FS_DAX=1 tests/bench/fs/run.sh     the share with virtiofs DAX (one row)
#
# Builds a throwaway instance (temp VMUC_HOME, shared image cache) with a
# writable share of a host directory under target/, runs the workload on
# the share, on /var/tmp in the guest and on the host, then deletes it.
# Appends dated rows to docs/benchmarks/fs.md. Target: >= VZ virtiofs.
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)
(cd "$root" && cargo build -q --release -p vmuc-cli && \
    cargo build -q --release -p vmuc-agent --target aarch64-unknown-linux-musl)
vmuc=$root/target/release/vmuc
codesign --force --sign - --entitlements "$root/vmuc.entitlements" "$vmuc" 2>/dev/null

home=$(mktemp -d /tmp/vmuc-fs.XXXX)
share=$(mktemp -d "$root/target/bench-fs.XXXX")
cleanup() {
    VMUC_HOME=$home "$vmuc" stop --force fsb >/dev/null 2>&1 || true
    rm -rf "$home" "$share"
}
trap cleanup EXIT
export VMUC_HOME=$home
if [[ ${FS_DAX:-0} == 1 ]]; then
    "$vmuc" create ubuntu --name fsb --cpus 4 --memory 4GiB \
        --set "mounts=[{location: $share, writable: true, dax: true}]" >/dev/null
else
    "$vmuc" create ubuntu --name fsb --cpus 4 --memory 4GiB --mount "$share:w" >/dev/null
fi
"$vmuc" start fsb --timeout 300 >/dev/null
# First boot: wait for cloud-init (snap seeding) so it does not compete.
"$vmuc" shell fsb -- 'for i in $(seq 180); do test -e /run/vmuc-boot-done && break; sleep 1; done' \
    </dev/null

workload=$root/tests/bench/fs/workload.py
virtiofs=$("$vmuc" shell fsb -- sudo python3 - "$share/w" 1 <"$workload")
if [[ ${FS_DAX:-0} == 1 ]]; then
    blk='{}' host='{}'
else
    blk=$("$vmuc" shell fsb -- sudo python3 - /var/tmp/vmuc-fs 1 <"$workload")
    host=$(python3 -I "$workload" "$share/h")
fi

python3 -I - "$root" "${FS_DAX:-0}" "$virtiofs" "$blk" "$host" <<'PY'
import datetime, json, subprocess, sys
root, dax, *results = sys.argv[1:]
mac = subprocess.run(["sw_vers", "-productVersion"], capture_output=True, text=True).stdout.strip()
chip = subprocess.run(["sysctl", "-n", "machdep.cpu.brand_string"], capture_output=True, text=True).stdout.strip()
rev = subprocess.run(["git", "-C", root, "rev-parse", "--short", "HEAD"], capture_output=True, text=True).stdout.strip()
with open(f"{root}/docs/benchmarks/fs.md", "a") as f:
    labels = ["virtiofs DAX" if dax == "1" else "virtiofs", "guest virtio-blk ext4", "host APFS (cache warm)"]
    for what, r in zip(labels, results):
        r = json.loads(r.strip().splitlines()[-1])
        if not r:
            continue
        row = (f"| {datetime.date.today()} | {chip}, macOS {mac} | {rev} | {what} | {r['write_mbps']} | "
               f"{r['read_mbps']} | {r['create_ms']} | {r['stat_ms']} | {r['read_small_ms']} | {r['unlink_ms']} | "
               f"{r['open_close_us']} |")
        f.write(row + "\n")
        print(row)
PY
