// SPDX-License-Identifier: Apache-2.0
//! Changes the host makes in a share, for the guest's file watchers.
//!
//! virtiofs cannot tell the guest that a file changed under it, so its
//! inotify watchers (dev servers, test runners, build watchers) never
//! hear of an edit made on the Mac. A [`Watcher`] (FSEvents) reports the
//! host's changes to the shared directories, the VMM's own (the guest's
//! writes) excluded, and [`classify`] turns each batch into the events a
//! local change would have raised, for the guest agent to replay
//! (docs/experiments/2026-10-13-e4-inotify-propagation.md). Only changes
//! in directories the guest has looked up are kept: it cannot have
//! cached or be watching anything else.

use std::collections::HashSet;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::passthrough::Seen;

#[cfg(target_os = "macos")]
mod fsevents;
#[cfg(target_os = "macos")]
pub use fsevents::Watcher;

/// FSEvents' per-event flags (`kFSEventStreamEventFlag*`), which
/// accumulate while the stream coalesces.
pub mod flag {
    pub const MUST_SCAN_SUBDIRS: u32 = 0x1;
    pub const CREATED: u32 = 0x100;
    pub const REMOVED: u32 = 0x200;
    pub const INODE_META_MOD: u32 = 0x400;
    pub const RENAMED: u32 = 0x800;
    pub const MODIFIED: u32 = 0x1000;
    pub const FINDER_INFO_MOD: u32 = 0x2000;
    pub const CHANGE_OWNER: u32 = 0x4000;
    pub const XATTR_MOD: u32 = 0x8000;
    /// Metadata only: an inotify ATTRIB.
    pub const META: u32 = INODE_META_MOD | FINDER_INFO_MOD | CHANGE_OWNER | XATTR_MOD;
}

/// One FSEvents event: an absolute host path and its flags.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostEvent {
    pub path: PathBuf,
    pub flags: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChangeKind {
    Created,
    Removed,
    Modified,
    Attrib,
    MovedFrom,
    MovedTo,
}

/// A change as the guest should see it; `path` is relative to the share.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Change {
    pub kind: ChangeKind,
    pub path: String,
}

/// The changes in a batch of host events that the guest can see, in
/// order. What is there now decides the kind, since the flags pile up:
/// a path that is gone was removed, one that exists and is new to the
/// guest was created (and, with data, written), and a rename's two names
/// (FSEvents reports the old one, then the new one) become a move.
pub fn classify(root: &Path, seen: &Seen, batch: &[HostEvent]) -> Vec<Change> {
    let mut out = Vec::new();
    let mut dup = HashSet::new();
    // What this batch creates: the guest knows it by the time the later
    // events are replayed (a directory's contents, a file's next write).
    let mut created: HashSet<&Path> = HashSet::new();
    for e in batch {
        let Some((rel, parent)) = e
            .path
            .strip_prefix(root)
            .ok()
            .and_then(|rel| Some((rel, rel.parent()?)))
        else {
            continue;
        };
        if !created.contains(parent) && !key(&root.join(parent)).is_some_and(|k| seen.knows(k)) {
            continue;
        }
        let now = std::fs::symlink_metadata(&e.path).ok();
        let renamed = e.flags & flag::RENAMED != 0;
        let kind = match &now {
            None if renamed => ChangeKind::MovedFrom,
            None => ChangeKind::Removed,
            Some(_) if renamed => ChangeKind::MovedTo,
            Some(m)
                if e.flags & flag::CREATED != 0
                    && !created.contains(rel)
                    && !seen.knows((m.dev(), m.ino())) =>
            {
                ChangeKind::Created
            }
            Some(m) if e.flags & flag::MODIFIED != 0 && !m.is_dir() => ChangeKind::Modified,
            Some(_) if e.flags & flag::META != 0 => ChangeKind::Attrib,
            Some(_) => continue,
        };
        if matches!(kind, ChangeKind::Created | ChangeKind::MovedTo) {
            created.insert(rel);
        }
        let Some(path) = rel.to_str() else {
            continue;
        };
        // A file created with data was also written, as a local one raises
        // MODIFY after CREATE; FSEvents may fold later writes in as well.
        let written = kind == ChangeKind::Created
            && e.flags & flag::MODIFIED != 0
            && now.as_ref().is_some_and(|m| !m.is_dir());
        let also = written.then_some(ChangeKind::Modified);
        for kind in std::iter::once(kind).chain(also) {
            let change = Change {
                kind,
                path: path.to_owned(),
            };
            if dup.insert(change.clone()) {
                out.push(change);
            }
        }
    }
    pair_moves(&mut out);
    out
}

/// A MovedFrom right before a MovedTo is one rename; any other is the
/// removal or creation it looks like from the guest.
fn pair_moves(changes: &mut [Change]) {
    let mut i = 0;
    while i < changes.len() {
        if changes[i].kind == ChangeKind::MovedFrom
            && changes
                .get(i + 1)
                .is_some_and(|c| c.kind == ChangeKind::MovedTo)
        {
            i += 2;
            continue;
        }
        changes[i].kind = match changes[i].kind {
            ChangeKind::MovedFrom => ChangeKind::Removed,
            ChangeKind::MovedTo => ChangeKind::Created,
            k => k,
        };
        i += 1;
    }
}

/// The changes FSEvents dropped under `dir` (it said MustScanSubDirs),
/// recovered by checking what the guest knows there against the disk: a
/// known entry that is gone was removed, one replaced by another file was
/// created, a known file modified since `since` was modified, and an
/// entry the guest does not know was created, in a directory the guest
/// has looked inside (one with a known entry: nothing here opens a
/// directory the guest has not, such as a privacy-protected folder whose
/// first read waits for the user). It reads the host filesystem and can
/// block; run it where blocking holds nothing up.
pub fn rescan(root: &Path, seen: &Seen, dir: &Path, since: SystemTime) -> Vec<Change> {
    let rel_dir = match dir.strip_prefix(root) {
        Ok(rel) => rel,
        // The drop was reported for a directory holding the whole share.
        Err(_) if root.starts_with(dir) => Path::new(""),
        Err(_) => return Vec::new(),
    };
    let mut known = seen.under(rel_dir);
    known.sort_by_key(|(rel, _)| rel.components().count());
    let read: HashSet<PathBuf> = known
        .iter()
        .filter_map(|(rel, _)| Some(rel.parent()?.to_path_buf()))
        .collect();
    let mut out = Vec::new();
    let mut dup = HashSet::new();
    let mut push = |kind, rel: &Path| {
        if let Some(path) = rel.to_str().filter(|p| !p.is_empty()) {
            let change = Change {
                kind,
                path: path.to_owned(),
            };
            if dup.insert(change.clone()) {
                out.push(change);
            }
        }
    };
    for (rel, key) in known {
        let Ok(m) = std::fs::symlink_metadata(root.join(&rel)) else {
            push(ChangeKind::Removed, &rel);
            continue;
        };
        if (m.dev(), m.ino()) != key {
            push(ChangeKind::Created, &rel);
        } else if m.is_dir() {
            if !read.contains(&rel) {
                continue;
            }
            let Ok(entries) = std::fs::read_dir(root.join(&rel)) else {
                continue;
            };
            for e in entries.flatten() {
                let new = e.metadata().is_ok_and(|m| !seen.knows((m.dev(), m.ino())));
                if new {
                    push(ChangeKind::Created, &rel.join(e.file_name()));
                }
            }
        } else if m.modified().is_ok_and(|t| t >= since) {
            push(ChangeKind::Modified, &rel);
        }
    }
    out
}

fn key(p: &Path) -> Option<(u64, u64)> {
    std::fs::symlink_metadata(p)
        .ok()
        .map(|m| (m.dev(), m.ino()))
}

/// FSEvents is macOS's; elsewhere nothing is watched.
#[cfg(not(target_os = "macos"))]
#[derive(Debug)]
pub struct Watcher;

#[cfg(not(target_os = "macos"))]
impl Watcher {
    pub fn start(
        _roots: &[PathBuf],
        _latency: std::time::Duration,
        _tx: std::sync::mpsc::Sender<Vec<HostEvent>>,
    ) -> std::io::Result<Self> {
        Err(std::io::ErrorKind::Unsupported.into())
    }
}

#[cfg(test)]
mod tests;
