# SPDX-License-Identifier: Apache-2.0
"""File system workload for tests/bench/fs: python3 workload.py DIR [DROP]

Sequential 1 GiB write (1 MiB writes, fsync), sequential read after the
page cache is dropped (when DROP is 1, which needs root on Linux), then
5000 small files: create, stat (cold), read, unlink, and the latency of
open+close on one file. Prints one JSON line.
"""
import json, os, sys, time

d, drop = sys.argv[1], sys.argv[2:] == ["1"]
os.makedirs(d, exist_ok=True)
out = {}

def drop_caches():
    if drop:
        os.sync()
        with open("/proc/sys/vm/drop_caches", "w") as f:
            f.write("3\n")

block = os.urandom(1 << 20)
big = os.path.join(d, "big")
t = time.perf_counter()
fd = os.open(big, os.O_CREAT | os.O_WRONLY | os.O_TRUNC, 0o644)
for _ in range(1024):
    os.write(fd, block)
os.fsync(fd)
os.close(fd)
out["write_mbps"] = round(1024 / (time.perf_counter() - t))

drop_caches()
t = time.perf_counter()
fd = os.open(big, os.O_RDONLY)
while os.read(fd, 1 << 20):
    pass
os.close(fd)
out["read_mbps"] = round(1024 / (time.perf_counter() - t))
os.unlink(big)

tree = os.path.join(d, "tree")
small = os.urandom(4096)
paths = [os.path.join(tree, f"d{i % 50}", f"f{i}") for i in range(5000)]
t = time.perf_counter()
for i in range(50):
    os.makedirs(os.path.join(tree, f"d{i}"), exist_ok=True)
for p in paths:
    with open(p, "wb") as f:
        f.write(small)
out["create_ms"] = round((time.perf_counter() - t) * 1000)

drop_caches()
t = time.perf_counter()
n = sum(1 for root, _, files in os.walk(tree) for f in files if os.lstat(os.path.join(root, f)).st_size)
assert n == 5000, n
out["stat_ms"] = round((time.perf_counter() - t) * 1000)

drop_caches()
t = time.perf_counter()
for p in paths:
    with open(p, "rb") as f:
        f.read()
out["read_small_ms"] = round((time.perf_counter() - t) * 1000)

t = time.perf_counter()
for p in paths:
    os.unlink(p)
for i in range(50):
    os.rmdir(os.path.join(tree, f"d{i}"))
os.rmdir(tree)
out["unlink_ms"] = round((time.perf_counter() - t) * 1000)

# open+close: on virtiofs one OPEN and one RELEASE round trip each.
one = os.path.join(d, "one")
open(one, "w").close()
n = 20000
t = time.perf_counter()
for _ in range(n):
    os.close(os.open(one, os.O_RDONLY))
out["open_close_us"] = round((time.perf_counter() - t) / n * 1e6, 1)
os.unlink(one)
print(json.dumps(out))
