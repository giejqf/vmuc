# SPDX-License-Identifier: Apache-2.0
# Guest-side stall detector: the largest gap between CLOCK_MONOTONIC reads
# on one vCPU. Usage inside the guest: python3 stall.py SECONDS CPU

import os, sys, time
dur = float(sys.argv[1]); cpu = int(sys.argv[2])
os.sched_setaffinity(0, {cpu})
end = time.monotonic_ns() + int(dur * 1e9)
prev = time.monotonic_ns(); worst = 0; gaps = 0
while prev < end:
    now = time.monotonic_ns()
    d = now - prev
    if d > worst: worst = d
    if d > 1_000_000: gaps += 1
    prev = now
print(f"cpu{cpu} max gap {worst/1000:.0f} us, gaps>1ms: {gaps}")
