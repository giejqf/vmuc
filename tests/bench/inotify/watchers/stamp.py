# SPDX-License-Identifier: Apache-2.0
# Prefixes each stdin line with epoch milliseconds and a tool name.
import sys
import time

tool = sys.argv[1]
for line in sys.stdin:
    print(f"{time.time_ns() // 1_000_000} {tool} {line.rstrip()}", flush=True)
