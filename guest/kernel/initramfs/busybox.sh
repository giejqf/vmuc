#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# Build a busybox initramfs for Hypervisor.framework bring-up and the
# boot-serial benchmark: guest/kernel/out/busybox-initramfs.cpio
# (uncompressed, so boot timing includes no decompression).
#
# Runs on macOS and Linux; needs curl, tar, cpio. busybox comes from
# Alpine's busybox-static package, pinned by sha256. /dev/console comes
# from the kernel's built-in default initramfs, so no root is needed.
#
# The init script reads `vmuc.test=` from the kernel command line:
#   (unset)    interactive shell on the console
#   poweroff   print VMUC_INIT_OK and power off
#   reboot     print VMUC_INIT_OK and reboot (every boot)
set -euo pipefail

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
kdir=$(dirname "$here")
out=$kdir/out/busybox-initramfs.cpio
work=$kdir/build/busybox

apk_url=https://dl-cdn.alpinelinux.org/alpine/v3.22/main/aarch64/busybox-static-1.37.0-r20.apk
apk_sha256=ee469aee2958feffd7f64dd96655704025ff60af614f77a1d7323dc237c34da2

sha256() { if command -v sha256sum >/dev/null; then sha256sum "$1"; else shasum -a 256 "$1"; fi | cut -d' ' -f1; }

mkdir -p "$work" "$(dirname "$out")"
apk=$work/busybox-static.apk
if [[ ! -f $apk || $(sha256 "$apk") != "$apk_sha256" ]]; then
    echo "[busybox] downloading $apk_url" >&2
    curl -fsSL -o "$apk.part" "$apk_url"
    [[ $(sha256 "$apk.part") == "$apk_sha256" ]] || { echo "sha256 mismatch" >&2; rm -f "$apk.part"; exit 1; }
    mv "$apk.part" "$apk"
fi

root=$work/root
rm -rf "$root"
mkdir -p "$root"/{bin,dev,proc,sys,tmp,root}
tar -xzf "$apk" -C "$work" bin/busybox.static 2>/dev/null
cp "$work/bin/busybox.static" "$root/bin/busybox"
chmod 0755 "$root/bin/busybox"

cat > "$root/init" <<'INIT'
#!/bin/busybox sh
/bin/busybox --install -s /bin
export PATH=/bin HOME=/root
mount -t proc proc /proc
mount -t sysfs sysfs /sys
mount -t devtmpfs devtmpfs /dev 2>/dev/null
test="$(sed -n 's/.*vmuc\.test=\([a-z]*\).*/\1/p' /proc/cmdline)"
echo "VMUC_INIT_OK uptime=$(cut -d' ' -f1 /proc/uptime) cpus=$(grep -c ^processor /proc/cpuinfo) kernel=$(uname -r) time=$(date +%s)"
case "$test" in
    poweroff) poweroff -f ;;
    reboot) reboot -f ;;
esac
echo "busybox shell; 'poweroff -f' powers off, Ctrl-A x quits vmuc"
# A session leader opening the console tty gets it as controlling
# terminal, so job control and Ctrl-C work (this busybox has no cttyhack).
con=$(sed 's/.* //' /sys/class/tty/console/active)
exec setsid sh -c "exec sh <>/dev/${con:-console} >&0 2>&1"
INIT
chmod 0755 "$root/init"

( cd "$root" && find . | LC_ALL=C sort | cpio -o -H newc -R 0:0 2>/dev/null ) > "$out.part"
mv "$out.part" "$out"
echo "[busybox] wrote $out ($(wc -c < "$out" | tr -d ' ') bytes)" >&2
