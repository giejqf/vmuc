# SPDX-License-Identifier: Apache-2.0
# Guest side of cost.py: reads `inotifywait -m --format '%e %f'` lines and
# answers in DIR/ack (outside the watch) so the host can time each event:
# ack-c<N> for a created trig-<N>, ack-m<K> for the K-th MODIFY of
# mod.txt, ack-burst once COUNT burst-* files were created.
import os
import sys

d, count = sys.argv[1], int(sys.argv[2])
mods = bursts = 0


def ack(name):
    open(os.path.join(d, "ack", name), "w").close()


for line in sys.stdin:
    event, _, name = line.rstrip("\n").partition(" ")
    if "CREATE" in event and name.startswith("trig-"):
        ack("ack-c" + name[5:])
    elif "MODIFY" in event and name == "mod.txt":
        mods += 1
        ack(f"ack-m{mods}")
    elif "CREATE" in event and name.startswith("burst-"):
        bursts += 1
        if bursts == count:
            ack("ack-burst")
