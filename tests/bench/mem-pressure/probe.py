# SPDX-License-Identifier: Apache-2.0
"""mem-pressure: one guest workload under one simulated host pressure level.

Called by run.sh (which builds and signs vmuc). Starts the instance,
prepares the workload, settles, drops the guest's caches, then holds the
level with `vmuc _pressure` while the workload runs, sampling `vmuc stat`
every second. Guest-side costs come from /proc before and after: memory
stall time (PSI `some` total), major faults, OOM kills. Appends one row to
docs/benchmarks/mem-pressure.md and writes the samples to
target/bench/mem-pressure/.
"""
import datetime, json, os, statistics, subprocess, sys, threading, time

vmuc, name, workload, level, seconds, root = sys.argv[1:7]
seconds = int(seconds)
MIB = 1 << 20
PULSE = 180  # `pulse`: critical and normal alternate this often

WORKLOADS = {
    # Nothing: what the policy does to a guest that does not need memory.
    "idle": ("true", "sleep {s}"),
    # Re-reads a 3 GiB file: a working set that fits the guest's RAM only
    # as page cache.
    "cache": (
        "f=/var/tmp/e1-cache.bin; [ -s $f ] || dd if=/dev/urandom of=$f bs=1M count=3072 status=none",
        "end=$((SECONDS+{s})); n=0; while [ $SECONDS -lt $end ]; do "
        "dd if=/var/tmp/e1-cache.bin of=/dev/null bs=1M status=none; n=$((n+1)); done; "
        "echo result=$((n*3)) GiB read",
    ),
    # Bursts of 2.5 GiB of anonymous memory, held 30 s, freed for 30 s:
    # a link step or a test suite arriving while the balloon is inflated.
    "anon": (
        "true",
        "python3 -c \"\n"
        "import time\n"
        "end = time.time() + {s}; worst = 0.0; n = 0\n"
        "while time.time() < end:\n"
        "    t = time.time(); b = bytearray(2560 << 20)\n"
        "    for i in range(0, len(b), 4096): b[i] = 1\n"
        "    worst = max(worst, time.time() - t); n += 1\n"
        "    time.sleep(30); del b; time.sleep(30)\n"
        "print(f'result={{n}} bursts, slowest {{worst:.1f}} s')\n\"",
    ),
    # vmuc's own kernel config, built from scratch with -j4.
    "build": (
        "set -e; [ -d /var/tmp/linux ] || { sudo DEBIAN_FRONTEND=noninteractive apt-get install -y -qq "
        "--no-install-recommends build-essential flex bison bc libssl-dev libelf-dev dwarves >/dev/null; "
        "curl -fsSL https://cdn.kernel.org/pub/linux/kernel/v6.x/linux-6.18.55.tar.xz | tar -C /var/tmp -xJ; "
        "mv /var/tmp/linux-6.18.55 /var/tmp/linux; }",
        "rm -rf /var/tmp/kobj && mkdir -p /var/tmp/kobj && cp /var/tmp/vmuc.config /var/tmp/kobj/.config && "
        "make -s -C /var/tmp/linux O=/var/tmp/kobj ARCH=arm64 olddefconfig >/dev/null && "
        "t=$SECONDS && make -s -C /var/tmp/linux O=/var/tmp/kobj ARCH=arm64 -j4 Image >/dev/null 2>&1; "
        "echo result=build exit $? in $((SECONDS-t)) s",
    ),
}


def run(*args, **kw):
    return subprocess.run([vmuc, *args], capture_output=True, text=True, **kw)


def shell(cmd, **kw):
    return run("shell", name, "--workdir", "/", "--", cmd, stdin=subprocess.DEVNULL, **kw)


def guest_counters():
    out = shell(
        "grep '^some' /proc/pressure/memory; grep -E '^(pgmajfault|oom_kill) ' /proc/vmstat"
    ).stdout
    c = {}
    for line in out.splitlines():
        if line.startswith("some"):
            c["stall_us"] = int(line.split("total=")[1])
        else:
            k, v = line.split()
            c[k] = int(v)
    return c


def pressure(lvl, span):
    run("_pressure", name, lvl, "--for", str(span), check=True)


prep, body = WORKLOADS[workload]
run("stop", name)
run("start", name, check=True)
if workload == "build":
    run("copy", f"{root}/guest/kernel/out/config", f"{name}:/var/tmp/vmuc.config", check=True)
r = shell(prep, timeout=1800)
if r.returncode != 0:
    sys.exit(f"prepare failed: {r.stderr[-500:]}")
time.sleep(60)
run("reclaim", name, check=True)
time.sleep(10)
before = guest_counters()

result = {}
def work():
    r = shell(body.format(s=seconds), timeout=seconds * 4 + 600)
    result["out"] = (r.stdout + r.stderr).strip().splitlines()
    result["status"] = r.returncode
worker = threading.Thread(target=work)
t0 = time.time()
worker.start()

samples, lvl, switched = [], None, 0.0
while worker.is_alive() or time.time() - t0 < seconds:
    now = time.time() - t0
    want = level if level != "pulse" else ("critical" if int(now // PULSE) % 2 == 0 else "normal")
    if want != lvl or now - switched > 60:
        pressure(want, 120)
        lvl, switched = want, now
    s = json.loads(run("stat", name, "--json", check=True).stdout)
    samples.append({"t": round(now, 1), "level": want, "footprint": s["host_footprint"],
                    "balloon": s["balloon"], "available": s["guest_available"],
                    "cpu_ns": s["host_cpu_nanos"]})
    time.sleep(max(0.0, 1.0 - (time.time() - t0 - now)))
    if not worker.is_alive() and workload == "build":
        break
pressure("normal", 0)
worker.join()
after = guest_counters()
run("stop", name)

fp = [x["footprint"] / MIB for x in samples]
cpu = (samples[-1]["cpu_ns"] - samples[0]["cpu_ns"]) / 1e9 / max(1e-9, samples[-1]["t"] - samples[0]["t"]) * 100
bal = [x["balloon"] / MIB for x in samples]
avail = [x["available"] / MIB for x in samples if x["available"]]
# Balloon episodes: each change of direction (inflating, deflating).
moves = []
for a, b in zip(bal, bal[1:]):
    if abs(b - a) >= 16:
        d = 1 if b > a else -1
        if not moves or moves[-1] != d:
            moves.append(d)
res = next((l.split("result=", 1)[1] for l in result.get("out", []) if "result=" in l),
           f"exit {result.get('status')}")
policy_on = "pressureBalloon: true" in open(os.path.expanduser(f"~/.vmuc/{name}/vmuc.yaml")).read()
sha = subprocess.run(["git", "-C", root, "rev-parse", "--short", "HEAD"], capture_output=True, text=True).stdout.strip()
dirty = subprocess.run(["git", "-C", root, "diff", "--quiet", "HEAD", "--", "crates"]).returncode != 0
mac = subprocess.run(["sw_vers", "-productVersion"], capture_output=True, text=True).stdout.strip()
chip = subprocess.run(["sysctl", "-n", "machdep.cpu.brand_string"], capture_output=True, text=True).stdout.strip()
q = statistics.quantiles(fp, n=20)
row = (f"| {datetime.date.today()} | {chip}, macOS {mac} | {name} | balloon {'on' if policy_on else 'off'} "
       f"({sha}{'+' if dirty else ''}) | {workload} | {level} | {round(time.time() - t0)} | "
       f"{statistics.mean(fp):.0f} / {q[18]:.0f} / {max(fp):.0f} | {max(bal):.0f} | {len(moves)} | "
       f"{min(avail) if avail else 0:.0f} | {cpu:.0f} | {(after['stall_us'] - before['stall_us']) / 1e6:.1f} | "
       f"{after['pgmajfault'] - before['pgmajfault']} | {after['oom_kill'] - before['oom_kill']} | {res} |")
with open(f"{root}/docs/benchmarks/mem-pressure.md", "a") as f:
    f.write(row + "\n")
os.makedirs(f"{root}/target/bench/mem-pressure", exist_ok=True)
stamp = datetime.datetime.now().strftime("%Y%m%d-%H%M%S")
with open(f"{root}/target/bench/mem-pressure/{stamp}-{name}-{workload}-{level}.json", "w") as f:
    json.dump({"row": row, "samples": samples, "out": result.get("out")}, f)
print(row)
