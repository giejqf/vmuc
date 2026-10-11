// SPDX-License-Identifier: Apache-2.0
//! The instance's virtiofs shares: one virtio-fs device per configured
//! mount, served by `vmuc-fs` as the user running the VMM. A mount whose
//! host directory is missing is skipped with a warning; the seed lists
//! only the shares that were attached, so the guest never waits on one.

use vmuc_config::ResolvedConfig;
use vmuc_config::cidata::SeedMount;
use vmuc_fs::passthrough::Passthrough;
use vmuc_fs::server::Server;
use vmuc_proto::EventKind;
use vmuc_virtio::devices::fs::Fs;
use vmuc_vmm::VmBuilder;

use crate::instance;
use crate::state::EventLog;

/// A share ready to become a device.
pub struct Share {
    pub mount: SeedMount,
    server: Server,
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

/// Adds one virtio-fs device per share.
pub fn attach(b: &mut VmBuilder, shares: Vec<Share>) -> anyhow::Result<()> {
    for s in shares {
        b.add_virtio(Box::new(Fs::new(&s.mount.tag, s.server)?))?;
    }
    Ok(())
}
