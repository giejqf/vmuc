#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# Fills the formula's placeholders:
#   render.sh VERSION SOURCE_TARBALL KERNEL_IMG AGENT_BIN > vmuc.rb
# Checksums are computed from the files given, so the formula always
# matches the assets uploaded next to it.
set -euo pipefail

version=${1:?usage: render.sh VERSION SOURCE_TARBALL KERNEL_IMG AGENT_BIN}
sum() { shasum -a 256 "$1" | cut -d' ' -f1; }
src=$(sum "${2:?source tarball}")
kernel=$(sum "${3:?kernel image}")
agent=$(sum "${4:?agent binary}")
case $version in
    *[!0-9.]* | '') echo "render.sh: version must look like 0.1.0, got $version" >&2; exit 1 ;;
esac
sed -e "s/VERSION/$version/g" \
    -e "s/SHA256_SOURCE/$src/" -e "s/SHA256_KERNEL/$kernel/" -e "s/SHA256_AGENT/$agent/" \
    "$(dirname "${BASH_SOURCE[0]}")/vmuc.rb"
