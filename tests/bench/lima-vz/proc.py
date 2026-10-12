# SPDX-License-Identifier: Apache-2.0
"""proc.py PID...: user+system CPU seconds (sum) and phys_footprint bytes
(sum) of the processes, the counters `vmuc stat` uses for its own VMM.
Prints one JSON line."""
import ctypes, json, sys

libc = ctypes.CDLL("/usr/lib/libSystem.B.dylib")


class RusageInfoV4(ctypes.Structure):
    _fields_ = [("uuid", ctypes.c_uint8 * 16)] + [(f"f{i}", ctypes.c_uint64) for i in range(40)]


class Timebase(ctypes.Structure):
    _fields_ = [("numer", ctypes.c_uint32), ("denom", ctypes.c_uint32)]


tb = Timebase()
libc.mach_timebase_info(ctypes.byref(tb))
cpu, footprint = 0.0, 0
for pid in map(int, sys.argv[1:]):
    ru = RusageInfoV4()
    if libc.proc_pid_rusage(pid, 4, ctypes.byref(ru)) != 0:
        sys.exit(f"proc_pid_rusage({pid}) failed")
    # f0 user, f1 system time in Mach ticks; f7 phys_footprint.
    cpu += (ru.f0 + ru.f1) * tb.numer / tb.denom / 1e9
    footprint += ru.f7
print(json.dumps({"cpu_s": round(cpu, 3), "footprint": footprint}))
