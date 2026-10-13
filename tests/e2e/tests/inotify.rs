// SPDX-License-Identifier: Apache-2.0
//! E4: changes made on the host in a share reach inotify watchers in the
//! guest as the events a local change raises, and the guest's own changes
//! are not echoed back. Needs Apple silicon, the kernel artifact (with
//! the replay patch) and the musl agent.

use std::time::{Duration, Instant};

use vmuc_e2e::cli::{root, run, vmuc};

/// A watcher on one directory (the cloud image has no inotify-tools):
/// one `MASK NAME` line per event.
const WATCHER: &str = r#"
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

#[test]
#[ignore = "boots Ubuntu: Apple silicon only"]
fn host_changes_reach_guest_watchers_once() {
    let _vm = vmuc_e2e::vm_lock();
    let bin = &vmuc();
    let home = tempfile::Builder::new()
        .prefix("vmuc-e2e")
        .tempdir_in("/tmp")
        .unwrap();
    let h = home.path();
    let share = tempfile::tempdir_in(root().join("target")).unwrap();
    let s = share.path().canonicalize().unwrap();
    let w = s.join("w");
    std::fs::create_dir(&w).unwrap();
    let mounts = format!("mounts=[{{location: {}, writable: true}}]", s.display());
    let out = run(
        bin,
        h,
        &[
            "create", "ubuntu", "--name", "notify", "--cpus", "2", "--memory", "2GiB", "--set",
            &mounts,
        ],
    );
    assert!(out.status.success(), "create failed");
    assert!(
        run(bin, h, &["start", "notify", "--timeout", "180"])
            .status
            .success()
    );

    let script = format!(
        "for i in $(seq 120); do test -e /run/vmuc-boot-done && break; sleep 1; done; \
         cat > /tmp/watch.py <<'EOF'\n{WATCHER}\nEOF\n\
         nohup python3 /tmp/watch.py {w} /tmp/events >/dev/null 2>&1 & sleep 1; \
         echo hi > {w}/from-guest",
        w = w.display()
    );
    assert!(
        run(bin, h, &["shell", "notify", "--", &script])
            .status
            .success()
    );

    std::fs::write(w.join("host.txt"), "a\n").unwrap();
    std::thread::sleep(Duration::from_millis(300));
    std::fs::write(w.join("host.txt"), "a\nb\n").unwrap();
    std::thread::sleep(Duration::from_millis(300));
    std::fs::rename(w.join("host.txt"), w.join("moved.txt")).unwrap();
    std::thread::sleep(Duration::from_millis(300));
    std::fs::remove_file(w.join("moved.txt")).unwrap();

    let want = [
        "0x100 host.txt",
        "0x2 host.txt",
        "0x40 host.txt",
        "0x80 moved.txt",
        "0x200 moved.txt",
    ];
    let deadline = Instant::now() + Duration::from_secs(10);
    let events = loop {
        let out = run(bin, h, &["shell", "notify", "--", "cat /tmp/events"]);
        let events: Vec<String> = String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::to_owned)
            .collect();
        if events.iter().any(|e| e == want[4]) || Instant::now() > deadline {
            break events;
        }
        std::thread::sleep(Duration::from_millis(200));
    };
    let mut it = events.iter();
    for e in want {
        assert!(
            it.any(|x| x == e),
            "{e} missing or out of order: {events:?}"
        );
    }
    for e in ["0x100 from-guest", "0x2 from-guest"] {
        let n = events.iter().filter(|x| *x == e).count();
        assert_eq!(n, 1, "the guest's own change came back: {events:?}");
    }

    assert!(
        run(bin, h, &["stop", "notify"]).status.success(),
        "stop failed"
    );
    assert!(
        run(bin, h, &["delete", "notify"]).status.success(),
        "delete failed"
    );
}
