# SPDX-License-Identifier: Apache-2.0
"""What propagating host changes costs: the round trip from a host change
to a guest watcher (inotifywait) seeing it, a burst of created files, and
the VMM's CPU while the guest is idle.

    python3 -I cost.py VMUC INSTANCE SHARE [LABEL]

SHARE is a writable share mirrored at the same path in the guest, which
has inotify-tools. The guest answers each event with a file in
SHARE/ack, which is outside the watched directory and, being the VMM's
own write, not propagated back; the host polls for it, so no clock sync
is involved. Appends a row to docs/benchmarks/inotify.md.
"""

import datetime
import os
import platform
import shlex
import statistics
import subprocess
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(os.path.dirname(os.path.dirname(HERE)))
LONE, BURST, IDLE_S = 50, 2000, 60


def wait_for(path, timeout=30):
    t0 = time.perf_counter()
    while not os.path.exists(path):
        if time.perf_counter() - t0 > timeout:
            raise TimeoutError(path)
        time.sleep(0.0002)
    return time.perf_counter() - t0


def cpu_s(pid):
    out = subprocess.run(["ps", "-o", "time=", "-p", str(pid)], check=True,
                         capture_output=True, text=True).stdout.strip()
    m, s = out.split(":")
    return int(m) * 60 + float(s)


def main():
    vmuc, name, share, *label = sys.argv[1:]
    share = os.path.realpath(share)
    d = os.path.join(share, "cost")
    subprocess.run(["rm", "-rf", d], check=True)
    os.makedirs(os.path.join(d, "ack"))
    with open(os.path.join(d, "mod.txt"), "w") as f:
        f.write("0\n")
    pid = int(open(os.path.expanduser(f"~/.vmuc/{name}/vmm.pid")).read())
    watch = (f"inotifywait -m -q -e create,modify --format '%e %f' {shlex.quote(d)}"
             f" | python3 -u {HERE}/ack.py {shlex.quote(d)} {BURST}")
    subprocess.run([vmuc, "shell", name, "--", f"nohup sh -c {shlex.quote(watch)} "
                    ">/dev/null 2>&1 &"], check=True, stdin=subprocess.DEVNULL)
    time.sleep(2)

    created, modified = [], []
    for i in range(LONE):
        t0 = time.perf_counter()
        open(os.path.join(d, f"trig-{i}"), "w").close()
        wait_for(os.path.join(d, "ack", f"ack-c{i}"))
        created.append(time.perf_counter() - t0)
        time.sleep(0.3)
        t0 = time.perf_counter()
        with open(os.path.join(d, "mod.txt"), "a") as f:
            f.write(f"{i}\n")
        wait_for(os.path.join(d, "ack", f"ack-m{i + 1}"))
        modified.append(time.perf_counter() - t0)
        time.sleep(0.3)

    c0, t0 = cpu_s(pid), time.perf_counter()
    for i in range(BURST):
        open(os.path.join(d, f"burst-{i}"), "w").close()
    burst = wait_for(os.path.join(d, "ack", "ack-burst"), timeout=120)
    time.sleep(1)
    burst_cpu = cpu_s(pid) - c0

    subprocess.run([vmuc, "shell", name, "--", "pkill -x inotifywait"],
                   stdin=subprocess.DEVNULL)
    time.sleep(5)
    c0 = cpu_s(pid)
    time.sleep(IDLE_S)
    idle = (cpu_s(pid) - c0) / IDLE_S * 100

    ms = lambda xs, q: statistics.quantiles(xs, n=20)[q] * 1000
    print(f"create: p50 {ms(created, 9):.2f} ms  p95 {ms(created, 18):.2f} ms")
    print(f"modify: p50 {ms(modified, 9):.2f} ms  p95 {ms(modified, 18):.2f} ms")
    print(f"burst of {BURST} creates: {burst:.2f} s to the last event, VMM CPU {burst_cpu:.2f} s")
    print(f"idle VMM CPU: {idle:.2f} %")
    chip = subprocess.run(["sysctl", "-n", "machdep.cpu.brand_string"], capture_output=True,
                          text=True).stdout.strip()
    commit = subprocess.run(["git", "-C", ROOT, "rev-parse", "--short", "HEAD"],
                            capture_output=True, text=True).stdout.strip()
    row = (f"| {datetime.date.today()} | {chip}, macOS {platform.mac_ver()[0]} | {commit} | "
           f"{name}{' (' + label[0] + ')' if label else ''} | "
           f"{ms(created, 9):.1f} / {ms(created, 18):.1f} | {ms(modified, 9):.1f} / "
           f"{ms(modified, 18):.1f} | {burst:.2f} s, CPU {burst_cpu:.2f} s | {idle:.2f} % |\n")
    with open(os.path.join(ROOT, "docs/benchmarks/inotify.md"), "a") as f:
        f.write(row)
    subprocess.run(["rm", "-rf", d], check=True)


if __name__ == "__main__":
    main()
