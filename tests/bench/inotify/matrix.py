# SPDX-License-Identifier: Apache-2.0
"""What five Linux file watchers in a guest report for file operations on a
shared directory, done in the guest (the reference), on the host, and on
the host followed by a replay in the guest.

    python3 -I matrix.py VMUC INSTANCE SHARE MODE...

MODE is `guest`, `host`, or a replay: `touch` (utimensat of the mtime the
host saw, through the share), `ioctl` (the vmuc kernel's replay ioctl) or
`agent` (the instance's own propagation, nothing replayed by this script). SHARE is the host path of a writable
share mirrored at the same path in the guest, and the guest has run
`guest.sh setup`. Prints one table per mode: operation x watcher.
"""

import os
import shlex
import subprocess
import sys
import time

GUEST_SH = os.path.join(os.path.dirname(os.path.abspath(__file__)), "guest.sh")
TOOLS = ["inotifywait", "watchfiles", "watchdog", "chokidar", "fsnotify"]
SETTLE_S = 3

# (name, shell command run in SHARE, what a host watcher reports: relative
# path and kind, as FSEvents flags it).
OPS = [
    ("modify", "echo more >> a.txt", [("a.txt", "modified")]),
    ("same-size", "printf BBBB | dd of=a.txt bs=1 conv=notrunc 2>/dev/null",
     [("a.txt", "modified")]),
    ("atomic-save", "cp a.txt .a.tmp && echo saved >> .a.tmp && mv .a.tmp a.txt",
     [(".a.tmp", "renamed"), ("a.txt", "renamed")]),
    ("create", "echo new > new.txt", [("new.txt", "created")]),
    ("mkdir+file", "mkdir d2 && echo f > d2/f.txt", [("d2", "created"), ("d2/f.txt", "created")]),
    ("edit-in-new-dir", "echo more >> d2/f.txt", [("d2/f.txt", "modified")]),
    ("rename", "mv b.txt c.txt", [("b.txt", "renamed"), ("c.txt", "renamed")]),
    ("chmod", "chmod 755 a.txt", [("a.txt", "attrib")]),
    ("delete", "rm new.txt", [("new.txt", "removed")]),
    ("rmdir", "rm -rf d2", [("d2/f.txt", "removed"), ("d2", "removed")]),
]
REPLAY_PY = os.path.join(os.path.dirname(os.path.abspath(__file__)), "replay.py")


def ms():
    return time.time_ns() // 1_000_000


class Probe:
    def __init__(self, vmuc, name, share):
        self.vmuc, self.name, self.share = vmuc, name, share

    def guest(self, cmd, check=True):
        return subprocess.run([self.vmuc, "shell", self.name, "--workdir", self.share, "--", cmd],
                              check=check, capture_output=True, text=True,
                              stdin=subprocess.DEVNULL).stdout

    def reset(self):
        subprocess.run(["sh", "-c", "rm -rf ./* ./.a.tmp; echo a > a.txt; echo b > b.txt;"
                        " mkdir sub; echo x > sub/x.txt"], cwd=self.share, check=True)
        time.sleep(1)
        self.guest(f"{GUEST_SH} start {shlex.quote(self.share)}")

    def touch_targets(self, changed):
        """The replay rule: an existing path is touched, a gone one's
        parent, and a created one's parent as well."""
        out = []
        for rel, kind in changed:
            path = os.path.join(self.share, rel)
            parent = os.path.dirname(path)
            exists = os.path.lexists(path)
            for p, ok in ((path, exists),
                          (parent, (kind in ("created", "renamed") or not exists)
                           and os.path.isdir(parent))):
                if ok and p not in out:
                    out.append(p)
        return out

    def replay_touch(self, changed):
        cmds = []
        for p in self.touch_targets(changed):
            ns = os.lstat(p).st_mtime_ns
            cmds.append(f"touch -m -h -d @{ns // 10**9}.{ns % 10**9:09d} {shlex.quote(p)}")
        if cmds:
            self.guest("sudo sh -c " + shlex.quote("; ".join(cmds)))

    def replay_ioctl(self, changed):
        """The kernel replay: a rename pair becomes MOVED_FROM/MOVED_TO,
        the rest CREATE, DELETE, ATTRIB, or MODIFY and CLOSE_WRITE."""
        gone = [r for r, k in changed if k == "renamed"
                and not os.path.lexists(os.path.join(self.share, r))]
        came = [r for r, k in changed if k == "renamed"
                and os.path.lexists(os.path.join(self.share, r))]
        pair = len(gone) == 1 and len(came) == 1
        args = []
        for rel, kind in changed:
            path = os.path.join(self.share, rel)
            if not os.path.isdir(os.path.dirname(path)):
                continue  # the parent's own DELETE covers it
            if not os.path.lexists(path):
                args.append(("MOVED_FROM" if pair else "DELETE") + " " + path)
            elif kind in ("created", "renamed"):
                args.append(("MOVED_TO" if pair else "CREATE") + " " + path)
            elif kind == "attrib":
                args.append("ATTRIB " + path)
            else:
                args += ["MODIFY " + path, "CLOSE_WRITE " + path]
        args.sort(key=lambda a: not a.startswith("MOVED_FROM"))
        self.guest("sudo python3 -I " + REPLAY_PY + " " + " ".join(shlex.quote(a) for a in args))

    def run(self, mode):
        self.reset()
        marks = []
        for name, cmd, changed in OPS:
            marks.append((name, ms()))
            if mode == "guest":
                self.guest(cmd)
            else:
                subprocess.run(["sh", "-c", cmd], cwd=self.share, check=True)
                if mode == "touch":
                    self.replay_touch(changed)
                elif mode == "ioctl":
                    self.replay_ioctl(changed)
            time.sleep(SETTLE_S)
        marks.append(("end", ms()))
        logs = self.guest("cat /tmp/w/*.log", check=False)
        self.guest(f"{GUEST_SH} stop", check=False)
        return marks, logs

    def table(self, mode, marks, logs):
        seen = {}
        for line in logs.splitlines():
            parts = line.split(" ", 2)
            if len(parts) < 3 or not parts[0].isdigit():
                continue
            t, tool, rest = int(parts[0]), parts[1], parts[2]
            op = next((marks[i][0] for i in range(len(marks) - 1)
                       if marks[i][1] <= t < marks[i + 1][1]), None)
            if op is None:
                continue
            ev = rest.replace(self.share + "/", "").replace(self.share, ".")
            lst = seen.setdefault((op, tool), [])
            if ev not in lst:
                lst.append(ev)
        print(f"\n## mode: {mode}\n")
        for name, _, _ in OPS:
            print(f"### {name}")
            for tool in TOOLS:
                evs = seen.get((name, tool), [])
                print(f"  {tool:12} {'; '.join(evs) if evs else '-'}")


def main():
    vmuc, name, share, *modes = sys.argv[1:]
    probe = Probe(vmuc, name, os.path.realpath(share))
    for mode in modes:
        marks, logs = probe.run(mode)
        probe.table(mode, marks, logs)


if __name__ == "__main__":
    main()
