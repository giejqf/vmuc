#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# idle-cpu: host CPU used by an idle guest, and the VMM's phys_footprint.
#
#   tests/bench/idle-cpu/run.sh busybox [CPUS]          busybox initramfs
#   tests/bench/idle-cpu/run.sh instance NAME [CPUS]    an existing instance's
#                                                       disk + seed via _boot
#
# Boots with `vmuc _boot`, waits SETTLE seconds (default 60 for an
# instance, 5 for busybox), samples the process's CPU time over SAMPLE
# seconds (default 30), records footprint, stops the VM, and appends a
# dated row to docs/benchmarks/idle-cpu.md. Target: < 0.5 % of one core
# with 4 vCPUs.
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)
mode=${1:?usage: run.sh busybox|instance NAME [CPUS]}
kernel=$root/guest/kernel/out/Image
(cd "$root" && cargo build -q --release -p vmuc-cli)
vmuc=$root/target/release/vmuc
codesign --force --sign - --entitlements "$root/vmuc.entitlements" "$vmuc" 2>/dev/null

case $mode in
    busybox)
        cpus=${2:-4}; settle=${SETTLE:-5}; what=busybox
        initrd=$root/guest/kernel/out/busybox-initramfs.cpio
        [[ -f $initrd ]] || "$root/guest/kernel/initramfs/busybox.sh"
        args=(--initrd "$initrd" --cmdline "console=ttyAMA0 panic=-1 quiet ${VMUC_BENCH_CMDLINE:-}")
        ;;
    instance)
        name=${2:?instance name}; cpus=${3:-4}; settle=${SETTLE:-60}; what="instance $name"
        dir=$HOME/.vmuc/$name
        "$vmuc" _seed "$name" --agent "${VMUC_AGENT:-$root/target/aarch64-unknown-linux-musl/release/vmuc-agent}" --no-mounts >/dev/null
        cmdline=$("$vmuc" _bootinfo "$name" | python3 -I -c 'import json,sys; print(json.load(sys.stdin)["cmdline"])')
        args=(--disk "$dir/disk.img" --disk "$dir/cidata.iso:ro" --hvc --memory 4GiB
              --cmdline "$cmdline ${VMUC_BENCH_CMDLINE:-}")
        ;;
    *) echo "unknown mode $mode" >&2; exit 2 ;;
esac
sample=${SAMPLE:-30}

log=$(mktemp)
( sleep $((settle + sample + 30)) | "$vmuc" _boot --kernel "$kernel" --cpus "$cpus" "${args[@]}" >"$log" 2>&1 ) &
sleep 1
pid=$(pgrep -n -x vmuc)
sleep "$settle"
cpu() { ps -o cputime= -p "$pid" | awk -F'[:.]' '{ printf "%.2f", ($1*60 + $2) + $3/100 }'; }
t0=$(cpu); sleep "$sample"; t1=$(cpu)
fp=$(footprint "$pid" 2>/dev/null | awk '/phys_footprint:/ { print $2, $3; exit }')
kill "$pid" 2>/dev/null || true
{ wait; } 2>/dev/null || true
rm -f "$log"

pct=$(python3 -I -c "print(f'{($t1 - $t0) / $sample * 100:.2f}')")
row="| $(date +%F) | $(sysctl -n machdep.cpu.brand_string), macOS $(sw_vers -productVersion) | $what | $cpus | ${settle}s | ${sample}s | $pct | $fp |"
echo "$row" >> "$root/docs/benchmarks/idle-cpu.md"
echo "$row"
