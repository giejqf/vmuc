// SPDX-License-Identifier: Apache-2.0

use std::fs;

use super::*;
use crate::abi::ROOT_ID;
use crate::host::linux;

#[test]
fn nodes_and_handles_come_back_in_a_new_server() {
    let dir = tempfile::tempdir().unwrap();
    let sub = dir.path().join("d");
    fs::create_dir(&sub).unwrap();
    for name in ["keep", "moved", "gone", "replaced"] {
        fs::write(sub.join(name), name).unwrap();
    }
    let old = Passthrough::new(dir.path(), false).unwrap();
    let (dnode, _) = old.lookup_rel("d".into()).unwrap();
    let open = |name: &str, flags| {
        let (node, _) = old.lookup_rel(Path::new("d").join(name)).unwrap();
        (node, old.open(node, flags).unwrap())
    };
    let (keep, keep_fh) = open("keep", libc::O_RDWR | libc::O_TRUNC);
    let (_, moved_fh) = open("moved", libc::O_RDONLY);
    let (_, gone_fh) = open("gone", libc::O_RDONLY);
    let (_, replaced_fh) = open("replaced", libc::O_RDONLY);
    let dir_fh = old.opendir(dnode).unwrap();
    old.write(keep_fh, 0, b"kept").unwrap();
    // Renamed on the host while open: the handle follows its file.
    fs::rename(sub.join("moved"), sub.join("moved2")).unwrap();
    let st = old.save_state();
    // And while suspended: one file deleted, one replaced.
    fs::remove_file(sub.join("gone")).unwrap();
    fs::remove_file(sub.join("replaced")).unwrap();
    fs::write(sub.join("replaced"), "another file").unwrap();

    let new = Passthrough::new(dir.path(), false).unwrap();
    let restored = new.restore_state(st);
    assert_eq!(
        restored,
        Restored {
            nodes: 6,
            handles: 3,
            lost: 2
        }
    );
    assert_eq!(new.rel(keep).unwrap(), Path::new("d/keep"));
    assert_eq!(new.rel(ROOT_ID).unwrap(), Path::new(""));
    assert_eq!(
        new.read(keep_fh, 0, 16).unwrap(),
        b"kept",
        "not truncated again"
    );
    new.write(keep_fh, 4, b"!").unwrap();
    assert_eq!(fs::read(sub.join("keep")).unwrap(), b"kept!");
    assert_eq!(new.read(moved_fh, 0, 16).unwrap(), b"moved");
    assert_eq!(new.read(gone_fh, 0, 1), Err(linux::EBADF));
    assert_eq!(new.read(replaced_fh, 0, 1), Err(linux::EBADF));
    assert!(matches!(&*new.handle(dir_fh).unwrap(), Handle::Dir(entries) if entries.len() == 6));
    let (again, _) = new.lookup_rel("d/keep".into()).unwrap();
    assert_eq!(again, keep, "a lookup finds the restored node");
    assert!(
        new.open(keep, libc::O_RDONLY).unwrap() > dir_fh,
        "handles carry on"
    );
}

#[test]
fn a_saved_path_cannot_leave_the_share() {
    let dir = tempfile::tempdir().unwrap();
    let share = dir.path().join("share");
    fs::create_dir(&share).unwrap();
    fs::write(dir.path().join("outside"), "secret").unwrap();
    let p = Passthrough::new(&share, false).unwrap();
    let st = ShareState {
        nodes: vec![NodeState {
            id: 7,
            rel: b"../outside".to_vec(),
            key: (0, 0),
            lookups: 1,
        }],
        next_node: 8,
        handles: vec![HandleState {
            fh: 3,
            open: Open::File {
                rel: b"../outside".to_vec(),
                flags: libc::O_RDONLY,
                ino: 0,
            },
        }],
        next_fh: 4,
    };
    let r = p.restore_state(st);
    assert_eq!(r.lost, 1);
    assert_eq!(p.rel(7), Err(linux::ESTALE));
    assert_eq!(p.read(3, 0, 6), Err(linux::EBADF));
}
