// SPDX-License-Identifier: Apache-2.0
//! E2: a share with virtiofs DAX (`mounts[].dax`) on a real Ubuntu
//! instance: the guest mounts it with `dax=inode`, reads a file through
//! the window without its page cache growing, and sees host edits at once,
//! and the host sees guest writes. Needs Apple silicon, the kernel
//! artifact and the musl agent.

use std::process::Command;

use vmuc_e2e::cli::{Cleanup, root, run, vmuc};

fn sha256(path: &std::path::Path) -> String {
    let out = Command::new("shasum")
        .args(["-a", "256"])
        .arg(path)
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .next()
        .unwrap()
        .to_owned()
}

#[test]
#[ignore = "boots Ubuntu: Apple silicon only"]
fn a_dax_share_maps_files_instead_of_caching_them() {
    let _vm = vmuc_e2e::vm_lock();
    let bin = &vmuc();
    let home = tempfile::Builder::new()
        .prefix("vmuc-e2e")
        .tempdir_in("/tmp")
        .unwrap();
    let h = home.path();
    let _cleanup = Cleanup::new(bin, h, "dax");
    let share = tempfile::tempdir_in(root().join("target")).unwrap();
    // Canonical: the guest's /proc/mounts lists the resolved path.
    let canonical = share.path().canonicalize().unwrap();
    let s = canonical.as_path();
    let data: Vec<u8> = (0..64u32 << 20)
        .map(|i| (i.wrapping_mul(2_654_435_761) >> 24) as u8)
        .collect();
    std::fs::write(s.join("big"), &data).unwrap();
    std::fs::write(s.join("note"), "one\n").unwrap();
    let mounts = format!(
        "mounts=[{{location: {}, writable: true, dax: true}}]",
        s.display()
    );
    let out = run(
        bin,
        h,
        &[
            "create", "ubuntu", "--name", "dax", "--cpus", "2", "--memory", "2GiB", "--set",
            &mounts,
        ],
    );
    assert!(out.status.success(), "create failed");
    assert!(
        run(bin, h, &["start", "dax", "--timeout", "180"])
            .status
            .success()
    );

    let script = format!(
        "for i in $(seq 120); do test -e /run/vmuc-boot-done && break; sleep 1; done; \
         grep ' {s} ' /proc/mounts; c() {{ awk '/^Cached/ {{print $2}}' /proc/meminfo; }}; \
         a=$(c); sha256sum {s}/big; b=$(c); echo cache-growth-kib=$((b - a)); \
         echo guest > {s}/from-guest",
        s = s.display()
    );
    let out = run(bin, h, &["shell", "dax", "--", &script]);
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(out.status.success(), "shell failed");
    assert!(stdout.contains("dax=inode"), "{stdout}");
    assert!(
        stdout.contains(&sha256(&s.join("big"))),
        "content differs: {stdout}"
    );
    let growth: i64 = stdout
        .lines()
        .find_map(|l| l.strip_prefix("cache-growth-kib="))
        .and_then(|v| v.trim().parse().ok())
        .unwrap();
    assert!(
        growth < 16 << 10,
        "the guest cached {growth} KiB of a 64 MiB DAX file"
    );
    assert_eq!(
        std::fs::read_to_string(s.join("from-guest")).unwrap(),
        "guest\n"
    );

    std::fs::write(s.join("note"), "two\n").unwrap();
    let out = run(
        bin,
        h,
        &["shell", "dax", "--", &format!("cat {}/note", s.display())],
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "two",
        "host edit not seen"
    );

    assert!(
        run(bin, h, &["stop", "dax"]).status.success(),
        "stop failed"
    );
    assert!(
        run(bin, h, &["delete", "dax"]).status.success(),
        "delete failed"
    );
}
