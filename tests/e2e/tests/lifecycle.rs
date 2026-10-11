// SPDX-License-Identifier: Apache-2.0
//! M4: `vmuc create`, `start`, `shell`, `stop`, `delete` on a real Ubuntu
//! cloud image, through the signed `vmuc` binary, in a throwaway
//! VMUC_HOME (the image download cache is shared, so only the first run
//! downloads). Needs Apple silicon, the kernel artifact and the musl
//! agent (`make agent`); see the crate docs.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The signed debug binary, built if needed.
fn vmuc() -> PathBuf {
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

fn run(bin: &Path, home: &Path, args: &[&str]) -> Output {
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

#[test]
#[ignore = "boots Ubuntu: Apple silicon only"]
fn create_start_shell_stop_delete() {
    let _vm = vmuc_e2e::vm_lock();
    let bin = vmuc();
    // A short home: unix socket paths are limited to 104 bytes.
    let home = tempfile::Builder::new()
        .prefix("vmuc-e2e")
        .tempdir_in("/tmp")
        .unwrap();
    let h = home.path();

    let out = run(
        &bin,
        h,
        &[
            "create", "ubuntu", "--name", "e2e", "--cpus", "2", "--memory", "2GiB",
        ],
    );
    assert!(out.status.success(), "create failed");

    let t = Instant::now();
    let out = run(&bin, h, &["start", "e2e", "--timeout", "180"]);
    assert!(out.status.success(), "start failed");
    eprintln!("start took {:?}", t.elapsed());

    let out = run(&bin, h, &["list"]);
    assert!(String::from_utf8_lossy(&out.stdout).contains("Running"));

    let out = run(
        &bin,
        h,
        &[
            "shell",
            "e2e",
            "--",
            // On an instance's first boot, Ubuntu's snap seeding holds
            // cloud-init's final stage (so boot.sh) ~30 s without a network.
            "uname -r; id -u; for i in $(seq 120); do test -e /run/vmuc-boot-done && break; \
             sleep 1; done; cat /run/vmuc-boot-done",
        ],
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "shell failed");
    assert!(stdout.contains("-vmuc"), "{stdout}");

    let boot_id = std::fs::read_to_string(h.join("e2e/events.jsonl")).unwrap();
    let last = stdout.lines().last().unwrap_or_default().trim().to_owned();
    assert!(
        boot_id.contains(&last),
        "boot-done marker {last:?} is not this boot's ID"
    );

    // A guest web server shows up on the host's loopback by itself.
    let port = 38_000 + (std::process::id() % 1000) as u16;
    let serve = format!("nohup python3 -m http.server {port} --directory /etc >/dev/null 2>&1 &");
    assert!(
        run(&bin, h, &["shell", "e2e", "--", &serve])
            .status
            .success()
    );
    let t = Instant::now();
    let body = loop {
        if let Ok(mut s) = std::net::TcpStream::connect(("127.0.0.1", port)) {
            use std::io::{Read, Write};
            s.write_all(b"GET /hostname HTTP/1.0\r\n\r\n").unwrap();
            let mut body = String::new();
            let _ = s.read_to_string(&mut body);
            if body.contains("200") {
                break body;
            }
        }
        assert!(
            t.elapsed() < Duration::from_secs(20),
            "port {port} was never forwarded"
        );
        std::thread::sleep(Duration::from_millis(200));
    };
    assert!(body.contains("vmuc-"), "{body}");
    eprintln!("port {port} forwarded after {:?}", t.elapsed());

    let out = run(&bin, h, &["delete", "e2e"]);
    assert!(
        !out.status.success(),
        "delete must refuse a running instance"
    );

    let t = Instant::now();
    let out = run(&bin, h, &["stop", "e2e"]);
    assert!(out.status.success(), "stop failed");
    assert!(t.elapsed() < Duration::from_secs(90));
    let out = run(&bin, h, &["list"]);
    assert!(String::from_utf8_lossy(&out.stdout).contains("Stopped"));

    let out = run(&bin, h, &["delete", "e2e"]);
    assert!(out.status.success(), "delete failed");
}
