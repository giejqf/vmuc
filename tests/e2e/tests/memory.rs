// SPDX-License-Identifier: Apache-2.0
//! M5: memory a guest frees goes back to macOS. An Ubuntu instance fills
//! 1 GiB of tmpfs, deletes it, and the VMM's phys_footprint must drop by
//! most of it within 15 s (the bench measures the 10 s target; the test
//! leaves headroom for a loaded machine). Needs Apple silicon, the kernel
//! artifact and the musl agent; see the crate docs.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

fn vmuc() -> PathBuf {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
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

fn ok(bin: &Path, home: &Path, args: &[&str]) -> String {
    let out = Command::new(bin)
        .args(args)
        .env("VMUC_HOME", home)
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "vmuc {}: {}",
        args.join(" "),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn footprint(bin: &Path, home: &Path) -> u64 {
    let json = ok(bin, home, &["stat", "mem", "--json"]);
    let key = "\"host_footprint\":";
    let at = json.find(key).expect("host_footprint in stat") + key.len();
    json[at..]
        .split(|c: char| !c.is_ascii_digit())
        .next()
        .unwrap()
        .parse()
        .unwrap()
}

#[test]
#[ignore = "boots Ubuntu: Apple silicon only"]
fn freed_guest_memory_returns_to_the_host() {
    let _vm = vmuc_e2e::vm_lock();
    let bin = vmuc();
    let home = tempfile::Builder::new()
        .prefix("vmuc-e2e")
        .tempdir_in("/tmp")
        .unwrap();
    let h = home.path();
    ok(
        &bin,
        h,
        &[
            "create", "ubuntu", "--name", "mem", "--cpus", "2", "--memory", "3GiB",
        ],
    );
    ok(&bin, h, &["start", "mem", "--timeout", "180"]);

    ok(
        &bin,
        h,
        &[
            "shell",
            "mem",
            "--",
            "sudo mkdir -p /mnt/t && sudo mount -t tmpfs -o size=2G tmpfs /mnt/t \
         && sudo dd if=/dev/zero of=/mnt/t/f bs=1M count=1024 status=none",
        ],
    );
    std::thread::sleep(Duration::from_secs(1));
    let filled = footprint(&bin, h);
    ok(&bin, h, &["shell", "mem", "--", "sudo rm /mnt/t/f"]);
    let t = Instant::now();
    let goal = filled - (1u64 << 30) * 8 / 10;
    let mut now = filled;
    while t.elapsed() < Duration::from_secs(15) && now > goal {
        std::thread::sleep(Duration::from_millis(500));
        now = footprint(&bin, h);
    }
    eprintln!(
        "footprint {} MiB -> {} MiB in {:?}",
        filled >> 20,
        now >> 20,
        t.elapsed()
    );
    ok(&bin, h, &["stop", "mem", "--force"]);
    assert!(
        now <= goal,
        "less than 80 % of the freed 1 GiB came back within 15 s"
    );
}
