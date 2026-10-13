// SPDX-License-Identifier: Apache-2.0
//! Driving the signed `vmuc` binary for tests that boot real instances.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

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
