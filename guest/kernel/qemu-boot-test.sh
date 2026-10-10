#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# Boot guest/kernel/out/Image under QEMU TCG (-M virt, device tree, virtio-mmio
# only) with the boot-test initramfs and wait for the init to print
# VMUC_BOOT_OK. QEMU is a test harness here, never a runtime dependency.
#
#   guest/kernel/qemu-boot-test.sh          exit 0 iff VMUC_BOOT_OK was printed
#   TIMEOUT=600 guest/kernel/qemu-boot-test.sh
#
# Console goes to guest/kernel/out/boot-test.log, the virtio console port to
# guest/kernel/out/boot-test-hvc0.log.
set -euo pipefail

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
out=$here/out
image=$out/Image
initrd=$out/boottest-initramfs.cpio.gz
log=$out/boot-test.log
hvclog=$out/boot-test-hvc0.log
disk=$here/build/boot-test-disk.raw
timeout=${TIMEOUT:-300}
qemu=${QEMU:-qemu-system-aarch64}

for f in "$image" "$initrd"; do
    [[ -f $f ]] || { echo "missing $f (run build.sh and initramfs/build.sh)" >&2; exit 2; }
done
command -v "$qemu" >/dev/null || { echo "$qemu not found" >&2; exit 2; }

mkdir -p "$(dirname "$disk")"
rm -f "$disk" "$log" "$hvclog"
truncate -s 64M "$disk"

# The init checks these virtio IDs are present and bound (vmuc.expect=).
cmdline="console=ttyAMA0 earlycon=pl011,0x9000000 panic=-1 vmuc.expect=net,blk,console,rng,balloon"

# shellcheck disable=SC2054  # commas are inside single -drive/-chardev words
qemu_args=(
    -M virt -cpu cortex-a72 -smp 2 -m 1024
    -accel tcg
    -nographic -no-reboot
    -global virtio-mmio.force-legacy=false
    -kernel "$image" -initrd "$initrd" -append "$cmdline"
    -drive if=none,id=d0,file="$disk",format=raw,cache=unsafe
    -device virtio-blk-device,drive=d0
    -netdev user,id=n0 -device virtio-net-device,netdev=n0
    -device virtio-rng-device
    -device virtio-balloon-device,free-page-reporting=on
    -device virtio-serial-device
    -chardev file,id=hvc0,path="$hvclog" -device virtconsole,chardev=hvc0
)

qemu_pid=
cleanup() {
    if [[ -n $qemu_pid ]] && kill -0 "$qemu_pid" 2>/dev/null; then
        kill "$qemu_pid" 2>/dev/null || true
        sleep 1
        kill -9 "$qemu_pid" 2>/dev/null || true
    fi
}
trap cleanup EXIT INT TERM

now_ms() { date +%s%3N; }

echo "[boot-test] $qemu ${qemu_args[*]}" >&2
start=$(now_ms)
"$qemu" "${qemu_args[@]}" </dev/null >"$log" 2>&1 &
qemu_pid=$!

status=timeout
ok_ms=
while :; do
    if grep -q '^VMUC_BOOT_OK' "$log" 2>/dev/null; then
        ok_ms=$(( $(now_ms) - start )); status=ok; break
    fi
    if grep -q '^VMUC_BOOT_FAIL' "$log" 2>/dev/null; then
        status=fail; break
    fi
    if grep -q 'Kernel panic - not syncing' "$log" 2>/dev/null; then
        status=panic; break
    fi
    if ! kill -0 "$qemu_pid" 2>/dev/null; then
        status=exited; break
    fi
    if (( $(now_ms) - start > timeout * 1000 )); then
        status=timeout; break
    fi
    sleep 0.2
done

# After a marker, give the guest a moment to power off cleanly; on a
# timeout kill QEMU right away. Either way it is gone before we read the log.
if [[ $status != timeout ]]; then
    for _ in $(seq 50); do
        kill -0 "$qemu_pid" 2>/dev/null || break
        sleep 0.2
    done
fi
cleanup
qemu_pid=

echo "[boot-test] ---- last 40 lines of $log ----" >&2
tail -n 40 "$log" >&2
echo "[boot-test] ---------------------------------" >&2

case $status in
    ok)
        hvc=missing
        grep -q '^VMUC_HVC_OK' "$hvclog" 2>/dev/null && hvc=ok
        echo "[boot-test] PASS: VMUC_BOOT_OK after ${ok_ms} ms (QEMU start -> marker); virtconsole: $hvc"
        exit 0 ;;
    fail)    echo "[boot-test] FAIL: $(grep -m1 '^VMUC_BOOT_FAIL' "$log")" >&2; exit 1 ;;
    panic)   echo "[boot-test] FAIL: kernel panic" >&2; exit 1 ;;
    exited)  echo "[boot-test] FAIL: QEMU exited without VMUC_BOOT_OK" >&2; exit 1 ;;
    *)       echo "[boot-test] FAIL: no VMUC_BOOT_OK within ${timeout}s" >&2; exit 1 ;;
esac
