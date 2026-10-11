#!/bin/sh
# SPDX-License-Identifier: Apache-2.0
#
# vmuc guest boot script. Runs as root on every boot from the cidata ISO,
# started by /var/lib/cloud/scripts/per-boot/00-vmuc.boot.sh. Installs
# the agent, runs the built-in boot scripts and the provisioning scripts,
# then signals the host by writing the boot ID to /run/vmuc-boot-done.
# `boot.sh agent` (from cloud-init's bootcmd) only installs and starts
# the agent, early in boot.
set -eu

INFO() {
	echo "vmuc $(date +%Y-%m-%dT%H:%M:%S)| $*"
}

WARN() {
	echo "vmuc $(date +%Y-%m-%dT%H:%M:%S)| WARNING: $*" >&2
}

: "${VMUC_CIDATA_MNT:=/mnt/vmuc-cidata}"
export VMUC_CIDATA_MNT

rm -f /run/vmuc-boot-done

# shellcheck source=/dev/null
. "${VMUC_CIDATA_MNT}/vmuc.env"

CODE=0

# Installs /usr/local/bin/vmuc-agent from the ISO when it is missing or
# changed, then makes sure it is enabled and running under the service
# manager the guest has (systemd, or OpenRC on Alpine).
install_agent() {
	src="${VMUC_CIDATA_MNT}/vmuc-agent"
	dst=/usr/local/bin/vmuc-agent
	if [ ! -f "${src}" ]; then
		INFO "No agent binary on the seed; skipping agent install"
		return 0
	fi
	changed=0
	if [ ! -f "${dst}" ]; then
		changed=1
	else
		src_sum="$(sha256sum <"${src}")"
		dst_sum="$(sha256sum <"${dst}")"
		if [ "${src_sum}" != "${dst_sum}" ]; then
			changed=1
		fi
	fi
	if [ "${changed}" = 1 ]; then
		INFO "Installing ${dst}"
		mkdir -p "$(dirname "${dst}")"
		cp "${src}" "${dst}.tmp"
		chmod 0755 "${dst}.tmp"
		mv -f "${dst}.tmp" "${dst}"
	fi
	if command -v systemctl >/dev/null 2>&1; then
		unit=/etc/systemd/system/vmuc-agent.service
		if ! cmp -s "${VMUC_CIDATA_MNT}/vmuc-agent.service" "${unit}"; then
			cp "${VMUC_CIDATA_MNT}/vmuc-agent.service" "${unit}"
			changed=1
		fi
		if [ "${changed}" = 1 ]; then
			systemctl daemon-reload
		fi
		systemctl enable vmuc-agent.service >/dev/null 2>&1
		# --no-block: bootcmd runs inside cloud-init.service, and a blocking
		# start from there would wait on (or deadlock with) boot ordering.
		if [ "${changed}" = 1 ]; then
			systemctl restart --no-block vmuc-agent.service
		else
			systemctl start --no-block vmuc-agent.service
		fi
	elif command -v rc-update >/dev/null 2>&1; then
		cp "${VMUC_CIDATA_MNT}/vmuc-agent.openrc" /etc/init.d/vmuc-agent
		chmod 0755 /etc/init.d/vmuc-agent
		rc-update add vmuc-agent default >/dev/null 2>&1 || true
		if [ "${changed}" = 1 ]; then
			rc-service vmuc-agent restart
		else
			rc-service vmuc-agent start >/dev/null 2>&1 || rc-service vmuc-agent restart
		fi
	else
		WARN "No supported service manager; starting the agent directly"
		pkill -x vmuc-agent >/dev/null 2>&1 || true
		nohup "${dst}" >/var/log/vmuc-agent.log 2>&1 &
	fi
}

# Runs every *.sh in a directory, in name order, as root.
run_scripts() {
	dir="$1"
	[ -d "${dir}" ] || return 0
	for f in "${dir}"/*.sh; do
		[ -e "${f}" ] || continue
		INFO "Executing ${f}"
		if ! "${f}"; then
			WARN "Failed to execute ${f}"
			CODE=1
		fi
	done
}

# Runs provision.user/*.sh as the instance user. The ISO is only readable
# by root, so the scripts and the environment are copied to a scratch
# directory owned by the user first.
run_user_scripts() {
	dir="${VMUC_CIDATA_MNT}/provision.user"
	[ -d "${dir}" ] || return 0
	tmp="$(mktemp -d /tmp/vmuc-provision.XXXXXX)"
	cp "${VMUC_CIDATA_MNT}/vmuc.env" "${tmp}/vmuc.env"
	for f in "${dir}"/*.sh; do
		[ -e "${f}" ] || continue
		cp "${f}" "${tmp}/"
	done
	chmod 0755 "${tmp}"/*.sh
	chown -R "${VMUC_USER}" "${tmp}"
	runtime_dir="/run/user/${VMUC_UID}"
	for f in "${tmp}"/*.sh; do
		[ -e "${f}" ] || continue
		INFO "Executing ${f} as ${VMUC_USER}"
		if ! su -l "${VMUC_USER}" -s /bin/sh -c ". '${tmp}/vmuc.env'; export VMUC_CIDATA_MNT='${VMUC_CIDATA_MNT}'; [ -d '${runtime_dir}' ] && export XDG_RUNTIME_DIR='${runtime_dir}'; exec '${f}'"; then
			WARN "Failed to execute ${f} as ${VMUC_USER}"
			CODE=1
		fi
	done
	rm -rf "${tmp}"
}

install_agent
if [ "${1:-}" = agent ]; then
	exit 0
fi
run_scripts "${VMUC_CIDATA_MNT}/boot"
run_scripts "${VMUC_CIDATA_MNT}/provision.data"
run_scripts "${VMUC_CIDATA_MNT}/provision.system"
run_user_scripts

# The boot ID changes on every boot, so the host can tell this boot's
# marker from a stale one.
echo "${VMUC_BOOT_ID}" >/run/vmuc-boot-done
INFO "Boot done (exit code ${CODE})"
exit "${CODE}"
