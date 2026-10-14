# SPDX-License-Identifier: Apache-2.0
"""net-cost: what guest-to-host traffic costs the host, by rate. An Ubuntu
instance (4 vCPUs, 4 GiB, a fresh VMUC_HOME) on vmnet's shared network
sends to a sink on the host through its gateway for 10 s, as fast as it
can and paced by the guest kernel (SO_MAX_PACING_RATE) at 10, 1 and
0.1 Gbit/s, then sits idle for 10 s; for each, the rate the sink saw and
the CPU of the VMM and the vmnet helper (`ps` cputime) and of the guest's
sender. Needs the vmnet helper (`vmuc sudoers`). `VMUC=path` measures
another build of vmuc; `--lima NAME` measures a running Lima instance on
VZ NAT instead (the Virtualization.framework process in the VMM column,
no helper). Appends a row to docs/benchmarks/net-cost.md.

    python3 -I tests/bench/net-cost/run.py [--lima NAME]
"""

import datetime
import os
import platform
import socket
import subprocess
import sys
import tempfile
import threading
import time

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(os.path.dirname(os.path.dirname(HERE)))
NAME = "cost"
RATES = [("max", 0), ("10G", 10_000_000_000), ("1G", 1_000_000_000), ("100M", 100_000_000)]
SENDER = r'''
import resource, socket, sys, time
rate, host, port, secs = int(sys.argv[1]), sys.argv[2], int(sys.argv[3]), float(sys.argv[4])
s = socket.socket()
if rate:
    s.setsockopt(socket.SOL_SOCKET, 47, rate // 8)  # SO_MAX_PACING_RATE, bytes/s
s.connect((host, port))
buf = b"x" * (1 << 20)
end = time.monotonic() + secs
while time.monotonic() < end:
    s.sendall(buf)
s.close()
r = resource.getrusage(resource.RUSAGE_SELF)
print(r.ru_utime + r.ru_stime)
'''


def cputime(pid):
    t = subprocess.run(["ps", "-o", "cputime=", "-p", str(pid)], capture_output=True,
                       text=True).stdout.strip()
    parts = t.split(":")
    return float(parts[-1]) + 60 * int(parts[-2]) + (3600 * int(parts[-3]) if len(parts) > 2 else 0)


def sink(srv, got):
    conn, _ = srv.accept()
    n = 0
    while True:
        b = conn.recv(1 << 20)
        if not b:
            break
        n += len(b)
    got.append(n)


def measure(shell, gateway, vmm, helper):
    """The row's cells, each rate in turn; `helper` may be None."""
    shell(f"cat > /tmp/send.py <<'EOF'\n{SENDER}\nEOF")
    time.sleep(5)
    cells = []
    for label, rate in RATES + [("idle", None)]:
        procs = [vmm] + ([helper] if helper else [])
        c0, t0 = [cputime(p) for p in procs], time.monotonic()
        if rate is None:
            time.sleep(10)
            gbps, guest = 0.0, 0.0
        else:
            srv = socket.socket()
            srv.bind(("0.0.0.0", 0))
            srv.listen(1)
            got = []
            th = threading.Thread(target=sink, args=(srv, got))
            th.start()
            out = shell(f"python3 /tmp/send.py {rate} {gateway} {srv.getsockname()[1]} 10")
            th.join()
            srv.close()
            gbps, guest = got[0] * 8 / 10 / 1e9, float(out.split()[-1]) * 10
        dt = time.monotonic() - t0
        v, h = ([100 * (cputime(p) - c) / dt for p, c in zip(procs, c0)] + [0.0])[:2]
        print(f"{label:<5} {gbps:7.2f} Gbit/s  VMM {v:4.0f}%  helper {h:4.0f}%  guest sender {guest:4.0f}%")
        cells.append(f"{v:.0f} / {h:.0f}" if rate is None else
                     f"{gbps:.1f}: {v:.0f} / {h:.0f} / {guest:.0f}" if rate == 0 else
                     f"{v:.0f} / {h:.0f} / {guest:.0f}")
        time.sleep(2)
    return cells


def lima(name):
    """A running Lima instance on VZ NAT: the VZ process is the VMM."""
    def shell(cmd):
        return subprocess.run(["limactl", "shell", name, "--", "sh", "-c", cmd],
                              capture_output=True, text=True, stdin=subprocess.DEVNULL,
                              check=True).stdout
    vz = int(subprocess.run(["pgrep", "-f", "Virtualization.VirtualMachine"],
                            capture_output=True, text=True).stdout.split()[0])
    # The route the guest uses (Lima adds a user-mode NIC beside VZ NAT).
    gateway = shell("ip -4 route get 1.1.1.1").split()[2]
    version = subprocess.run(["limactl", "--version"], capture_output=True,
                             text=True).stdout.split()[-1]
    return f"Lima {version} vz", measure(shell, gateway, vz, None)


def vmuc_instance():
    vmuc = os.environ.get("VMUC")
    if not vmuc:
        subprocess.run(["cargo", "build", "-q", "--release", "-p", "vmuc-cli"], cwd=ROOT, check=True)
        vmuc = os.path.join(ROOT, "target/release/vmuc")
        subprocess.run(["codesign", "--force", "--sign", "-", "--entitlements",
                        os.path.join(ROOT, "vmuc.entitlements"), vmuc], check=True,
                       capture_output=True)
    home = tempfile.mkdtemp(prefix="vmuc-cost.", dir="/tmp")
    env = {**os.environ, "VMUC_HOME": home}

    def vm(*args):
        p = subprocess.run([vmuc, *args], env=env, capture_output=True, text=True,
                           stdin=subprocess.DEVNULL)
        if p.returncode != 0:
            sys.exit(f"vmuc {' '.join(args)} failed:\n{p.stdout}{p.stderr}")
        return p.stdout

    # The build's own checkout, when VMUC points into one.
    commit = subprocess.run(["git", "-C", os.path.dirname(vmuc), "rev-parse", "--short", "HEAD"],
                            capture_output=True, text=True).stdout.strip()
    try:
        vm("create", "ubuntu", "--name", NAME, "--cpus", "4", "--memory", "4GiB")
        vm("start", NAME)
        vm("shell", NAME, "--", "cloud-init status --wait >/dev/null; true")
        gateway = vm("shell", NAME, "--", "ip -4 route show default").split()[2]
        vmm = int(open(os.path.join(home, NAME, "vmm.pid")).read())
        helper = int(subprocess.run(["pgrep", "-x", "vmuc-vmnet-helper"], capture_output=True,
                                    text=True).stdout.split()[-1])
        return commit, measure(lambda cmd: vm("shell", NAME, "--", cmd), gateway, vmm, helper)
    finally:
        subprocess.run([vmuc, "stop", "--force", NAME], env=env, capture_output=True)
        subprocess.run(["rm", "-rf", home])


def main():
    if len(sys.argv) > 2 and sys.argv[1] == "--lima":
        label, cells = lima(sys.argv[2])
    else:
        label, cells = vmuc_instance()
    chip = subprocess.run(["sysctl", "-n", "machdep.cpu.brand_string"], capture_output=True,
                          text=True).stdout.strip()
    row = (f"| {datetime.date.today()} | {chip}, macOS {platform.mac_ver()[0]} | {label} | "
           + " | ".join(cells) + " |\n")
    with open(os.path.join(ROOT, "docs/benchmarks/net-cost.md"), "a") as f:
        f.write(row)


if __name__ == "__main__":
    main()
