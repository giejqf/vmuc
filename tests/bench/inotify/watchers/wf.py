# SPDX-License-Identifier: Apache-2.0
# watchfiles (notify-rs underneath): one line per change, no filter.
import sys
import time

import watchfiles

for changes in watchfiles.watch(sys.argv[1], watch_filter=None, debounce=100, step=10):
    for change, path in sorted(changes):
        print(f"{time.time_ns() // 1_000_000} watchfiles {change.name} {path}", flush=True)
