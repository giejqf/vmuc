# SPDX-License-Identifier: Apache-2.0
"""smstop: what an SMSTOP that has nothing to stop costs, and what the
guest's syscall-heavy work costs while its kernel issues one on every
syscall entry and kernel_neon_begin(). Builds smstop.rs for the host and
for the guest and runs it where the CPU reports SME; in an Ubuntu
instance (4 vCPUs, 4 GiB, a fresh VMUC_HOME) runs work.sh. VMUC_KERNEL
and VMUC_KERNEL_CMDLINE pass through to vmuc, so the same run compares a
patched kernel or `arm64.nosme`. Appends a dated section to
docs/benchmarks/smstop.md.

    python3 -I tests/bench/smstop/run.py
"""

import datetime
import os
import platform
import subprocess
import sys
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(os.path.dirname(os.path.dirname(HERE)))
NAME = "smstop"


def sh(*args, **kw):
    return subprocess.run(args, capture_output=True, text=True, check=True, **kw).stdout


def build(target):
    out = os.path.join(tempfile.gettempdir(), f"vmuc-smstop-{target}")
    extra = ["-C", "linker=rust-lld", "-C", "target-feature=+crt-static",
             "-C", "link-self-contained=yes"] if "linux" in target else []
    sh("rustc", "--edition", "2024", "-O", "-C", "target-cpu=generic", "--target", target,
       *extra, os.path.join(HERE, "smstop.rs"), "-o", out, cwd=ROOT)
    return out


def table(text, label):
    """smstop.rs output as markdown rows, `label` in the first column."""
    head, *rows = [line.split() for line in text.splitlines() if line.strip()]
    lines = ["| | " + " | ".join(head) + " |", "|" + "---|" * (len(head) + 1)]
    lines += [f"| {label} | " + " | ".join(r) + " |" for r in rows]
    return "\n".join(lines) + "\n"


def main():
    host = None
    if sh("sysctl", "-n", "hw.optional.arm.FEAT_SME").strip() == "1":
        host = sh(build("aarch64-apple-darwin"))
    guest_bin = build("aarch64-unknown-linux-musl")

    vmuc = os.environ.get("VMUC")
    if not vmuc:
        subprocess.run(["cargo", "build", "-q", "--release", "-p", "vmuc-cli"], cwd=ROOT, check=True)
        vmuc = os.path.join(ROOT, "target/release/vmuc")
        sh("codesign", "--force", "--sign", "-", "--entitlements",
           os.path.join(ROOT, "vmuc.entitlements"), vmuc)
    home = tempfile.mkdtemp(prefix="vmuc-smstop.", dir="/tmp")
    env = {**os.environ, "VMUC_HOME": home}

    def vm(*args, data=b""):
        p = subprocess.run([vmuc, *args], env=env, capture_output=True, input=data)
        if p.returncode != 0:
            sys.exit(f"vmuc {' '.join(args)} failed:\n{p.stdout.decode()}{p.stderr.decode()}")
        return p.stdout.decode()

    try:
        vm("create", "ubuntu", "--name", NAME, "--cpus", "4", "--memory", "4GiB")
        vm("start", NAME)
        vm("shell", NAME, "--", "cloud-init status --wait >/dev/null; true")
        kernel = " ".join(filter(None, [vm("shell", NAME, "--", "uname -r").strip(),
                                        os.path.basename(os.environ.get("VMUC_KERNEL", "")),
                                        os.environ.get("VMUC_KERNEL_CMDLINE", "")]))
        sme = "sme" in vm("shell", NAME, "--", "grep -m1 Features /proc/cpuinfo").split()
        guest = abi = None
        if sme:
            with open(guest_bin, "rb") as f:
                guest = vm("shell", NAME, "--", "cat > /tmp/s && chmod +x /tmp/s && /tmp/s",
                           data=f.read())
            abi = vm("shell", NAME, "--", "/tmp/s --abi").split()
        with open(os.path.join(HERE, "work.sh"), "rb") as f:
            work = [line.split() for line in vm("shell", NAME, "--", "bash -s",
                                                data=f.read()).splitlines()]
    finally:
        subprocess.run([vmuc, "stop", "--force", NAME], env=env, capture_output=True)
        subprocess.run(["rm", "-rf", home])

    chip = sh("sysctl", "-n", "machdep.cpu.brand_string").strip()
    commit = sh("git", "-C", ROOT, "rev-parse", "--short", "HEAD").strip()
    section = [f"\n### {datetime.date.today()}, {chip}, macOS {platform.mac_ver()[0]}, "
               f"vmuc {commit}, guest kernel {kernel}, guest SME {'on' if sme else 'off'}\n",
               "Guest work, ms (best of five):\n",
               "| " + " | ".join(w[0] for w in work) + " |\n|" + "---|" * len(work) + "\n| "
               + " | ".join(f"{float(w[1]) * 1000:.1f}" for w in work) + " |\n"]
    if abi:
        section.append(f"SVCR after a syscall made in streaming mode: {abi[2]} (SM must be "
                       f"cleared); with ZA on: {abi[4]} (ZA kept, 2).\n")
    if host or guest:
        section.append("A block of scalar work after each mode, ns (best of seven):\n")
        section.append((table(host, "host") if host else "")
                       + ("\n" if host and guest else "")
                       + (table(guest, "guest") if guest else ""))
    with open(os.path.join(ROOT, "docs/benchmarks/smstop.md"), "a") as f:
        f.write("\n".join(section))


if __name__ == "__main__":
    main()
