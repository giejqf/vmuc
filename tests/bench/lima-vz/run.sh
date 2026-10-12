#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# lima-vz: the reference vmuc's targets are set against, Lima on
# Virtualization.framework, measured on this Mac with vmuc's own probes'
# methods, so each benchmark page compares the two directly.
#
#   tests/bench/lima-vz/run.sh [RUNS]      (default 3 warm starts)
#
# Creates a throwaway Lima instance (Ubuntu 24.04, 4 CPUs, 4 GiB, vz,
# virtiofs, VZ NAT, containerd off as vmuc's template has none, plus a
# writable share under target/), then appends "Lima vz" rows to
# boot-ssh.md, idle-cpu.md, mem-reclaim.md, fs.md and net-iperf.md, and
# deletes the instance. Needs limactl and iperf3 on the host and no other
# Virtualization.framework VM running (the VZ process is found by name).
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)
runs=${1:-3}
name=vmucbench
here=$root/tests/bench/lima-vz
share=$(mktemp -d "$root/target/bench-lima.XXXX")
cleanup() {
    limactl delete -f "$name" >/dev/null 2>&1 || true
    rm -rf "$share"
}
trap cleanup EXIT

limactl delete -f "$name" >/dev/null 2>&1 || true
limactl create --tty=false --name="$name" --vm-type=vz --mount-type=virtiofs \
    --cpus=4 --memory=4 --network=vzNAT --containerd=none \
    --set ".mounts += [{\"location\": \"$share\", \"writable\": true}]" \
    template://ubuntu-24.04 >/dev/null 2>&1
limactl start --tty=false "$name" >/dev/null 2>&1   # first boot: not measured

python3 -I - "$root" "$here" "$name" "$share" "$runs" <<'PY'
import datetime, json, statistics, subprocess, sys, time

root, here, name, share, runs = sys.argv[1:]
MIB = 1 << 20
mac = subprocess.run(["sw_vers", "-productVersion"], capture_output=True, text=True).stdout.strip()
chip = subprocess.run(["sysctl", "-n", "machdep.cpu.brand_string"], capture_output=True, text=True).stdout.strip()
ver = subprocess.run(["limactl", "--version"], capture_output=True, text=True).stdout.split()[-1]
host = f"{chip}, macOS {mac}"
what = f"Lima {ver} vz"
today = datetime.date.today()


def run(*a, **kw):
    return subprocess.run(a, check=True, capture_output=True, text=True, **kw).stdout


def shell(cmd, stdin=None):
    return run("limactl", "shell", name, "--", "sh", "-c", cmd, input=stdin)


def vz_pid():
    pids = run("pgrep", "-f", "com.apple.Virtualization.VirtualMachine").split()
    if len(pids) != 1:
        sys.exit(f"expected one Virtualization.framework VM process, found {pids}")
    return pids[0]


def proc(*pids):
    return json.loads(run("python3", "-I", f"{here}/proc.py", *pids))


def append(page, row):
    with open(f"{root}/docs/benchmarks/{page}", "a") as f:
        f.write(row + "\n")
    print(f"{page}: {row}")


# Warm start to a shell.
starts, totals = [], []
for _ in range(int(runs)):
    run("limactl", "stop", name)
    t0 = time.perf_counter()
    run("limactl", "start", "--tty=false", name)
    t1 = time.perf_counter()
    shell("true")
    t2 = time.perf_counter()
    starts.append((t1 - t0) * 1000)
    totals.append((t2 - t0) * 1000)
append("boot-ssh.md", f"| {today} | {host} | {what} ubuntu-24.04 | Ubuntu stock | {runs} | "
       f"{statistics.median(starts):.0f} | {statistics.median(totals):.0f} | {max(totals):.0f} |")

# Idle: the VZ process plus Lima's host agent, 60 s settle, 30 s sample.
vz = vz_pid()
ha = open(f"{subprocess.os.path.expanduser('~')}/.lima/{name}/ha.pid").read().strip()
time.sleep(60)
a = proc(vz, ha)
time.sleep(30)
b = proc(vz, ha)
fp = proc(vz)["footprint"]
append("idle-cpu.md", f"| {today} | {host} | {what} (VZ + hostagent) | 4 | 60s | 30s | "
       f"{(b['cpu_s'] - a['cpu_s']) / 30 * 100:.2f} | {fp // MIB} MiB |")

# Memory: as tests/bench/mem-reclaim (2 GiB in tmpfs, freed; then the
# guest drops caches), reading the VZ process's footprint.
f0 = proc(vz)["footprint"]
shell("sudo mkdir -p /mnt/b && sudo mount -t tmpfs -o size=3G tmpfs /mnt/b"
      " && sudo dd if=/dev/zero of=/mnt/b/fill bs=1M count=2048 status=none")
time.sleep(1)
f1 = proc(vz)["footprint"]
shell("sudo rm /mnt/b/fill")
t0 = time.monotonic()
series = []
while time.monotonic() - t0 < 20:
    series.append((time.monotonic() - t0, proc(vz)["footprint"]))
    time.sleep(0.5)
at10 = [f for t, f in series if t <= 10.0][-1]
back10 = (f1 - at10) / (2048 * MIB) * 100
first90 = next((t for t, f in series if f1 - f >= 0.9 * 2048 * MIB), None)
shell("sync; echo 3 | sudo tee /proc/sys/vm/drop_caches >/dev/null")
time.sleep(10)
idle = proc(vz)["footprint"]
t90 = f"{first90:.1f}" if first90 is not None else ">20"
append("mem-reclaim.md", f"| {today} | {host} | {what} | `-` | {f0 // MIB} | {f1 // MIB} | "
       f"{back10:.0f} | {t90} | {idle // MIB} | - |")

# virtiofs and the guest's own disk, the same workload as tests/bench/fs.
workload = open(f"{root}/tests/bench/fs/workload.py").read()
for where, d in [("Lima vz virtiofs", f"{share}/w"), ("Lima vz guest disk", "/var/tmp/limafs")]:
    r = json.loads(shell(f"sudo python3 - {d} 1", stdin=workload).strip().splitlines()[-1])
    append("fs.md", f"| {today} | {host} | {what} | {where} | {r['write_mbps']} | {r['read_mbps']} | "
           f"{r['create_ms']} | {r['stat_ms']} | {r['read_small_ms']} | {r['unlink_ms']} | "
           f"{r['open_close_us']} |")

# Network: iperf3 from the host to the guest's VZ NAT address.
shell("sudo apt-get -qq update >/dev/null 2>&1; "
      "sudo DEBIAN_FRONTEND=noninteractive apt-get -qq install -y iperf3 >/dev/null 2>&1; iperf3 -s -D")
ip = shell("ip -4 -o addr show lima0").split()[3].split("/")[0]
rates = []
for args in ([], ["-R"], ["-P", "4"], ["-R", "-P", "4"]):
    time.sleep(1)  # iperf3 -s takes one client at a time
    out = json.loads(run("iperf3", "-c", ip, "-t", "8", "-J", *args))
    rates.append(out["end"]["sum_received"]["bits_per_second"] / 1e9)
append("net-iperf.md", f"| {today} | {host} | {what} | VZ NAT | " + " | ".join(f"{r:.1f}" for r in rates) + " |")
PY
