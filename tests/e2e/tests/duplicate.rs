// SPDX-License-Identifier: Apache-2.0
//! ADR 0009: a stopped instance duplicated runs beside its source with the
//! source's files and its own hostname, machine ID and MAC address; a
//! running instance is not duplicated. Needs Apple silicon, the kernel
//! artifact and the musl agent.

use std::path::Path;

use vmuc_e2e::cli::{Cleanup, run, vmuc};

fn shell(bin: &Path, home: &Path, name: &str, script: &str) -> String {
    let out = run(bin, home, &["shell", name, "--", script]);
    assert!(out.status.success(), "shell on {name} failed: {script}");
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

const IDENTITY: &str = "echo $(hostname) $(cat /etc/machine-id) \
                        $(cat /sys/class/net/eth0/address 2>/dev/null || echo no-nic) \
                        $(nproc) $(cat ~/marker)";

#[test]
#[ignore = "boots Ubuntu: Apple silicon only"]
fn a_duplicate_runs_beside_its_source_as_another_machine() {
    let _vm = vmuc_e2e::vm_lock();
    let bin = &vmuc();
    let home = tempfile::Builder::new()
        .prefix("vmuc-e2e")
        .tempdir_in("/tmp")
        .unwrap();
    let h = home.path();
    let _orig = Cleanup::new(bin, h, "orig");
    let _copy = Cleanup::new(bin, h, "copy");
    let create = [
        "create", "ubuntu", "--name", "orig", "--cpus", "2", "--memory", "2GiB",
    ];
    assert!(run(bin, h, &create).status.success(), "create failed");
    assert!(
        run(bin, h, &["start", "orig", "--timeout", "180"])
            .status
            .success()
    );
    shell(bin, h, "orig", "echo kept > ~/marker && sync");
    assert!(
        !run(bin, h, &["duplicate", "orig", "copy"]).status.success(),
        "a running instance is not duplicated"
    );
    assert!(run(bin, h, &["stop", "orig"]).status.success());

    let dup = run(bin, h, &["duplicate", "orig", "copy", "--cpus", "3"]);
    assert!(dup.status.success(), "duplicate failed");
    assert!(
        String::from_utf8_lossy(&dup.stdout).contains("copy-on-write"),
        "/tmp is APFS: the disk is a clone"
    );
    for name in ["orig", "copy"] {
        assert!(
            run(bin, h, &["start", name, "--timeout", "180"])
                .status
                .success(),
            "start {name}"
        );
    }
    let orig: Vec<String> = shell(bin, h, "orig", IDENTITY)
        .split(' ')
        .map(str::to_owned)
        .collect();
    let copy: Vec<String> = shell(bin, h, "copy", IDENTITY)
        .split(' ')
        .map(str::to_owned)
        .collect();
    assert_eq!(orig[0], "vmuc-orig");
    assert_eq!(copy[0], "vmuc-copy");
    assert_ne!(copy[1], orig[1], "its own machine ID");
    if orig[2] != "no-nic" {
        assert_ne!(copy[2], orig[2], "its own MAC");
    }
    assert_eq!((orig[3].as_str(), copy[3].as_str()), ("2", "3"));
    assert_eq!(copy[4], "kept", "the source's files");
    assert!(
        !h.join("copy").join("new-machine-id").exists(),
        "the machine ID was set and the request dropped"
    );

    for name in ["orig", "copy"] {
        assert!(run(bin, h, &["stop", name]).status.success());
        assert!(run(bin, h, &["delete", name]).status.success());
    }
}
