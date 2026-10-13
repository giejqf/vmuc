# SPDX-License-Identifier: Apache-2.0
"""apfs-sparse (E6): which holes a file keeps on the volume of DIR (default:
a temporary directory under target/). For each case it prints st_blocks,
the data extents SEEK_DATA/SEEK_HOLE report after an fsync, and for the
large cases how far the volume's free space fell. With IMAGE, it also
counts the all-zero 64 KiB runs inside IMAGE's data extents (a converted
disk image: zeros there are holes the writer skipped and APFS filled).

    python3 -I tests/bench/apfs-sparse/run.py [DIR] [IMAGE]
"""

import fcntl
import os
import struct
import sys
import tempfile
import time

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(os.path.dirname(os.path.dirname(HERE)))
KIB, MIB, GIB = 1 << 10, 1 << 20, 1 << 30
F_FULLFSYNC, F_PUNCHHOLE = 51, 99
BLOCK = b"\x5a" * 4096


def extents(fd, size):
    out, off = [], 0
    while off < size:
        try:
            data = os.lseek(fd, off, os.SEEK_DATA)
        except OSError:
            break
        hole = os.lseek(fd, data, os.SEEK_HOLE)
        out.append((data, hole))
        off = hole
    return out


def fmt(b):
    return f"{b // MIB}M" if b % MIB == 0 else f"{b // KIB}K"


def flush(fd):
    os.fsync(fd)
    fcntl.fcntl(fd, F_FULLFSYNC)


def punch(fd, off, n):
    fcntl.fcntl(fd, F_PUNCHHOLE, struct.pack("IIqq", 0, 0, off, n))


def free(d):
    s = os.statvfs(d)
    return s.f_bavail * s.f_frsize


class Case:
    def __init__(self, d, size):
        self.d, self.size = d, size
        self.path = os.path.join(d, "case.img")
        self.fd = os.open(self.path, os.O_RDWR | os.O_CREAT | os.O_TRUNC, 0o644)
        os.ftruncate(self.fd, size)
        self.free0 = free(d)

    def show(self, label, space=False):
        ex = extents(self.fd, self.size)
        line = (f"{label:<56} st_blocks {os.fstat(self.fd).st_blocks * 512 // KIB:>8} KiB; "
                + "data " + (", ".join(f"[{fmt(a)},{fmt(b)})" for a, b in ex[:6])
                             + (" …" if len(ex) > 6 else "")))
        if space:
            time.sleep(1)
            line += f"; free space down {(self.free0 - free(self.d)) // MIB} MiB"
        print(line)

    def close(self):
        os.close(self.fd)
        os.unlink(self.path)
        time.sleep(1)


def main():
    d = sys.argv[1] if len(sys.argv) > 1 else tempfile.mkdtemp(dir=os.path.join(ROOT, "target"))
    print("1. 4 KiB written in the middle of a file of SIZE, then fsync")
    for size in [1, 4, 16, 32, 64, 1024]:
        c = Case(d, size * MIB)
        os.pwrite(c.fd, BLOCK, size * MIB // 2)
        dirty = os.fstat(c.fd).st_blocks * 512 // KIB
        flush(c.fd)
        c.show(f"   {size} MiB (before the flush: {dirty} KiB)")
        c.close()

    print("2. 4 KiB writes after gaps of 64K..32M, front to back, in a 1 GiB file")
    c = Case(d, GIB)
    at = 64 * MIB
    for gap in [64 * KIB, MIB, 4 * MIB, 8 * MIB, 12 * MIB, 15 * MIB, 16 * MIB, 17 * MIB, 32 * MIB]:
        os.pwrite(c.fd, BLOCK, at)
        at += 4096 + gap
    os.pwrite(c.fd, BLOCK, at)
    flush(c.fd)
    c.show("   one flush")
    c.close()

    print("3. Across flushes, and holes punched after one")
    c = Case(d, GIB)
    os.pwrite(c.fd, BLOCK, 0)
    flush(c.fd)
    os.pwrite(c.fd, BLOCK, 8 * MIB)
    flush(c.fd)
    c.show("   4K at 0, flushed; 4K at 8M, flushed")
    c.close()
    c = Case(d, GIB)
    os.pwrite(c.fd, b"\x5a" * (64 * MIB), 0)
    flush(c.fd)
    punch(c.fd, 32 * MIB, MIB)
    flush(c.fd)
    os.pwrite(c.fd, BLOCK, 31 * MIB)
    flush(c.fd)
    os.pwrite(c.fd, BLOCK, 32 * MIB + 512 * KIB)
    flush(c.fd)
    c.show("   64M written; 1M punched at 32M; 4K beside it, 4K in it")
    c.close()

    print("4. 128 x 4 KiB, 8 MiB apart over 1 GiB (512 KiB of data)")
    offs = [i * 8 * MIB for i in range(128)]
    for label, order, punched, top_first in [
        ("front to back", offs, False, False),
        ("back to front", list(reversed(offs)), False, False),
        ("front to back, then the gaps punched", offs, True, False),
        ("the last first and flushed, then the rest", offs, False, True),
    ]:
        c = Case(d, GIB)
        if top_first:
            os.pwrite(c.fd, BLOCK, offs[-1])
            flush(c.fd)
        for o in order:
            os.pwrite(c.fd, BLOCK, o)
        flush(c.fd)
        if punched:
            for o in offs:
                punch(c.fd, o + 16 * KIB, 8 * MIB - 16 * KIB)
            flush(c.fd)
        c.show(f"   {label}", space=True)
        c.close()

    if len(sys.argv) > 2:
        image = sys.argv[2]
        fd = os.open(image, os.O_RDONLY)
        st = os.fstat(fd)
        data = zero = 0
        for a, b in extents(fd, st.st_size):
            for pos in range(a, b, 8 * MIB):
                buf = os.pread(fd, min(8 * MIB, b - pos), pos)
                for i in range(0, len(buf), 64 * KIB):
                    run = buf[i:i + 64 * KIB]
                    if run.count(0) == len(run):
                        zero += len(run)
                    else:
                        data += len(run)
        os.close(fd)
        print(f"5. {image}: {st.st_size // MIB} MiB, {st.st_blocks * 512 // MIB} MiB allocated; "
              f"non-zero 64K runs {data // MIB} MiB, all-zero {zero // MIB} MiB")


if __name__ == "__main__":
    main()
