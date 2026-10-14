# SPDX-License-Identifier: Apache-2.0
"""cpu-crypto: does the guest get the host's crypto instructions, and use
them? Builds kernels.rs for the host and for the guest and runs both
(instruction throughput, std's runtime detection). In an Ubuntu instance
(4 vCPUs, 4 GiB, a fresh VMUC_HOME): the crypto features in
/proc/cpuinfo, the drivers the kernel picked (/proc/crypto), the kernel's
drivers through AF_ALG (afalg.py) and OpenSSL's `speed`, also on the host
when Homebrew's OpenSSL is installed. VMUC_KERNEL and VMUC_KERNEL_CMDLINE
pass through to vmuc (e.g. `arm64.nosme`). Appends a dated section to
docs/benchmarks/cpu-crypto.md.

    python3 -I tests/bench/cpu-crypto/run.py
"""

import datetime
import os
import platform
import subprocess
import sys
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(os.path.dirname(os.path.dirname(HERE)))
NAME = "crypto"
FEATURES = ["aes", "pmull", "sha1", "sha2", "sha3", "sha512", "crc32", "dit", "sm3", "sm4", "rng"]
ALGS = ["aes", "xts(aes)", "gcm(aes)", "sha256", "sha512", "sha3-256", "crc32c"]
SPEED = ["aes-128-gcm", "aes-256-xts", "sha256", "sha512", "sha3-256", "chacha20-poly1305"]
HOST_OPENSSL = "/opt/homebrew/opt/openssl/bin/openssl"
# OpenSSL's capability bits for what an M4 has, without the two paths it
# enables on macOS by brand string and on Linux only for listed core IDs
# (ARMV8_UNROLL8_EOR3, ARMV8_HAVE_SHA3_AND_WORTH_USING): what it picks
# for this CPU on Linux.
LINUX_ARMCAP = "0x87d"
DRIVERS = r'''
best = {}
for block in open("/proc/crypto").read().split("\n\n"):
    d = dict(l.split(":", 1) for l in block.splitlines() if ":" in l)
    d = {k.strip(): v.strip() for k, v in d.items()}
    if d and int(d["priority"]) > int(best.get(d["name"], {}).get("priority", -1)):
        best[d["name"]] = d
for name in ALGS:
    d = best.get(name)
    print(name, d["driver"] + ("" if d.get("selftest", "passed") == "passed" else " (selftest failed)") if d else "-")
'''


def sh(*args, **kw):
    return subprocess.run(args, capture_output=True, text=True, check=True, **kw).stdout


def build(target):
    out = os.path.join(tempfile.gettempdir(), f"vmuc-kernels-{target}")
    extra = ["-C", "linker=rust-lld", "-C", "target-feature=+crt-static",
             "-C", "link-self-contained=yes"] if "linux" in target else []
    sh("rustc", "--edition", "2024", "-O", "-C", "target-cpu=generic", "--target", target,
       *extra, os.path.join(HERE, "kernels.rs"), "-o", out, cwd=ROOT)
    return out


def pairs(text):
    return dict(line.split(None, 1) for line in text.splitlines() if line.strip())


def speed(run, env=""):
    """GB/s per algorithm from `openssl speed -mr`, 16 KiB blocks."""
    out = {}
    for alg in SPEED:
        lines = run(f"{env}openssl speed -mr -elapsed -seconds 1 -bytes 16384 -evp {alg} 2>/dev/null")
        f = [l for l in lines.splitlines() if l.startswith("+F")]
        out[alg] = f"{float(f[0].split(':')[3]) / 1e9:.2f}" if f else "-"
    return out


def table(head, rows):
    lines = ["| " + " | ".join(head) + " |", "|" + "---|" * len(head)]
    lines += ["| " + " | ".join(r) + " |" for r in rows]
    return "\n".join(lines) + "\n"


def main():
    host_kernels = pairs(sh(build("aarch64-apple-darwin")))
    guest_bin = build("aarch64-unknown-linux-musl")
    host_speed = host_linux = {}
    if os.path.exists(HOST_OPENSSL):
        host = lambda c: subprocess.run(c.replace("openssl", HOST_OPENSSL, 1), shell=True,
                                        capture_output=True, text=True).stdout
        host_speed = speed(host)
        host_linux = speed(host, f"OPENSSL_armcap={LINUX_ARMCAP} ")
        host_speed["version"] = host_linux["version"] = sh(HOST_OPENSSL, "version").split()[1]

    vmuc = os.environ.get("VMUC")
    if not vmuc:
        subprocess.run(["cargo", "build", "-q", "--release", "-p", "vmuc-cli"], cwd=ROOT, check=True)
        vmuc = os.path.join(ROOT, "target/release/vmuc")
        sh("codesign", "--force", "--sign", "-", "--entitlements",
           os.path.join(ROOT, "vmuc.entitlements"), vmuc)
    home = tempfile.mkdtemp(prefix="vmuc-crypto.", dir="/tmp")
    env = {**os.environ, "VMUC_HOME": home}

    def vm(*args, data=b""):
        p = subprocess.run([vmuc, *args], env=env, capture_output=True, input=data)
        if p.returncode != 0:
            sys.exit(f"vmuc {' '.join(args)} failed:\n{p.stdout.decode()}{p.stderr.decode()}")
        return p.stdout.decode()

    try:
        vm("create", "ubuntu", "--name", NAME, "--cpus", "4", "--memory", "4GiB")
        vm("start", NAME)
        run = lambda cmd: vm("shell", NAME, "--", cmd)
        run("cloud-init status --wait >/dev/null; true")
        kernel = " ".join(filter(None, [run("uname -r").strip(),
                                        os.path.basename(os.environ.get("VMUC_KERNEL", "")),
                                        os.environ.get("VMUC_KERNEL_CMDLINE", "")]))
        have = run("grep -m1 Features /proc/cpuinfo").split(":", 1)[1].split()
        with open(guest_bin, "rb") as f:
            guest_kernels = pairs(vm("shell", NAME, "--", "cat > /tmp/k && chmod +x /tmp/k && /tmp/k",
                                     data=f.read()))
        drivers = pairs(vm("shell", NAME, "--", "python3 -",
                           data=f"ALGS = {ALGS!r}\n{DRIVERS}".encode()))
        with open(os.path.join(HERE, "afalg.py"), "rb") as f:
            afalg = pairs(vm("shell", NAME, "--", "python3 -", data=f.read()))
        guest_speed = speed(run)
        guest_speed["version"] = run("openssl version").split()[1]
    finally:
        subprocess.run([vmuc, "stop", "--force", NAME], env=env, capture_output=True)
        subprocess.run(["rm", "-rf", home])

    chip = sh("sysctl", "-n", "machdep.cpu.brand_string").strip()
    commit = sh("git", "-C", ROOT, "rev-parse", "--short", "HEAD").strip()
    names = list(host_kernels)
    section = [f"\n### {datetime.date.today()}, {chip}, macOS {platform.mac_ver()[0]}, "
               f"vmuc {commit}, guest kernel {kernel.strip()}\n",
               "Guest CPU features: " + ", ".join(f + ("" if f in have else " (absent)") for f in FEATURES)
               + ".\n\nKernel's choice: " + ", ".join(f"{a} → {drivers.get(a, '-')}" for a in ALGS) + ".\n",
               "Instructions, Gop/s (same source on both; `dit` is 1 when PSTATE.DIT could be set):\n",
               table(["", *names], [["host", *host_kernels.values()], ["guest", *guest_kernels.values()]]),
               "Guest kernel drivers through AF_ALG, GB/s:\n",
               table(list(afalg), [list(afalg.values())]),
               "OpenSSL `speed`, 16 KiB, GB/s:\n",
               table(["", "version", *SPEED],
                     ([["host", host_speed["version"], *(host_speed[a] for a in SPEED)],
                       [f"host, `OPENSSL_armcap={LINUX_ARMCAP}`", host_linux["version"],
                        *(host_linux[a] for a in SPEED)]] if host_speed else [])
                     + [["guest", guest_speed["version"], *(guest_speed[a] for a in SPEED)]])]
    with open(os.path.join(ROOT, "docs/benchmarks/cpu-crypto.md"), "a") as f:
        f.write("\n".join(section))


if __name__ == "__main__":
    main()
