// SPDX-License-Identifier: Apache-2.0
//! ADR 0007: a suspended Ubuntu instance resumes with its programs, its
//! open files on a share and its network as they were, its clock right,
//! and the host's changes made meanwhile replayed to its file watchers;
//! a discarded state boots afresh. Needs Apple silicon, the kernel
//! artifact and the musl agent.

use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use vmuc_e2e::cli::{Cleanup, WATCHER, root, run, vmuc};

fn shell(bin: &Path, home: &Path, script: &str) -> String {
    let out = run(bin, home, &["shell", "susp", "--", script]);
    assert!(out.status.success(), "shell failed: {script}");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// `NAME VALUE` lines as printed by the probe below.
fn field(text: &str, name: &str) -> String {
    text.lines()
        .find_map(|l| l.strip_prefix(name)?.strip_prefix(' '))
        .unwrap_or_else(|| panic!("no {name} in {text:?}"))
        .to_owned()
}

const PROBE: &str = "echo boot_id $(cat /proc/sys/kernel/random/boot_id); \
                     echo count $(cat /dev/shm/count); echo clock $(date +%s.%N)";

#[test]
#[ignore = "boots Ubuntu: Apple silicon only"]
fn a_suspended_instance_carries_on_where_it_was() {
    let _vm = vmuc_e2e::vm_lock();
    let bin = &vmuc();
    let home = tempfile::Builder::new()
        .prefix("vmuc-e2e")
        .tempdir_in("/tmp")
        .unwrap();
    let h = home.path();
    let _cleanup = Cleanup::new(bin, h, "susp");
    let share = tempfile::tempdir_in(root().join("target")).unwrap();
    let w = share.path().canonicalize().unwrap().join("w");
    std::fs::create_dir(&w).unwrap();
    let mounts = format!(
        "mounts=[{{location: {}, writable: true}}]",
        w.parent().unwrap().display()
    );
    let create = [
        "create", "ubuntu", "--name", "susp", "--cpus", "2", "--memory", "2GiB", "--set", &mounts,
    ];
    assert!(run(bin, h, &create).status.success(), "create failed");
    assert!(
        run(bin, h, &["start", "susp", "--timeout", "180"])
            .status
            .success()
    );
    let setup = format!(
        "for i in $(seq 120); do test -e /run/vmuc-boot-done && break; sleep 1; done; \
         cat > /tmp/watch.py <<'EOF'\n{WATCHER}\nEOF\n\
         nohup python3 /tmp/watch.py {w} /tmp/events >/dev/null 2>&1 & \
         nohup sh -c 'i=0; while true; do i=$((i+1)); echo $i > /dev/shm/count; sleep 0.2; done' \
           >/dev/null 2>&1 & \
         nohup sh -c 'exec 3>>{w}/log; while true; do echo x >&3; sleep 0.2; done' \
           >/dev/null 2>&1 & \
         sleep 1; {PROBE}",
        w = w.display()
    );
    let before = shell(bin, h, &setup);

    assert!(run(bin, h, &["suspend", "susp"]).status.success());
    let list = String::from_utf8_lossy(&run(bin, h, &["list"]).stdout).into_owned();
    assert!(list.contains("Suspended"), "{list}");
    assert!(
        !run(bin, h, &["shell", "susp", "--", "true"])
            .status
            .success()
    );
    assert!(
        !run(bin, h, &["stop", "susp"]).status.success(),
        "a plain stop keeps the state"
    );
    let log_len = std::fs::metadata(w.join("log")).unwrap().len();
    std::fs::write(w.join("away.txt"), "written while suspended\n").unwrap();
    std::thread::sleep(Duration::from_secs(3));

    assert!(
        run(bin, h, &["start", "susp", "--timeout", "60"])
            .status
            .success()
    );
    let host_clock = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs_f64();
    let after = shell(bin, h, PROBE);
    assert_eq!(field(&after, "boot_id"), field(&before, "boot_id"));
    let (n0, n1): (u64, u64) = (
        field(&before, "count").parse().unwrap(),
        field(&after, "count").parse().unwrap(),
    );
    assert!(n1 > n0, "the counter carried on: {n0} → {n1}");
    let guest_clock: f64 = field(&after, "clock").parse().unwrap();
    assert!(
        (guest_clock - host_clock).abs() < 1.0,
        "guest clock {guest_clock}, host {host_clock}"
    );
    std::thread::sleep(Duration::from_secs(1));
    assert!(
        std::fs::metadata(w.join("log")).unwrap().len() > log_len,
        "the writer's open file on the share works again"
    );
    let events = shell(
        bin,
        h,
        "for i in $(seq 50); do grep -q away.txt /tmp/events && break; sleep 0.1; done; \
         cat /tmp/events",
    );
    assert!(
        events.lines().any(|e| e == "0x100 away.txt"),
        "the host's change while away: {events:?}"
    );
    let net = shell(bin, h, "ip -4 -o addr show eth0 | wc -l");
    assert_eq!(net.trim(), "1", "eth0 kept its address");

    // Again, from a resumed guest whose RAM is still the image's.
    assert!(run(bin, h, &["suspend", "susp"]).status.success());
    assert!(run(bin, h, &["start", "susp"]).status.success());
    let again = shell(bin, h, PROBE);
    assert_eq!(field(&again, "boot_id"), field(&before, "boot_id"));
    assert!(field(&again, "count").parse::<u64>().unwrap() > n1);
    discarding_boots_afresh(bin, h, &field(&before, "boot_id"));
    assert!(run(bin, h, &["stop", "susp"]).status.success());
    assert!(run(bin, h, &["delete", "susp"]).status.success());
}

/// Suspended again and discarded: the next start boots.
fn discarding_boots_afresh(bin: &Path, h: &Path, boot_id: &str) {
    assert!(run(bin, h, &["suspend", "susp"]).status.success());
    assert!(run(bin, h, &["stop", "--force", "susp"]).status.success());
    let list = String::from_utf8_lossy(&run(bin, h, &["list"]).stdout).into_owned();
    assert!(list.contains("Stopped"), "{list}");
    assert!(
        run(bin, h, &["start", "susp", "--timeout", "180"])
            .status
            .success()
    );
    let booted = shell(
        bin,
        h,
        "echo boot_id $(cat /proc/sys/kernel/random/boot_id)",
    );
    assert_ne!(field(&booted, "boot_id"), boot_id);
}
