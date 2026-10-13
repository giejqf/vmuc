// SPDX-License-Identifier: Apache-2.0
//! The instance's virtiofs shares: one virtio-fs device per configured
//! mount, served by `vmuc-fs` as the user running the VMM. A mount whose
//! host directory is missing is skipped with a warning; the seed lists
//! only the shares that were attached, so the guest never waits on one.

use std::os::fd::RawFd;
use std::sync::Arc;

use vmuc_config::ResolvedConfig;
use vmuc_config::cidata::SeedMount;
use vmuc_fs::dax::DaxWindow;
use vmuc_fs::passthrough::Passthrough;
use vmuc_fs::server::Server;
use vmuc_proto::EventKind;
use vmuc_virtio::devices::fs::Fs;
use vmuc_vmm::{FileWindow, VmBuilder};

use super::watch::Watched;
use crate::instance;
use crate::state::EventLog;

/// A share ready to become a device.
pub struct Share {
    pub mount: SeedMount,
    server: Server,
}

impl Share {
    /// What the host-change watcher needs of it.
    pub fn watched(&self) -> Watched {
        Watched {
            tag: self.mount.tag.clone(),
            root: self.server.fs().root().to_owned(),
            seen: self.server.fs().seen(),
        }
    }
}

/// Opens every configured mount's host directory.
pub fn open(resolved: &ResolvedConfig, events: &EventLog) -> Vec<Share> {
    resolved
        .mounts
        .iter()
        .zip(instance::seed_mounts(resolved))
        .filter_map(
            |(m, mount)| match Passthrough::new(&m.location, !m.writable) {
                Ok(fs) => Some(Share {
                    mount,
                    server: Server::new(fs),
                }),
                Err(e) => {
                    let message = format!("not sharing {}: {e}", m.location.display());
                    tracing::warn!("{message}");
                    events.push(EventKind::Warning { message });
                    None
                }
            },
        )
        .collect()
}

/// Adds one virtio-fs device per share, with a DAX window for the shares
/// that asked for one.
pub fn attach(b: &mut VmBuilder, shares: Vec<Share>) -> anyhow::Result<()> {
    for s in shares {
        let dev = if s.mount.dax {
            let window = b.add_file_window()?;
            let (base, size) = (window.base(), window.size());
            let server = s.server.with_dax(Arc::new(Dax(window)));
            Fs::new(&s.mount.tag, server)?.with_window(base, size)
        } else {
            Fs::new(&s.mount.tag, s.server)?
        };
        b.add_virtio(Box::new(dev))?;
    }
    Ok(())
}

/// The VMM's file window as the FUSE server's DAX window.
#[derive(Debug)]
struct Dax(Arc<FileWindow>);

impl DaxWindow for Dax {
    fn size(&self) -> u64 {
        self.0.size()
    }
    fn map(
        &self,
        fd: RawFd,
        foffset: u64,
        moffset: u64,
        len: u64,
        writable: bool,
    ) -> std::io::Result<()> {
        self.0.map(fd, foffset, moffset, len, writable)
    }
    fn unmap(&self, moffset: u64, len: u64) -> std::io::Result<()> {
        self.0.unmap(moffset, len)
    }
}
