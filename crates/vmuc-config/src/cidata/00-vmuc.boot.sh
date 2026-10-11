#!/bin/sh
# SPDX-License-Identifier: Apache-2.0
#
# Installed by cloud-init as /var/lib/cloud/scripts/per-boot/00-vmuc.boot.sh,
# and also run inline from bootcmd as `sh -c <this> - agent` so the agent
# starts seconds before the per-boot stage. Mounts the cidata ISO (label
# "cidata") and hands over to its boot.sh with the same arguments.
set -eu

VMUC_CIDATA_MNT=/mnt/vmuc-cidata
dev=/dev/disk/by-label/cidata
if [ ! -e "${dev}" ]; then
	# No udev symlinks (busybox mdev): ask blkid instead.
	dev="$(blkid -L cidata)"
fi
mkdir -p "${VMUC_CIDATA_MNT}"
chmod 0700 "${VMUC_CIDATA_MNT}"
if ! mountpoint -q "${VMUC_CIDATA_MNT}"; then
	mount -o ro,mode=0700,dmode=0700,overriderockperm,exec,uid=0 "${dev}" "${VMUC_CIDATA_MNT}"
fi
export VMUC_CIDATA_MNT
exec "${VMUC_CIDATA_MNT}/boot.sh" "$@"
