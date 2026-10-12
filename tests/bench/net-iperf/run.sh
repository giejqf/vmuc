#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# net-iperf: TCP throughput between the host and a guest on vmnet (shared
# mode, through the root helper): one flow and four, both directions.
#
#   tests/bench/net-iperf/run.sh
#
# Builds a throwaway instance (temp VMUC_HOME, shared image cache),
# installs iperf3 in it, runs iperf3 from the host against the guest's
# address, deletes the instance and appends a row to
# docs/benchmarks/net-iperf.md. Needs the vmnet helper installed
# (`vmuc sudoers`) and iperf3 on the host. Target: > 10 Gbit/s.
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)
(cd "$root" && cargo build -q --release -p vmuc-cli && \
    cargo build -q --release -p vmuc-agent --target aarch64-unknown-linux-musl)
vmuc=$root/target/release/vmuc
codesign --force --sign - --entitlements "$root/vmuc.entitlements" "$vmuc" 2>/dev/null

home=$(mktemp -d /tmp/vmuc-iperf.XXXX)
cleanup() {
    VMUC_HOME=$home "$vmuc" stop --force netb >/dev/null 2>&1 || true
    rm -rf "$home"
}
trap cleanup EXIT
export VMUC_HOME=$home
"$vmuc" create ubuntu --name netb --cpus 4 --memory 4GiB >/dev/null
"$vmuc" start netb --timeout 300 >/dev/null
"$vmuc" shell netb -- 'for i in $(seq 180); do test -e /run/vmuc-boot-done && break; sleep 1; done
    sudo apt-get -qq update >/dev/null 2>&1
    sudo DEBIAN_FRONTEND=noninteractive apt-get -qq install -y iperf3 >/dev/null 2>&1
    iperf3 -s -D' </dev/null
ip=$("$vmuc" shell netb -- 'ip -4 -o addr show eth0' </dev/null | awk '{print $4}' | cut -d/ -f1)
tso=$("$vmuc" shell netb -- 'ethtool -k eth0' </dev/null | awk '/^tcp-segmentation-offload:/ {print $2}')

python3 -I - "$root" "$ip" "$tso" "$(git -C "$root" rev-parse --short HEAD)" <<'PY'
import datetime, json, subprocess, sys, time
root, ip, tso, rev = sys.argv[1:]
rates = []
for args in ([], ["-R"], ["-P", "4"], ["-R", "-P", "4"]):
    time.sleep(1)  # iperf3 -s takes one client at a time
    out = subprocess.run(["iperf3", "-c", ip, "-t", "8", "-J", *args],
                         check=True, capture_output=True, text=True).stdout
    rates.append(json.loads(out)["end"]["sum_received"]["bits_per_second"] / 1e9)
mac = subprocess.run(["sw_vers", "-productVersion"], capture_output=True, text=True).stdout.strip()
chip = subprocess.run(["sysctl", "-n", "machdep.cpu.brand_string"], capture_output=True, text=True).stdout.strip()
net = "vmnet shared, TSO" if tso == "on" else "vmnet shared, no offload"
row = (f"| {datetime.date.today()} | {chip}, macOS {mac} | vmuc {rev} | {net} | "
       + " | ".join(f"{r:.1f}" for r in rates) + " |")
with open(f"{root}/docs/benchmarks/net-iperf.md", "a") as f:
    f.write(row + "\n")
print(row)
PY
