// SPDX-License-Identifier: Apache-2.0
//! Changes the host makes in a share, for the guest's file watchers.
//!
//! virtiofs cannot tell the guest that a file changed under it, so its
//! inotify watchers (dev servers, test runners, build watchers) never
//! hear of an edit made on the Mac. A [`Watcher`] (FSEvents) reports the
//! host's changes to the shared directories, the VMM's own (the guest's
//! writes) excluded (docs/experiments/2026-10-13-e4-inotify-propagation.md).

use std::path::PathBuf;

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
