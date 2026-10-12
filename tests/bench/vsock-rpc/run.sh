#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# vsock-rpc: round trips of a guest-agent call (`mem_info`) the way vmuc
# makes them: control-socket connect, vsock connection to the agent,
# request, reply.
#
#   tests/bench/vsock-rpc/run.sh NAME [CALLS]      (default 2000)
#
# Starts the instance if needed (and stops it again after), lets it
# settle, then appends a dated row to docs/benchmarks/vsock-rpc.md.
# Target: p50 < 0.3 ms.
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)
name=${1:?usage: run.sh NAME [CALLS]}
calls=${2:-2000}
(cd "$root" && cargo build -q --release -p vmuc-cli && \
    cargo build -q --release -p vmuc-agent --target aarch64-unknown-linux-musl)
vmuc=$root/target/release/vmuc
codesign --force --sign - --entitlements "$root/vmuc.entitlements" "$vmuc" 2>/dev/null

started=
if ! "$vmuc" list | awk -v n="$name" '$1 == n && $2 == "Running" { found = 1 } END { exit !found }'; then
    "$vmuc" start "$name" >/dev/null
    started=1
    sleep 10
fi
result=$("$vmuc" _agent "vsock:$name" mem_info --repeat "$calls")
[ -n "$started" ] && "$vmuc" stop "$name" >/dev/null

python3 -I - "$root" "$name" "$result" <<'PY'
import datetime, json, subprocess, sys
root, name, result = sys.argv[1:]
r = json.loads(result)
mac = subprocess.run(["sw_vers", "-productVersion"], capture_output=True, text=True).stdout.strip()
chip = subprocess.run(["sysctl", "-n", "machdep.cpu.brand_string"], capture_output=True, text=True).stdout.strip()
row = (f"| {datetime.date.today()} | {chip}, macOS {mac} | {name} | mem_info | {r['n']} | "
       f"{r['p50_us']} | {r['p90_us']} | {r['p99_us']} | {r['max_us']} |")
with open(f"{root}/docs/benchmarks/vsock-rpc.md", "a") as f:
    f.write(row + "\n")
print(row)
PY
