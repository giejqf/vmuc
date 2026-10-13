// SPDX-License-Identifier: Apache-2.0
//! A share across a snapshot (ADR 0007): the nodes the guest holds, which
//! it keeps using by ID, and its open handles. A file handle is reopened
//! by the path its file has at the snapshot, with the same access mode,
//! if the file there is still the same one; a directory handle keeps the
//! listing taken when it was opened.

use std::ffi::OsString;
use std::fs::File;
use std::os::fd::AsRawFd;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, PoisonError};

use serde::{Deserialize, Serialize};

use super::{DirEntry, Handle, Passthrough, key};
use crate::host;
use crate::inodes::Node;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShareState {
    pub nodes: Vec<NodeState>,
    pub next_node: u64,
    pub handles: Vec<HandleState>,
    pub next_fh: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeState {
    pub id: u64,
    /// Relative to the share root, as bytes (a name need not be UTF-8).
    pub rel: Vec<u8>,
    pub key: (u64, u64),
    pub lookups: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandleState {
    pub fh: u64,
    pub open: Open,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Open {
    File { rel: Vec<u8>, flags: i32, ino: u64 },
    Dir { entries: Vec<DirEntry> },
}

/// What a restore brought back.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Restored {
    pub nodes: usize,
    pub handles: usize,
    /// File handles whose file is gone or another now.
    pub lost: usize,
}

/// The status flags a reopen keeps; creation flags must not apply twice.
const KEPT_FLAGS: i32 = libc::O_ACCMODE | libc::O_APPEND | libc::O_NONBLOCK;

fn rel_path(bytes: Vec<u8>) -> Option<PathBuf> {
    let p = PathBuf::from(OsString::from_vec(bytes));
    // A saved path names something inside the share, nothing else.
    p.components()
        .all(|c| matches!(c, Component::Normal(_)))
        .then_some(p)
}

impl Passthrough {
    pub fn save_state(&self) -> ShareState {
        let (nodes, next_node) = self.inodes().save();
        let nodes = nodes
            .into_iter()
            .map(|(id, n)| NodeState {
                id,
                rel: n.rel.into_os_string().into_vec(),
                key: n.key,
                lookups: n.lookups,
            })
            .collect();
        let handles = self.handles.read().unwrap_or_else(PoisonError::into_inner);
        let mut saved: Vec<HandleState> = handles
            .iter()
            .filter_map(|(&fh, h)| {
                let open = match &**h {
                    Handle::File(f) => self.reopenable(f)?,
                    Handle::Dir(entries) => Open::Dir {
                        entries: entries.clone(),
                    },
                };
                Some(HandleState { fh, open })
            })
            .collect();
        saved.sort_unstable_by_key(|h| h.fh);
        ShareState {
            nodes,
            next_node,
            handles: saved,
            next_fh: self.next_fh.load(Ordering::Relaxed),
        }
    }

    fn reopenable(&self, f: &File) -> Option<Open> {
        let path = host::fd_path(f.as_raw_fd()).ok()?;
        let rel = path.strip_prefix(&self.root).ok()?;
        // SAFETY: F_GETFL on a descriptor we own.
        let flags = unsafe { libc::fcntl(f.as_raw_fd(), libc::F_GETFL) };
        let ino = f.metadata().ok()?.ino();
        (flags >= 0).then(|| Open::File {
            rel: rel.as_os_str().as_bytes().to_vec(),
            flags: flags & KEPT_FLAGS,
            ino,
        })
    }

    /// Puts saved state back into a share freshly opened on the same root.
    pub fn restore_state(&self, st: ShareState) -> Restored {
        let nodes: Vec<(u64, Node)> = st
            .nodes
            .into_iter()
            .filter_map(|n| {
                let rel = rel_path(n.rel)?;
                // The file now there answers for the node; a node whose
                // file is gone keeps failing as it would have.
                let key = self.stat(&rel).map_or(n.key, |s| key(&s));
                Some((
                    n.id,
                    Node {
                        rel,
                        key,
                        lookups: n.lookups,
                    },
                ))
            })
            .collect();
        let mut restored = Restored {
            nodes: nodes.len(),
            ..Restored::default()
        };
        self.inodes().restore(nodes, st.next_node);
        let mut handles = self.handles.write().unwrap_or_else(PoisonError::into_inner);
        handles.clear();
        for h in st.handles {
            let handle = match h.open {
                Open::File { rel, flags, ino } => {
                    let reopened =
                        rel_path(rel).and_then(|rel| self.reopen(&rel, flags & KEPT_FLAGS, ino));
                    let Some(f) = reopened else {
                        restored.lost += 1;
                        continue;
                    };
                    Handle::File(f)
                }
                Open::Dir { entries } => Handle::Dir(entries),
            };
            handles.insert(h.fh, Arc::new(handle));
            restored.handles += 1;
        }
        self.next_fh.store(st.next_fh.max(1), Ordering::Relaxed);
        restored
    }

    fn reopen(&self, rel: &Path, flags: i32, ino: u64) -> Option<File> {
        let f = self.open_rel(rel, flags, 0).ok()?;
        f.metadata().is_ok_and(|m| m.ino() == ino).then_some(f)
    }
}

#[cfg(test)]
mod tests;
