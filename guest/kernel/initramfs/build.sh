#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# Build the boot-test initramfs: a single static init (init.c) packed as
# guest/kernel/out/boottest-initramfs.cpio.gz. No root needed: the archive
# is written by gen_init_cpio from the kernel tree (built from source here),
# which creates the /dev/console node from a spec instead of the filesystem.
set -euo pipefail

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
kdir=$(dirname "$here")
out_dir=$kdir/out
work=$kdir/build/initramfs
version=$(head -n1 "$kdir/VERSION")
src=$kdir/build/linux-$version

mkdir -p "$out_dir" "$work"

[[ -f $src/usr/gen_init_cpio.c ]] \
    || { echo "kernel source $src missing; run guest/kernel/build.sh first" >&2; exit 1; }

echo "[initramfs] compiling init (aarch64 static)" >&2
aarch64-linux-gnu-gcc -static -Os -Wall -Wextra -o "$work/init" "$here/init.c"
aarch64-linux-gnu-strip "$work/init"

if [[ ! -x $work/gen_init_cpio || $src/usr/gen_init_cpio.c -nt $work/gen_init_cpio ]]; then
    cc -O2 -o "$work/gen_init_cpio" "$src/usr/gen_init_cpio.c"
fi

cat > "$work/spec" <<SPEC
dir /dev 0755 0 0
nod /dev/console 0600 0 0 c 5 1
dir /proc 0755 0 0
dir /sys 0755 0 0
file /init $work/init 0755 0 0
SPEC

export SOURCE_DATE_EPOCH=${SOURCE_DATE_EPOCH:-$(stat -c %Y "$src/Makefile")}
"$work/gen_init_cpio" -t "$SOURCE_DATE_EPOCH" "$work/spec" | gzip -9 -n > "$out_dir/boottest-initramfs.cpio.gz"

echo "[initramfs] $(stat -c %s "$out_dir/boottest-initramfs.cpio.gz") bytes -> $out_dir/boottest-initramfs.cpio.gz" >&2
sha256sum "$out_dir/boottest-initramfs.cpio.gz"
