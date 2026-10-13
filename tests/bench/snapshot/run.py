# SPDX-License-Identifier: Apache-2.0
"""snapshot: a busybox guest (4 vCPUs, 2 GiB, the balloon) fills FILL MiB
of RAM with random data and ticks every 100 ms (the initramfs's
`vmuc.test=tick`); `vmuc _boot` snapshots it AFTER seconds in, then
each RAM mode restores it in a new process. A restore must carry on at
the next tick with uptime continuous, every CPU must do work again, and
virtio-rng must answer. Meanwhile the guest frees its data (at tick
FREE), writes it again (REFILL) and frees that (FREE2); the VMM's
phys_footprint is sampled every 250 ms. MODE is copy or map (guest RAM
copied in or mapped copy-on-write), with `-cold` to restore from a copy
of the snapshot written past the page cache (F_NOCACHE), as after a host
reboot. Appends a row per restore to docs/benchmarks/snapshot.md.

    python3 -I tests/bench/snapshot/run.py [FILL] [MODE...]
"""

import ctypes
import datetime
import fcntl
import os
import platform
import re
import subprocess
import sys
import tempfile
import threading
import time

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(os.path.dirname(os.path.dirname(HERE)))
VMUC = os.path.join(ROOT, "target/release/vmuc")
AFTER, FREE, REFILL, FREE2, SETTLE = 8.0, 100, 150, 200, 5.0
MIB = 1 << 20
F_NOCACHE = 48

libc = ctypes.CDLL("/usr/lib/libSystem.B.dylib")


class RusageInfoV4(ctypes.Structure):
    _fields_ = [("uuid", ctypes.c_uint8 * 16)] + [(f"f{i}", ctypes.c_uint64) for i in range(40)]


def footprint(pid):
    ru = RusageInfoV4()
    if libc.proc_pid_rusage(pid, 4, ctypes.byref(ru)) != 0:
        return None
    return ru.f7  # ri_phys_footprint


def cold_copy(snap):
    """A copy of the snapshot whose RAM image is not in the page cache."""
    dst = snap + ".cold"
    subprocess.run(["rm", "-rf", dst], check=True)
    os.mkdir(dst)
    for name in ("state.json", "gic.bin"):
        subprocess.run(["cp", os.path.join(snap, name), dst], check=True)
    fi = os.open(os.path.join(snap, "ram.img"), os.O_RDONLY)
    fo = os.open(os.path.join(dst, "ram.img"), os.O_WRONLY | os.O_CREAT, 0o644)
    for fd in (fi, fo):
        fcntl.fcntl(fd, F_NOCACHE, 1)
    size = os.fstat(fi).st_size
    os.ftruncate(fo, size)
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
    return dst


class Run:
    """A `vmuc _boot` process: console lines and footprint samples, each
    with the time since spawn."""

    def __init__(self, args):
        self.t0 = time.monotonic()
        self.p = subprocess.Popen([VMUC, "_boot", *args], stdin=subprocess.PIPE,
                                  stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        self.lines, self.samples = [], []
        threading.Thread(target=self._read, daemon=True).start()
        threading.Thread(target=self._sample, daemon=True).start()

    def _read(self):
        for raw in self.p.stdout:
            self.lines.append((time.monotonic() - self.t0, raw.decode(errors="replace").strip()))

    def _sample(self):
        while self.p.poll() is None:
            f = footprint(self.p.pid)
            if f is not None:
                self.samples.append((time.monotonic() - self.t0, f))
            time.sleep(0.25)

    def when(self, prefix):
        return next((t for t, l in self.lines if l.startswith(prefix)), None)

    def wait_for(self, prefix, timeout):
        end = time.monotonic() + timeout
        while self.when(prefix) is None and time.monotonic() < end and self.p.poll() is None:
            time.sleep(0.05)
        return self.when(prefix)

    def fp_at(self, t):
        """The last footprint sample at or before `t`, MiB."""
        return max(((s, f) for s, f in self.samples if s <= t), default=(0, 0))[1] // MIB

    def fp_min(self, t0, t1):
        return min((f for s, f in self.samples if t0 <= s <= t1), default=0) // MIB

    def ticks(self):
        return [(int(m.group(1)), float(m.group(2))) for _, l in self.lines
                if (m := re.match(r"VMUC_TICK (\d+) up=([\d.]+)", l))]

    def cpus(self):
        return [list(map(int, l.split()[1:])) for _, l in self.lines if l.startswith("VMUC_CPUS")]

    def quit(self):
        try:
            self.p.stdin.write(b"\x01x")
            self.p.stdin.close()
        except BrokenPipeError:
            pass
        self.p.wait(timeout=30)
        return self.p.stderr.read().decode(errors="replace").replace("\r", "")


def main():
    fill = int(sys.argv[1]) if len(sys.argv) > 1 else 512
    modes = sys.argv[2:] or ["copy", "map"]
    subprocess.run(["cargo", "build", "-q", "--release", "-p", "vmuc-cli"], cwd=ROOT, check=True)
    subprocess.run(["codesign", "--force", "--sign", "-", "--entitlements",
                    os.path.join(ROOT, "vmuc.entitlements"), VMUC], check=True,
                   capture_output=True)
    snap = tempfile.mkdtemp(prefix="vmuc-snap.", dir=os.path.join(ROOT, "target"))
    machine = ["--kernel", os.path.join(ROOT, "guest/kernel/out/Image"),
               "--initrd", os.path.join(ROOT, "guest/kernel/out/busybox-initramfs.cpio"),
               "--cpus", "4", "--memory", "2GiB", "--balloon"]
    cmdline = (f"console=ttyAMA0 panic=-1 vmuc.test=tick vmuc.fill={fill} vmuc.free={FREE} "
               f"vmuc.refill={REFILL} vmuc.free2={FREE2}")

    boot = Run([*machine, "--cmdline", cmdline, "--snapshot", snap, "--snapshot-after", str(AFTER)])
    boot.p.wait(timeout=60)
    err = boot.p.stderr.read().decode(errors="replace").replace("\r", "")
    m = re.search(r"snapshot in ([\d.]+)(m?s): vCPUs stopped in ([\d.]+)(µs|ms)", err)
    if not m:
        sys.exit(f"no snapshot:\n{err}")
    unit = {"s": 1e3, "ms": 1.0, "µs": 1e-3}
    snap_ms = float(m.group(1)) * unit[m.group(2) or "s"]
    pause_ms = float(m.group(3)) * unit[m.group(4)]
    last_tick, last_up = boot.ticks()[-1]
    if last_tick >= FREE:
        sys.exit(f"the snapshot came at tick {last_tick}, after the free at {FREE}")
    ram = os.path.join(snap, "ram.img")
    written = os.path.getsize(ram)
    on_disk = os.stat(ram).st_blocks * 512 // MIB
    boot_fp = boot.fp_at(AFTER)
    print(f"boot: snapshot at tick {last_tick} in {snap_ms:.0f} ms (vCPUs {pause_ms:.2f} ms), "
          f"ram.img {on_disk} MiB on disk of {written // MIB}, footprint {boot_fp} MiB")

    chip = subprocess.run(["sysctl", "-n", "machdep.cpu.brand_string"], capture_output=True,
                          text=True).stdout.strip()
    commit = subprocess.run(["git", "-C", ROOT, "rev-parse", "--short", "HEAD"],
                            capture_output=True, text=True).stdout.strip()
    for mode in modes:
        ram_mode, cold = mode.removesuffix("-cold"), mode.endswith("-cold")
        source = cold_copy(snap) if cold else snap
        r = Run([*machine, "--restore", source, "--restore-ram", ram_mode])
        t_first = r.wait_for("VMUC_TICK", 10)
        t_freed = r.wait_for("VMUC_FREED", 30)
        t_refilled = r.wait_for("VMUC_REFILLED", 30)
        t_freed2 = r.wait_for("VMUC_FREED2", 30)
        time.sleep(SETTLE)
        err = r.quit()
        if None in (t_first, t_freed, t_refilled, t_freed2):
            sys.exit(f"{mode}: the guest did not get through:\n{err}")
        m = re.search(r"restored in ([\d.]+)(m?s)", err)
        restored_ms = float(m.group(1)) * unit[m.group(2) or "s"]
        first_tick, first_up = r.ticks()[0]
        cpus_before, cpus_after = boot.cpus()[-1], r.cpus()[-1]
        checks = {
            "next tick": first_tick == last_tick + 1,
            "uptime": 0 < first_up - last_up < 0.5,
            "every CPU": all(a > b for a, b in zip(cpus_after, cpus_before)),
            "virtio-rng": any("hwrng=16" in l for _, l in r.lines),
        }
        failed = [k for k, ok in checks.items() if not ok]
        fp = {
            "start": r.fp_at(t_first + 0.3),
            "before free": r.fp_at(t_freed),
            "after free": r.fp_min(t_freed, t_freed + SETTLE),
            "refilled": r.fp_at(t_freed2),
            "after free 2": r.fp_min(t_freed2, t_freed2 + SETTLE),
        }
        print(f"{mode}: restored in {restored_ms:.0f} ms, first tick {t_first * 1e3:.0f} ms after "
              f"spawn, footprint {fp}, {'all checks pass' if not failed else 'FAILED ' + str(failed)}")
        row = (f"| {datetime.date.today()} | {chip}, macOS {platform.mac_ver()[0]} | {commit} | "
               f"{fill} | {on_disk} | {snap_ms:.0f} ({pause_ms:.2f}) | {ram_mode}{' (cold)' if cold else ''} | {restored_ms:.0f} | "
               f"{t_first * 1e3:.0f} | {boot_fp} | " + " | ".join(str(v) for v in fp.values()) +
               f" | {'ok' if not failed else 'FAILED: ' + ', '.join(failed)} |\n")
        with open(os.path.join(ROOT, "docs/benchmarks/snapshot.md"), "a") as f:
            f.write(row)
        if cold:
            subprocess.run(["rm", "-rf", source], check=True)
    subprocess.run(["rm", "-rf", snap], check=True)


if __name__ == "__main__":
    main()
