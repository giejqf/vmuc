#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# mem-pressure: the host-pressure policy against a real guest workload.
#
#   tests/bench/mem-pressure/run.sh NAME WORKLOAD LEVEL [SECONDS]
#
# WORKLOAD: idle | cache (re-read a 3 GiB file) | anon (2.5 GiB bursts) |
# build (vmuc's kernel, -j4; the row's duration is the build's).
# LEVEL: normal | warn | critical, held with `vmuc _pressure` for the
# whole run, or pulse (critical and normal alternating every 180 s).
# SECONDS defaults to 600. Whether the balloon may inflate is the
# instance's memoryPolicy.pressureBalloon, named in the row with the
# commit (+ when crates/ has uncommitted changes). Appends a dated row to
# docs/benchmarks/mem-pressure.md; per-second samples go to
# target/bench/mem-pressure/.
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)
name=${1:?usage: run.sh NAME WORKLOAD LEVEL [SECONDS]}
workload=${2:?workload: idle|cache|anon|build}
level=${3:?level: normal|warn|critical|pulse}
seconds=${4:-600}
(cd "$root" && cargo build -q --release -p vmuc-cli)
vmuc=$root/target/release/vmuc
codesign --force --sign - --entitlements "$root/vmuc.entitlements" "$vmuc" 2>/dev/null
python3 -I "$root/tests/bench/mem-pressure/probe.py" "$vmuc" "$name" "$workload" "$level" "$seconds" "$root"
