#!/usr/bin/env bash
# M1 integration harness: boot a real cloud image with the vmuc kernel,
# cloud-init seed and guest agent under QEMU TCG on a Linux box, then
# reach the guest through the agent (hello, boot-done, ssh via the
# tunnel) and confirm the virtio-balloon reporting feature is negotiated.
#
# This proves the guest side of vmuc (kernel config, seed, agent, ssh
# path, free page reporting). It says nothing about Hypervisor.framework.
#
# Usage: tests/qemu/run.sh [TEMPLATE] [--keep]
#   KERNEL=path/to/Image     (default guest/kernel/out/Image)
#   AGENT=path/to/vmuc-agent (default the musl release build)
#   TIMEOUT=seconds          (default 1500; TCG boots are slow)
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/../.." && pwd)
TEMPLATE=${1:-template:ubuntu}
KEEP=${2:-}
KERNEL=${KERNEL:-$ROOT/guest/kernel/out/Image}
AGENT=${AGENT:-$ROOT/target/aarch64-unknown-linux-musl/release/vmuc-agent}
TIMEOUT=${TIMEOUT:-1500}
VMUC=${VMUC:-$ROOT/target/debug/vmuc}
AGENT_PORT=17700
NAME=m1

for f in "$KERNEL" "$AGENT" "$VMUC"; do
  [ -e "$f" ] || { echo "missing $f" >&2; exit 2; }
done
command -v qemu-system-aarch64 >/dev/null || { echo "qemu-system-aarch64 missing" >&2; exit 2; }

WORK=$(mktemp -d /tmp/vmuc-qemu.XXXXXX)
export VMUC_HOME="$WORK/home"
QEMU_PID=
cleanup() {
  [ -n "$QEMU_PID" ] && kill "$QEMU_PID" 2>/dev/null || true
  if [ "$KEEP" = "--keep" ]; then
    echo "kept $WORK"
  else
    rm -rf "$WORK"
  fi
}
trap cleanup EXIT

say() { printf '\n==> %s\n' "$*"; }

say "create instance from $TEMPLATE"
"$VMUC" create "$TEMPLATE" --name $NAME --cpus 2 --memory 2GiB --disk 8GiB
BOOT=$("$VMUC" _bootinfo $NAME)
echo "$BOOT"
SEED=$("$VMUC" _seed $NAME --agent "$AGENT" --no-mounts)
BOOT_ID=$(echo "$SEED" | python3 -c 'import json,sys; print(json.load(sys.stdin)["bootId"])')
CMDLINE=$(echo "$BOOT" | python3 -c 'import json,sys; print(json.load(sys.stdin)["cmdline"])')
MAC=$(echo "$BOOT" | python3 -c 'import json,sys; print(json.load(sys.stdin)["mac_address"])')
DISK="$VMUC_HOME/$NAME/disk.img"
ISO="$VMUC_HOME/$NAME/cidata.iso"
CONSOLE="$WORK/console.log"

say "boot under QEMU TCG (console: $CONSOLE)"
START=$(date +%s)
qemu-system-aarch64 \
  -M virt -cpu cortex-a72 -smp 2 -m 2048 -nographic -no-reboot \
  -kernel "$KERNEL" \
  -append "$CMDLINE console=ttyAMA0 vmuc.agent.listen=tcp:0.0.0.0:7700" \
  -global virtio-mmio.force-legacy=false \
  -drive "file=$DISK,format=raw,if=none,id=d0,cache=unsafe" \
  -device virtio-blk-device,drive=d0 \
  -drive "file=$ISO,format=raw,if=none,id=d1,readonly=on" \
  -device virtio-blk-device,drive=d1 \
  -netdev "user,id=n0,hostfwd=tcp:127.0.0.1:$AGENT_PORT-:7700" \
  -device "virtio-net-device,netdev=n0,mac=$MAC" \
  -device virtio-rng-device \
  -device virtio-balloon-device,free-page-reporting=on \
  -serial "file:$CONSOLE" -monitor none \
  >"$WORK/qemu.out" 2>&1 &
QEMU_PID=$!

VIA="tcp:127.0.0.1:$AGENT_PORT"
say "wait for the agent (timeout ${TIMEOUT}s)"
until "$VMUC" _agent "$VIA" hello >/dev/null 2>&1; do
  if ! kill -0 "$QEMU_PID" 2>/dev/null; then
    echo "QEMU exited early:"; tail -40 "$CONSOLE"; exit 1
  fi
  if [ $(( $(date +%s) - START )) -gt "$TIMEOUT" ]; then
    echo "timed out waiting for the agent; console tail:"; tail -60 "$CONSOLE"; exit 1
  fi
  sleep 5
done
echo "agent answered after $(( $(date +%s) - START ))s"
"$VMUC" _agent "$VIA" hello

say "wait for boot.sh to finish (boot id $BOOT_ID)"
until "$VMUC" _agent "$VIA" exec '["cat","/run/vmuc-boot-done"]' 2>/dev/null | grep -q "$BOOT_ID"; do
  if [ $(( $(date +%s) - START )) -gt "$TIMEOUT" ]; then
    echo "timed out waiting for boot-done; console tail:"; tail -60 "$CONSOLE"; exit 1
  fi
  sleep 5
done
echo "boot done after $(( $(date +%s) - START ))s"

say "memory telemetry"
"$VMUC" _agent "$VIA" mem_info
"$VMUC" _agent "$VIA" psi || true

say "virtio-balloon free page reporting negotiated?"
FEATURES=$("$VMUC" _agent "$VIA" exec '["sh","-c","cat /sys/bus/virtio/drivers/virtio_balloon/virtio*/features"]' \
  | python3 -c 'import json,sys; print(json.load(sys.stdin)["data"]["stdout"].strip())')
echo "features: $FEATURES"
# Feature bits are printed as a string of 0/1 indexed by bit number.
[ "${FEATURES:5:1}" = "1" ] || { echo "VIRTIO_BALLOON_F_REPORTING (bit 5) not negotiated"; exit 1; }
echo "reporting: ok"

say "ssh through the agent tunnel"
GUEST_USER=$(echo "$BOOT" | python3 -c 'import json,sys; print(json.load(sys.stdin)["user"])')
SSH_OUT=$(ssh -F /dev/null \
  -o ProxyCommand="$VMUC _proxy $NAME 22 --via $VIA" \
  -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR \
  -o BatchMode=yes -o ConnectTimeout=30 \
  -i "$VMUC_HOME/_config/user" "$GUEST_USER@$NAME" -- 'uname -a; id; cat /proc/sys/vm/page_reporting 2>/dev/null || true')
echo "$SSH_OUT"
echo "$SSH_OUT" | grep -q "vmuc" || { echo "ssh did not reach the vmuc kernel"; exit 1; }

say "power off"
"$VMUC" _agent "$VIA" exec '["sh","-c","sleep 1; poweroff"]' >/dev/null 2>&1 || true
for _ in $(seq 1 60); do kill -0 "$QEMU_PID" 2>/dev/null || break; sleep 2; done
kill -0 "$QEMU_PID" 2>/dev/null && { echo "guest did not power off; killing"; kill "$QEMU_PID"; }
QEMU_PID=
echo
echo "M1 QEMU HARNESS: PASS (total $(( $(date +%s) - START ))s)"
