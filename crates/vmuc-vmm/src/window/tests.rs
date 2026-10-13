// SPDX-License-Identifier: Apache-2.0
use std::os::fd::AsRawFd;
use std::sync::{Arc, Mutex};

use super::*;

const MIB: u64 = 1 << 20;

#[derive(Default)]
struct Fake(Arc<Mutex<Vec<String>>>);

impl Stage2Map for Fake {
    unsafe fn map(&self, _host: *mut u8, ipa: u64, len: u64, writable: bool) -> io::Result<()> {
        let w = if writable { "rw" } else { "ro" };
        self.0
            .lock()
            .unwrap()
            .push(format!("map {}+{} {w}", ipa / MIB, len / MIB));
        Ok(())
    }
    fn unmap(&self, ipa: u64, len: u64) -> io::Result<()> {
        self.0
            .lock()
            .unwrap()
            .push(format!("unmap {}+{}", ipa / MIB, len / MIB));
        Ok(())
    }
}

fn window() -> (FileWindow, Arc<Mutex<Vec<String>>>, std::fs::File) {
    let fake = Fake::default();
    let calls = Arc::clone(&fake.0);
    let file = tempfile::tempfile().unwrap();
    file.set_len(16 * MIB).unwrap();
    (
        FileWindow::new(1024 * MIB, 64 * MIB, Box::new(fake)),
        calls,
        file,
    )
}

fn take(calls: &Mutex<Vec<String>>) -> Vec<String> {
    std::mem::take(&mut *calls.lock().unwrap())
}

#[test]
fn a_range_replaces_whatever_overlaps_it() {
    let (w, calls, f) = window();
    w.map(f.as_raw_fd(), 0, 2 * MIB, 2 * MIB, false).unwrap();
    w.map(f.as_raw_fd(), 4 * MIB, 2 * MIB, 2 * MIB, true)
        .unwrap();
    assert_eq!(
        take(&calls),
        ["map 1026+2 ro", "unmap 1026+2", "map 1026+2 rw"]
    );
    w.unmap(10 * MIB, 2 * MIB).unwrap();
    assert!(take(&calls).is_empty(), "nothing mapped there");
    drop(w);
    assert_eq!(take(&calls), ["unmap 1026+2"]);
}

#[test]
fn cutting_the_middle_keeps_both_ends_mapped() {
    let (w, calls, f) = window();
    w.map(f.as_raw_fd(), 0, 0, 8 * MIB, true).unwrap();
    w.unmap(2 * MIB, 4 * MIB).unwrap();
    assert_eq!(
        take(&calls),
        [
            "map 1024+8 rw",
            "unmap 1024+8",
            "map 1024+2 rw",
            "map 1030+2 rw"
        ]
    );
    drop(w);
    assert_eq!(take(&calls), ["unmap 1024+2", "unmap 1030+2"]);
}
