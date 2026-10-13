# SPDX-License-Identifier: Apache-2.0
# Replays host changes in the guest with the vmuc kernel's
# FUSE_VMUC_IOC_REPLAY (guest/kernel/patches/0002-*), as root. One
# "EVENT PATH" per argument; MOVED_TO takes the cookie of the last
# MOVED_FROM.
import ctypes
import fcntl
import os
import struct
import sys

EVENTS = {"MODIFY": 0x2, "ATTRIB": 0x4, "CLOSE_WRITE": 0x8, "MOVED_FROM": 0x40,
          "MOVED_TO": 0x80, "CREATE": 0x100, "DELETE": 0x200}
IOC_REPLAY = (3 << 30) | (24 << 16) | (229 << 8) | 0x76

cookie = 0
for arg in sys.argv[1:]:
    event, path = arg.split(" ", 1)
    parent, base = os.path.split(path)
    name = ctypes.create_string_buffer(base.encode())
    buf = bytearray(struct.pack("IIIIQ", EVENTS[event], cookie if event == "MOVED_TO" else 0,
                                len(base.encode()), 0, ctypes.addressof(name)))
    try:
        fd = os.open(parent, os.O_RDONLY | os.O_DIRECTORY)
        try:
            fcntl.ioctl(fd, IOC_REPLAY, buf)
        finally:
            os.close(fd)
    except OSError as e:
        print(f"{event} {path}: {e}", file=sys.stderr)
    if event == "MOVED_FROM":
        cookie = struct.unpack_from("I", buf, 4)[0]
