// SPDX-License-Identifier: Apache-2.0
//! Host changes in the shares, replayed in the guest so that its file
//! watchers hear of edits made on the Mac
//! (docs/experiments/2026-10-13-e4-inotify-propagation.md). One FSEvents
//! stream covers every share; this thread classifies each batch per share
//! and hands the changes to the guest agent. It holds no guest memory.

use std::path::PathBuf;
use std::sync::{Arc, mpsc};
use std::time::Duration;

use vmuc_fs::passthrough::Seen;
use vmuc_fs::watch::{self, ChangeKind, HostEvent, Watcher};
use vmuc_proto::{AgentRequest, AgentResponse, EventKind, FsEvent, FsEventKind};

use super::{Shared, ready};

/// How long FSEvents coalesces a burst; a lone change comes at once.
const LATENCY: Duration = Duration::from_millis(50);
/// Changes per agent request.
const CHUNK: usize = 1024;
const AGENT_TIMEOUT: Duration = Duration::from_secs(5);

/// A share as the watcher sees it.
pub struct Watched {
    pub tag: String,
    pub root: PathBuf,
    pub seen: Seen,
}

pub fn spawn(shared: Arc<Shared>, watched: Vec<Watched>) -> std::io::Result<()> {
    if !watched.is_empty() {
        std::thread::Builder::new()
            .name("vmuc-fs-watch".into())
            .spawn(move || run(&shared, &watched))?;
    }
    Ok(())
}

fn run(shared: &Shared, watched: &[Watched]) {
    let (tx, rx) = mpsc::channel();
    let roots: Vec<PathBuf> = watched.iter().map(|s| s.root.clone()).collect();
    let _watcher = match Watcher::start(&roots, LATENCY, tx) {
        Ok(w) => w,
        Err(e) => return tracing::warn!("not watching the shares for host changes: {e}"),
    };
    while let Ok(mut batch) = rx.recv() {
        batch.extend(rx.try_iter().flatten());
        tracing::debug!(events = batch.len(), "host changes");
        for e in batch
            .iter()
            .filter(|e| e.flags & watch::flag::MUST_SCAN_SUBDIRS != 0)
        {
            tracing::warn!(path = %e.path.display(), "FSEvents dropped changes; guest watchers miss them");
        }
        for share in watched {
            // A share inside another is in both; the guest shows it where
            // it is mounted, and the agent skips names a mount covers.
            let mine: Vec<HostEvent> = batch
                .iter()
                .filter(|e| e.path.starts_with(&share.root))
                .cloned()
                .collect();
            let changes = watch::classify(&share.root, &share.seen, &mine);
            for chunk in changes.chunks(CHUNK) {
                if !replay(shared, &share.tag, chunk) {
                    let message = "the guest kernel cannot replay host file changes: \
                                   watchers in the guest will not see edits made on the host"
                        .to_owned();
                    tracing::warn!("{message}");
                    return shared.events.push(EventKind::Warning { message });
                }
            }
        }
    }
}

/// Sends one chunk; false when the guest kernel cannot replay at all.
fn replay(shared: &Shared, tag: &str, changes: &[watch::Change]) -> bool {
    let req = AgentRequest::FsEvents {
        tag: tag.to_owned(),
        events: changes
            .iter()
            .map(|c| FsEvent {
                kind: kind(c.kind),
                path: c.path.clone(),
            })
            .collect(),
    };
    match ready::agent_call(&shared.vsock, &req, AGENT_TIMEOUT) {
        Ok(AgentResponse::FsEvents {
            unsupported: true, ..
        }) => false,
        Ok(AgentResponse::FsEvents { replayed, .. }) => {
            tracing::debug!(tag, replayed, sent = changes.len(), "replayed host changes");
            true
        }
        // Not mounted yet, or the agent is not up: no guest watcher can
        // be waiting for these.
        other => {
            tracing::debug!(tag, ?other, "host changes not replayed");
            true
        }
    }
}

fn kind(k: ChangeKind) -> FsEventKind {
    match k {
        ChangeKind::Created => FsEventKind::Created,
        ChangeKind::Removed => FsEventKind::Removed,
        ChangeKind::Modified => FsEventKind::Modified,
        ChangeKind::Attrib => FsEventKind::Attrib,
        ChangeKind::MovedFrom => FsEventKind::MovedFrom,
        ChangeKind::MovedTo => FsEventKind::MovedTo,
    }
}
