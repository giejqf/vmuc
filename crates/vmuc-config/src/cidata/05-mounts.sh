#!/bin/sh
# SPDX-License-Identifier: Apache-2.0
#
# Mounts every virtiofs share listed in vmuc.env. Idempotent: shares that
# are already mounted (by systemd from the fstab cloud-init wrote) are left
# alone unless their options differ from this boot's (fstab is rewritten
# only after systemd mounted from the last boot's), and `mount -a` at the
# end picks up anything fstab still lists.
set -eu

: "${VMUC_CIDATA_MNT:=/mnt/vmuc-cidata}"
# shellcheck source=/dev/null
. "${VMUC_CIDATA_MNT}/vmuc.env"

code=0
i=0
tag=
mount_point=
writable=
dax=
while [ "${i}" -lt "${VMUC_MOUNTS_COUNT:-0}" ]; do
	eval "tag=\${VMUC_MOUNT_${i}_TAG}"
	eval "mount_point=\${VMUC_MOUNT_${i}_MOUNTPOINT}"
	eval "writable=\${VMUC_MOUNT_${i}_WRITABLE}"
	eval "dax=\${VMUC_MOUNT_${i}_DAX:-0}"
	i=$((i + 1))
	mkdir -p "${mount_point}"
	opts=rw
	if [ "${writable}" != 1 ]; then
		opts=ro
	fi
	want_dax=
	if [ "${dax}" = 1 ]; then
		opts="${opts},dax=inode"
		want_dax=dax=inode
	fi
	if mountpoint -q "${mount_point}"; then
		have=$(awk -v m="${mount_point}" '$2 == m { print $4 }' /proc/mounts | tail -n 1)
		have_dax=$(printf '%s\n' "${have}" | tr ',' '\n' | grep '^dax=' || true)
		if [ "${have%%,*}" = "${opts%%,*}" ] && [ "${have_dax}" = "${want_dax}" ]; then
			continue
		fi
		umount "${mount_point}" || continue
	fi
	if ! mount -t virtiofs -o "${opts}" "${tag}" "${mount_point}"; then
		echo "vmuc: failed to mount ${tag} on ${mount_point}" >&2
		code=1
	fi
done

mount -a || true
exit "${code}"
