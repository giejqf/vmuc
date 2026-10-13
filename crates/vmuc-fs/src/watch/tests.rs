// SPDX-License-Identifier: Apache-2.0

use std::fs;

use super::*;
use crate::abi::ROOT_ID;
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

    fn root(&self) -> &Path {
        self.fs.root()
    }

    /// The guest looks `rel` up, as it does before caching or watching it.
    fn look_up(&self, rel: &str) {
        let mut node = ROOT_ID;
        for name in rel.split('/') {
            node = self
                .fs
                .lookup_rel(self.fs.child(node, name.as_bytes()).unwrap())
                .unwrap()
                .0;
        }
    }

    fn ev(&self, rel: &str, flags: u32) -> HostEvent {
        HostEvent {
            path: self.root().join(rel),
            flags,
        }
    }

    fn classify(&self, batch: &[HostEvent]) -> Vec<Change> {
        classify(self.root(), &self.fs.seen(), batch)
    }
}

fn ch(kind: ChangeKind, path: &str) -> Change {
    Change {
        kind,
        path: path.into(),
    }
}

#[test]
fn what_is_there_now_decides_the_kind() {
    let s = Share::new();
    fs::write(s.root().join("new.txt"), "n").unwrap();
    fs::write(s.root().join("known.txt"), "k").unwrap();
    s.look_up("known.txt");
    let batch = [
        s.ev("new.txt", flag::CREATED | flag::MODIFIED),
        s.ev("known.txt", flag::MODIFIED | flag::INODE_META_MOD),
        s.ev("gone.txt", flag::CREATED | flag::MODIFIED | flag::REMOVED),
    ];
    assert_eq!(
        s.classify(&batch),
        [
            ch(ChangeKind::Created, "new.txt"),
            ch(ChangeKind::Modified, "known.txt"),
            ch(ChangeKind::Removed, "gone.txt"),
        ]
    );
}

#[test]
fn a_known_file_is_not_created_again_and_metadata_is_attrib() {
    let s = Share::new();
    fs::write(s.root().join("a"), "a").unwrap();
    s.look_up("a");
    assert_eq!(s.classify(&[s.ev("a", flag::CREATED)]), []);
    assert_eq!(
        s.classify(&[s.ev("a", flag::CREATED | flag::INODE_META_MOD)]),
        [ch(ChangeKind::Attrib, "a")]
    );
}

#[test]
fn only_directories_the_guest_has_seen_count() {
    let s = Share::new();
    fs::create_dir_all(s.root().join("seen/unseen")).unwrap();
    fs::write(s.root().join("seen/unseen/f"), "f").unwrap();
    fs::write(s.root().join("seen/f"), "f").unwrap();
    s.look_up("seen");
    let batch = [
        s.ev("seen/unseen/f", flag::MODIFIED),
        s.ev("seen/f", flag::MODIFIED),
        s.ev("", flag::INODE_META_MOD),
    ];
    assert_eq!(s.classify(&batch), [ch(ChangeKind::Modified, "seen/f")]);
    let outside = HostEvent {
        path: "/elsewhere/f".into(),
        flags: flag::MODIFIED,
    };
    assert_eq!(s.classify(&[outside]), []);
}

#[test]
fn a_directory_created_in_the_batch_takes_its_contents_along() {
    let s = Share::new();
    fs::create_dir(s.root().join("d")).unwrap();
    fs::write(s.root().join("d/f"), "f").unwrap();
    let batch = [
        s.ev("d", flag::CREATED),
        s.ev("d/f", flag::CREATED | flag::MODIFIED),
        s.ev("d/f", flag::MODIFIED),
    ];
    assert_eq!(
        s.classify(&batch),
        [
            ch(ChangeKind::Created, "d"),
            ch(ChangeKind::Created, "d/f"),
            ch(ChangeKind::Modified, "d/f"),
        ]
    );
}

#[test]
fn adjacent_renames_are_a_move_and_lone_ones_are_not() {
    let s = Share::new();
    fs::write(s.root().join("to"), "t").unwrap();
    fs::write(s.root().join("in"), "i").unwrap();
    let batch = [
        s.ev("from", flag::RENAMED),
        s.ev("to", flag::RENAMED),
        s.ev("away", flag::RENAMED),
        s.ev("x", flag::REMOVED),
        s.ev("in", flag::RENAMED),
    ];
    assert_eq!(
        s.classify(&batch),
        [
            ch(ChangeKind::MovedFrom, "from"),
            ch(ChangeKind::MovedTo, "to"),
            ch(ChangeKind::Removed, "away"),
            ch(ChangeKind::Removed, "x"),
            ch(ChangeKind::Created, "in"),
        ]
    );
}

#[test]
fn a_rescan_recovers_what_was_dropped() {
    let s = Share::new();
    fs::create_dir(s.root().join("d")).unwrap();
    for f in ["gone", "replaced", "edited", "same"] {
        fs::write(s.root().join("d").join(f), f).unwrap();
        s.look_up(&format!("d/{f}"));
    }
    let since = std::time::SystemTime::now();
    std::thread::sleep(std::time::Duration::from_millis(20));
    fs::remove_file(s.root().join("d/gone")).unwrap();
    fs::write(s.root().join("d/replaced.tmp"), "r2").unwrap();
    fs::rename(s.root().join("d/replaced.tmp"), s.root().join("d/replaced")).unwrap();
    fs::write(s.root().join("d/edited"), "e2").unwrap();
    fs::write(s.root().join("d/new"), "n").unwrap();
    fs::create_dir(s.root().join("unseen")).unwrap();
    fs::write(s.root().join("unseen/f"), "f").unwrap();
    fs::create_dir(s.root().join("unread")).unwrap();
    s.look_up("unread");
    fs::write(s.root().join("unread/f"), "f").unwrap();
    let rescan = |dir: &Path| {
        let mut c = rescan(s.root(), &s.fs.seen(), dir, since);
        c.sort_by(|a, b| a.path.cmp(&b.path));
        c
    };
    assert_eq!(
        rescan(&s.root().join("d")),
        [
            ch(ChangeKind::Modified, "d/edited"),
            ch(ChangeKind::Removed, "d/gone"),
            ch(ChangeKind::Created, "d/new"),
            ch(ChangeKind::Created, "d/replaced"),
        ]
    );
    let whole = rescan(s.root().parent().unwrap());
    assert!(
        whole.contains(&ch(ChangeKind::Created, "unseen")),
        "{whole:?}"
    );
    assert!(!whole.iter().any(|c| c.path == "unseen/f"), "{whole:?}");
    assert!(
        !whole.iter().any(|c| c.path == "unread/f"),
        "listed a directory the guest never looked inside: {whole:?}"
    );
    assert_eq!(rescan(Path::new("/elsewhere")), []);
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
