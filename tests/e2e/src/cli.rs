// SPDX-License-Identifier: Apache-2.0
//! Driving the signed `vmuc` binary for tests that boot real instances.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// An inotify watcher on one directory, for `python3 watch.py DIR OUT`
/// in the guest (the cloud image has no inotify-tools): one `MASK NAME`
/// line per event.
pub const WATCHER: &str = r#"
import ctypes, os, struct, sys
libc = ctypes.CDLL(None, use_errno=True)
fd = libc.inotify_init()
libc.inotify_add_watch(fd, sys.argv[1].encode(), 0x2 | 0x40 | 0x80 | 0x100 | 0x200)
out = open(sys.argv[2], "a", buffering=1)
while True:
    buf, i = os.read(fd, 65536), 0
    while i < len(buf):
        _, mask, _, n = struct.unpack_from("iIII", buf, i)
        out.write(f"{mask & 0xfff:#x} {buf[i + 16:i + 16 + n].rstrip(bytes(1)).decode()}\n")
        i += 16 + n
"#;

/// The repository root.
pub fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The signed debug binary, built if needed.
pub fn vmuc() -> PathBuf {
    let root = root();
    let ok = Command::new(env!("CARGO"))
        .args(["build", "-q", "-p", "vmuc-cli"])
        .current_dir(&root)
        .status()
        .unwrap()
        .success();
    assert!(ok, "cargo build -p vmuc-cli failed");
    let bin = root.join("target/debug/vmuc");
    let signed = Command::new("codesign")
        .args(["--force", "--sign", "-", "--entitlements"])
        .arg(root.join("vmuc.entitlements"))
        .arg(&bin)
        .output()
        .unwrap();
    assert!(signed.status.success(), "codesign: {signed:?}");
    bin
}

/// Runs `vmuc args` with `VMUC_HOME=home`, echoing what it printed.
pub fn run(bin: &Path, home: &Path, args: &[&str]) -> Output {
    let out = Command::new(bin)
        .args(args)
        .env("VMUC_HOME", home)
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    eprintln!(
        "$ vmuc {}\n{}{}",
        args.join(" "),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    out
}
