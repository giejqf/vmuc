// SPDX-License-Identifier: Apache-2.0

use std::fs;

use super::*;
use crate::passthrough::Passthrough;

struct Share {
    _dir: tempfile::TempDir,
    fs: Passthrough,
}

impl Share {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let fs = Passthrough::new(dir.path(), false).unwrap();
        Self { _dir: dir, fs }
    }

    fn root(&self) -> &std::path::Path {
        self.fs.root()
    }
}

#[cfg(target_os = "macos")]
#[test]
fn fsevents_reports_other_processes_and_not_this_one() {
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    let s = Share::new();
    let (tx, rx) = mpsc::channel();
    let _w = Watcher::start(&[s.root().to_path_buf()], Duration::from_millis(20), tx).unwrap();
    std::thread::sleep(Duration::from_millis(200));
    fs::write(s.root().join("mine"), "m").unwrap();
    let theirs = s.root().join("theirs");
    std::process::Command::new("touch")
        .arg(&theirs)
        .status()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut paths = Vec::new();
    while !paths.contains(&theirs) && Instant::now() < deadline {
        if let Ok(batch) = rx.recv_timeout(Duration::from_millis(100)) {
            paths.extend(batch.into_iter().map(|e: HostEvent| e.path));
        }
    }
    assert!(paths.contains(&theirs), "{paths:?}");
    assert!(!paths.contains(&s.root().join("mine")), "{paths:?}");
}

/// E4's probe for FSEvents' own delay: from a writer process exiting to
/// the batch arriving, 40 lone changes 0.3 s apart.
/// `cargo test -p vmuc-fs --release -- --ignored fsevents_delivery --nocapture`
#[cfg(target_os = "macos")]
#[test]
#[ignore = "takes 12 s; prints a measurement"]
fn fsevents_delivery_latency() {
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    let s = Share::new();
    let (tx, rx) = mpsc::channel();
    let _w = Watcher::start(&[s.root().to_path_buf()], Duration::from_millis(50), tx).unwrap();
    std::thread::sleep(Duration::from_millis(300));
    let mut ms = Vec::new();
    for i in 0..40 {
        let p = s.root().join(format!("f{i}"));
        std::process::Command::new("touch")
            .arg(&p)
            .status()
            .unwrap();
        let t0 = Instant::now();
        while !rx
            .recv_timeout(Duration::from_secs(2))
            .unwrap()
            .iter()
            .any(|e| e.path == p)
        {}
        ms.push(t0.elapsed().as_secs_f64() * 1000.0);
        std::thread::sleep(Duration::from_millis(300));
    }
    ms.sort_by(f64::total_cmp);
    println!(
        "FSEvents delivery: p50 {:.1} ms, p95 {:.1} ms",
        ms[20], ms[38]
    );
}
