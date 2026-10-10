#!/bin/sh
# SPDX-License-Identifier: Apache-2.0
#
# Mounts every virtiofs share listed in vmuc.env. Idempotent: shares that
# are already mounted (by cloud-init from the fstab entries it wrote) are
# left alone; `mount -a` at the end picks up anything fstab still lists.
set -eu

: "${VMUC_CIDATA_MNT:=/mnt/vmuc-cidata}"
# shellcheck source=/dev/null
. "${VMUC_CIDATA_MNT}/vmuc.env"

code=0
i=0
tag=
mount_point=
writable=
while [ "${i}" -lt "${VMUC_MOUNTS_COUNT:-0}" ]; do
	eval "tag=\${VMUC_MOUNT_${i}_TAG}"
	eval "mount_point=\${VMUC_MOUNT_${i}_MOUNTPOINT}"
	eval "writable=\${VMUC_MOUNT_${i}_WRITABLE}"
	i=$((i + 1))
	mkdir -p "${mount_point}"
	if mountpoint -q "${mount_point}"; then
		continue
	fi
	opts=rw
	if [ "${writable}" != 1 ]; then
		opts=ro
	fi
	if ! mount -t virtiofs -o "${opts}" "${tag}" "${mount_point}"; then
		echo "vmuc: failed to mount ${tag} on ${mount_point}" >&2
		code=1
	fi
done

mount -a || true
exit "${code}"
