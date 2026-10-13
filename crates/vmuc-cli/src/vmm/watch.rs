// SPDX-License-Identifier: Apache-2.0
//! Host changes in the shares, replayed in the guest so that its file
//! watchers hear of edits made on the Mac
//! (docs/experiments/2026-10-13-e4-inotify-propagation.md). One FSEvents
//! stream covers the writable shares, which hold what dev servers and
//! test runners watch (a read-only share can be the whole home directory,
//! with its churn and privacy-protected folders). One thread classifies
//! each batch per share and hands the changes to the guest agent; another
//! rescans when FSEvents drops changes, since a rescan reads the host
//! filesystem and can block. Neither holds guest memory. After a resume
//! the shares are rescanned once the guest runs, for the changes made
//! while it was suspended (ADR 0007).

use std::path::PathBuf;
use std::sync::{Arc, mpsc};
use std::time::{Duration, SystemTime};

use vmuc_fs::passthrough::Seen;
use vmuc_fs::watch::{self, ChangeKind, HostEvent, Watcher};
use vmuc_proto::{AgentRequest, AgentResponse, EventKind, FsEvent, FsEventKind, Phase};

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

/// `away_since`: when a resumed guest was suspended.
pub fn spawn(
    shared: Arc<Shared>,
    watched: Vec<Watched>,
    away_since: Option<SystemTime>,
) -> std::io::Result<()> {
    if watched.is_empty() {
        return Ok(());
    }
    let watched: Arc<[Watched]> = watched.into();
    let (rescan_tx, rescan_rx) = mpsc::channel();
    if let Some(since) = away_since {
        for share in watched.iter() {
            let _ = rescan_tx.send((share.root.clone(), since));
        }
    }
    let (sh, w) = (Arc::clone(&shared), Arc::clone(&watched));
    std::thread::Builder::new()
        .name("vmuc-fs-rescan".into())
        .spawn(move || rescans(&sh, &w, &rescan_rx))?;
    std::thread::Builder::new()
        .name("vmuc-fs-watch".into())
        .spawn(move || run(&shared, &watched, &rescan_tx))?;
    Ok(())
}

fn run(shared: &Shared, watched: &[Watched], rescan: &mpsc::Sender<(PathBuf, SystemTime)>) {
    let (tx, rx) = mpsc::channel();
    let roots: Vec<PathBuf> = watched.iter().map(|s| s.root.clone()).collect();
    let _watcher = match Watcher::start(&roots, LATENCY, tx) {
        Ok(w) => w,
        Err(e) => return tracing::warn!("not watching the shares for host changes: {e}"),
    };
    // A rescan after a drop looks back to the batch before it, with a
    // second to spare for FSEvents' own delay.
    let mut since = SystemTime::now();
    while let Ok(mut batch) = rx.recv() {
        batch.extend(rx.try_iter().flatten());
        let received = SystemTime::now();
        tracing::debug!(events = batch.len(), "host changes");
        for e in &batch {
            if e.flags & watch::flag::MUST_SCAN_SUBDIRS != 0 {
                tracing::info!(dir = %e.path.display(), "FSEvents dropped changes; rescanning");
                let _ = rescan.send((e.path.clone(), since));
            }
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
            if !send(shared, &share.tag, &changes) {
                let message = "the guest kernel cannot replay host file changes: \
                               watchers in the guest will not see edits made on the host"
                    .to_owned();
                tracing::warn!("{message}");
                return shared.events.push(EventKind::Warning { message });
            }
        }
        since = received - Duration::from_secs(1);
    }
}

/// Rescans after drops, one at a time; drops reported meanwhile are done
/// together.
fn rescans(shared: &Shared, watched: &[Watched], rx: &mpsc::Receiver<(PathBuf, SystemTime)>) {
    // Nothing is replayed before the guest's agent answers.
    while !matches!(shared.phase(), Phase::Running | Phase::Degraded) {
        if shared.machine.stopping() {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    while let Ok(first) = rx.recv() {
        let dirs: Vec<(PathBuf, SystemTime)> =
            std::iter::once(first).chain(rx.try_iter()).collect();
        for share in watched {
            let mut changes: Vec<watch::Change> = Vec::new();
            for (dir, since) in &dirs {
                for c in watch::rescan(&share.root, &share.seen, dir, *since) {
                    if !changes.contains(&c) {
                        changes.push(c);
                    }
                }
            }
            if !send(shared, &share.tag, &changes) {
                return;
            }
        }
    }
}

/// Sends changes in chunks; false when the guest kernel cannot replay.
fn send(shared: &Shared, tag: &str, changes: &[watch::Change]) -> bool {
    changes
        .chunks(CHUNK)
        .all(|chunk| replay(shared, tag, chunk))
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
