#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# balloon-release: does inflating the balloon over page cache give memory
# back to the host? The guest caches 2 GiB of a file read in order (the
# mem-pressure cache file), then the balloon target is set to 3.2 GiB
# through vmm.sock, forcing the guest to evict; footprint and released
# bytes before and 30 s after. The instance must exist and have run the
# mem-pressure cache workload once (for the file); it is started and
# stopped here.
#
#   tests/bench/mem-pressure/balloon-release.sh NAME
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)
name=${1:?usage: balloon-release.sh NAME}
(cd "$root" && cargo build -q --release -p vmuc-cli)
vmuc=$root/target/release/vmuc
codesign --force --sign - --entitlements "$root/vmuc.entitlements" "$vmuc" 2>/dev/null
"$vmuc" stop "$name" >/dev/null 2>&1 || true

python3 -I - "$vmuc" "$name" <<'PY'
import json, os, socket, subprocess, sys, time
vmuc, name = sys.argv[1:]
MIB = 1 << 20

def stat():
    d = json.loads(subprocess.run([vmuc, "stat", name, "--json"], check=True,
                                  capture_output=True, text=True).stdout)
    return d["host_footprint"] // MIB, d["released"] // MIB, d["balloon"] // MIB

subprocess.run([vmuc, "start", name], check=True, capture_output=True)
time.sleep(20)
subprocess.run([vmuc, "reclaim", name], check=True, capture_output=True)
time.sleep(8)
subprocess.run([vmuc, "shell", name, "--workdir", "/", "--",
                "dd if=/var/tmp/e1-cache.bin of=/dev/null bs=1M count=2048 status=none"],
               check=True, stdin=subprocess.DEVNULL)
time.sleep(5)
f1, r1, _ = stat()
s = socket.socket(socket.AF_UNIX)
s.connect(os.path.expanduser(f"~/.vmuc/{name}/vmm.sock"))
s.sendall((json.dumps({"method": "balloon_target", "params": {"bytes": 3200 * MIB}}) + "\n").encode())
s.recv(200)
time.sleep(30)
f2, r2, b2 = stat()
subprocess.run([vmuc, "stop", name], check=True, capture_output=True)
print(f"balloon {b2} MiB: footprint {f1} -> {f2} MiB ({f2 - f1:+}), released +{r2 - r1} MiB")
PY
