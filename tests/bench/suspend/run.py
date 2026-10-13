# SPDX-License-Identifier: Apache-2.0
"""suspend: an Ubuntu instance (4 vCPUs, 4 GiB, a fresh VMUC_HOME) boots,
finishes cloud-init and writes FILL MiB of random data to /dev/shm; then
`vmuc suspend` saves it (seconds for the command, milliseconds the guest
was stopped, MiB of RAM image on disk) and `vmuc start` resumes it
(seconds until ssh works, milliseconds until the guest ran again). The
VMM's footprint is read 2 s after the resume and again after the guest
has read all of its data back. A `-cold` mode rewrites the RAM image
past the page cache before the resume (F_NOCACHE), as after a host
reboot. The boot before is the reference. Appends a row per mode to
docs/benchmarks/suspend.md.

    python3 -I tests/bench/suspend/run.py [FILL] [MODE...]   (MODE: warm, cold)
"""

import datetime
import fcntl
import json
import os
import platform
import re
import subprocess
import sys
import tempfile
import time

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(os.path.dirname(os.path.dirname(HERE)))
VMUC = os.path.join(ROOT, "target/release/vmuc")
NAME = "bench"
MIB = 1 << 20
F_NOCACHE = 48


def vmuc(home, *args, check=True):
    t = time.monotonic()
    p = subprocess.run([VMUC, *args], env={**os.environ, "VMUC_HOME": home},
                       capture_output=True, text=True, stdin=subprocess.DEVNULL)
    if check and p.returncode != 0:
        sys.exit(f"vmuc {' '.join(args)} failed:\n{p.stdout}{p.stderr}")
    return time.monotonic() - t, p.stdout + p.stderr


def footprint(home):
    _, out = vmuc(home, "stat", NAME, "--json")
    return json.loads(out[out.index("{"):])["host_footprint"] // MIB


def make_cold(path):
    """Rewrites `path` through F_NOCACHE, holes kept, so none of it is cached."""
    tmp = path + ".cold"
    fi = os.open(path, os.O_RDONLY)
    fo = os.open(tmp, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o644)
    for fd in (fi, fo):
        fcntl.fcntl(fd, F_NOCACHE, 1)
    os.ftruncate(fo, os.fstat(fi).st_size)
    off = 0
    while True:
        try:
            data = os.lseek(fi, off, os.SEEK_DATA)
        except OSError:
            break
        hole = os.lseek(fi, data, os.SEEK_HOLE)
        for pos in range(data, hole, 8 * MIB):
            n = min(8 * MIB, hole - pos)
            os.pwrite(fo, os.pread(fi, n, pos), pos)
        off = hole
    os.fsync(fo)
    os.close(fo)
    os.close(fi)
    os.rename(tmp, path)


def main():
    fill = int(sys.argv[1]) if len(sys.argv) > 1 else 0
    modes = sys.argv[2:] or ["warm", "cold"]
    subprocess.run(["cargo", "build", "-q", "--release", "-p", "vmuc-cli"], cwd=ROOT, check=True)
    subprocess.run(["codesign", "--force", "--sign", "-", "--entitlements",
                    os.path.join(ROOT, "vmuc.entitlements"), VMUC], check=True,
                   capture_output=True)
    home = tempfile.mkdtemp(prefix="vmuc-bench.", dir="/tmp")
    chip = subprocess.run(["sysctl", "-n", "machdep.cpu.brand_string"], capture_output=True,
                          text=True).stdout.strip()
    commit = subprocess.run(["git", "-C", ROOT, "rev-parse", "--short", "HEAD"],
                            capture_output=True, text=True).stdout.strip()
    inst = os.path.join(home, NAME)
    try:
        vmuc(home, "create", "ubuntu", "--name", NAME, "--cpus", "4", "--memory", "4GiB")
        for mode in modes:
            _, out = vmuc(home, "start", NAME)
            boot_s = float(re.search(r"is running \(([\d.]+) s\)", out).group(1))
            vmuc(home, "shell", NAME, "--", "cloud-init status --wait >/dev/null; true")
            if fill:
                vmuc(home, "shell", NAME, "--", f"head -c {fill}M /dev/urandom > /dev/shm/fill")
            time.sleep(5)
            before_fp = footprint(home)
            suspend_s, out = vmuc(home, "suspend", NAME)
            stopped_ms = int(re.search(r"stopped for (\d+) ms", out).group(1))
            ram = os.path.join(inst, "suspend", "ram.img")
            on_disk = os.stat(ram).st_blocks * 512 // MIB
            if mode == "cold":
                make_cold(ram)
            resume_s, out = vmuc(home, "start", NAME)
            restore_ms = int(re.search(r"resumed from its suspended state in (\d+) ms",
                                       out).group(1))
            time.sleep(2)
            after_fp = footprint(home)
            vmuc(home, "shell", NAME, "--", "cat /dev/shm/fill > /dev/null 2>&1; true")
            read_fp = footprint(home)
            vmuc(home, "stop", NAME)
            print(f"{mode}: boot {boot_s:.1f} s, footprint {before_fp} MiB; suspend "
                  f"{suspend_s:.2f} s (guest stopped {stopped_ms} ms), ram.img {on_disk} MiB; "
                  f"resume {resume_s:.2f} s (restored in {restore_ms} ms), footprint "
                  f"{after_fp} MiB, {read_fp} MiB after reading the data back")
            row = (f"| {datetime.date.today()} | {chip}, macOS {platform.mac_ver()[0]} | "
                   f"{commit} | {fill} | {mode} | {boot_s:.1f} | {before_fp} | "
                   f"{suspend_s:.2f} ({stopped_ms}) | {on_disk} | {resume_s:.2f} "
                   f"({restore_ms}) | {after_fp} | {read_fp} |\n")
            with open(os.path.join(ROOT, "docs/benchmarks/suspend.md"), "a") as f:
                f.write(row)
    finally:
        vmuc(home, "stop", "--force", NAME, check=False)
        subprocess.run(["rm", "-rf", home], check=False)


if __name__ == "__main__":
    main()
