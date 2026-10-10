#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# Cross-build the vmuc guest kernel (arm64, Linux LTS) on an x86 Linux box.
#
#   guest/kernel/build.sh            build (cached download, incremental)
#   CLEAN=1 guest/kernel/build.sh    wipe guest/kernel/build and guest/kernel/out first
#
# Inputs:  guest/kernel/VERSION (version + tarball sha256), guest/kernel/vmuc.config
# Outputs: guest/kernel/out/{Image,config,version.txt}
#
# Needs: aarch64-linux-gnu-gcc, make, flex, bison, bc, libssl-dev, libelf-dev,
#        pahole (dwarves, for BTF), curl, tar, xz.
set -euo pipefail

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
build_dir=$here/build
out_dir=$here/out
jobs=${JOBS:-$(nproc)}

mapfile -t version_lines < "$here/VERSION"
version=${version_lines[0]}
sha256=${version_lines[1]}
major=${version%%.*}
tarball=linux-$version.tar.xz
url=https://cdn.kernel.org/pub/linux/kernel/v${major}.x/$tarball
src=$build_dir/linux-$version
obj=$build_dir/obj

log() { printf '\033[1m[kernel]\033[0m %s\n' "$*" >&2; }

if [[ ${CLEAN:-0} == 1 ]]; then
    log "CLEAN=1: removing $build_dir and $out_dir"
    rm -rf "$build_dir" "$out_dir"
fi
mkdir -p "$build_dir" "$out_dir"

# 1. Tarball, verified by sha256.
if [[ -f $build_dir/$tarball ]] && echo "$sha256  $build_dir/$tarball" | sha256sum -c --status; then
    log "$tarball present, sha256 ok"
else
    log "downloading $url"
    curl -fsSL -o "$build_dir/$tarball.part" "$url"
    echo "$sha256  $build_dir/$tarball.part" | sha256sum -c --status \
        || { log "sha256 mismatch for $tarball"; rm -f "$build_dir/$tarball.part"; exit 1; }
    mv "$build_dir/$tarball.part" "$build_dir/$tarball"
fi

# 2. Extract once (stamp file marks a complete extraction).
if [[ ! -f $src/.vmuc-extracted ]]; then
    log "extracting $tarball"
    rm -rf "$src"
    tar -C "$build_dir" -xf "$build_dir/$tarball"
    touch "$src/.vmuc-extracted"
fi

# 3. Reproducibility: fixed user/host; the timestamp is the release's own
#    (mtime of the top-level Makefile as stored in the tarball), so two
#    machines building the same VERSION embed the same build string.
export ARCH=arm64 CROSS_COMPILE=aarch64-linux-gnu-
export KBUILD_BUILD_USER=vmuc KBUILD_BUILD_HOST=vmuc
export SOURCE_DATE_EPOCH=${SOURCE_DATE_EPOCH:-$(stat -c %Y "$src/Makefile")}
export KBUILD_BUILD_TIMESTAMP=${KBUILD_BUILD_TIMESTAMP:-$(date -u -d "@$SOURCE_DATE_EPOCH" '+%a %b %e %H:%M:%S UTC %Y')}
export KBUILD_BUILD_VERSION=1

# 4. Configure: arm64 defconfig + our fragment. Re-run only when the fragment
#    changed, so repeated invocations are incremental.
mkdir -p "$obj"
if [[ ! -f $obj/.config || ! -f $obj/.vmuc-fragment-stamp ]] \
   || ! cmp -s "$here/vmuc.config" "$obj/.vmuc-fragment-stamp"; then
    log "merging arch/arm64/configs/defconfig + vmuc.config"
    ( cd "$src" && scripts/kconfig/merge_config.sh -O "$obj" \
          arch/arm64/configs/defconfig "$here/vmuc.config" ) > "$obj/merge.log" 2>&1 \
        || { cat "$obj/merge.log"; exit 1; }
    cp "$here/vmuc.config" "$obj/.vmuc-fragment-stamp"
fi

# 5. Verify every line of the fragment took effect. merge_config.sh only
#    warns, and defconfig dependencies silently drop options, so this is the
#    real check: "CONFIG_X=v" must be present verbatim; "# CONFIG_X is not
#    set" must not be contradicted by "CONFIG_X=y".
log "verifying fragment against $obj/.config"
unsat=0
while IFS= read -r line; do
    case $line in
        CONFIG_*=*)
            sym=${line%%=*}
            if ! grep -qxF -- "$line" "$obj/.config"; then
                printf '  NOT APPLIED  %-50s actual: %s\n' "$line" \
                    "$(grep -E "^(# )?${sym}[= ]" "$obj/.config" || echo '<absent>')" >&2
                unsat=$((unsat + 1))
            fi ;;
        "# CONFIG_"*" is not set")
            sym=${line#\# }; sym=${sym%% *}
            if grep -qE "^$sym=" "$obj/.config"; then
                printf '  NOT APPLIED  %-50s actual: %s\n' "$line" \
                    "$(grep -E "^$sym=" "$obj/.config")" >&2
                unsat=$((unsat + 1))
            fi ;;
    esac
done < "$here/vmuc.config"
if (( unsat > 0 )); then
    log "$unsat fragment line(s) did not take effect; fix vmuc.config"
    exit 1
fi
log "fragment fully applied"

# 6. Build Image (uncompressed; the VMM loads it directly). JOBS=1 is read
#    by scripts/Makefile.btf and pins pahole to one thread: with -jN pahole
#    1.25 emits BTF in a scheduling-dependent order, so two builds of the
#    same tree differed only in .BTF (and the BTF type ids patched into
#    .BTF_ids). Compilation itself still runs with -j$jobs.
log "building linux-$version with -j$jobs"
make -C "$src" O="$obj" -j"$jobs" JOBS=1 Image

# 7. Publish.
cp "$obj/arch/arm64/boot/Image" "$out_dir/Image"
cp "$obj/.config" "$out_dir/config"
release=$(make -s -C "$src" O="$obj" kernelrelease)
{
    echo "version=$version"
    echo "release=$release"
    echo "tarball_sha256=$sha256"
    echo "image_sha256=$(sha256sum "$out_dir/Image" | cut -d' ' -f1)"
    echo "image_bytes=$(stat -c %s "$out_dir/Image")"
    echo "source_date_epoch=$SOURCE_DATE_EPOCH"
} > "$out_dir/version.txt"

log "done: $out_dir/Image ($(du -h "$out_dir/Image" | cut -f1), release $release)"
sha256sum "$out_dir/Image" "$out_dir/config"
cat "$out_dir/version.txt"

# 8. The boot-test initramfs is cheap and lives next to the Image, so
#    `make kernel-test` needs nothing else.
"$here/initramfs/build.sh"
