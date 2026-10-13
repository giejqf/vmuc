# SPDX-License-Identifier: Apache-2.0
# watchdog's inotify observer: one line per event.
import sys
import time

from watchdog.events import FileSystemEventHandler
from watchdog.observers import Observer


class Print(FileSystemEventHandler):
    def on_any_event(self, e):
        kind = e.event_type + ("/dir" if e.is_directory else "")
        dest = f" -> {e.dest_path}" if getattr(e, "dest_path", "") else ""
        print(f"{time.time_ns() // 1_000_000} watchdog {kind} {e.src_path}{dest}", flush=True)


o = Observer()
o.schedule(Print(), sys.argv[1], recursive=True)
o.start()
o.join()
