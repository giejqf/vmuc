// SPDX-License-Identifier: Apache-2.0
//! A suspended instance (ADR 0007). `~/.vmuc/NAME/suspend/` holds the
//! VM's snapshot (`state.json`, `gic.bin`, `ram.img`) and `suspend.json`,
//! written last, flushed to stable storage, which makes it a complete
//! suspended state. It is resumed at most once: the marker goes before
//! the guest runs again, since the disk moves on from then.

use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use vmuc_config::paths::instance as files;

const MARKER: &str = "suspend.json";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Suspended {
    pub vmuc_version: String,
    pub boot_id: String,
    /// Unix milliseconds when the guest stopped.
    pub stopped_at_ms: u64,
    pub machine: Shape,
}

/// What a resume must rebuild exactly: the VM is restored into a machine
/// with the same vCPUs, memory and devices, in the same order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Shape {
    pub cpus: u32,
    pub memory: u64,
    /// The NIC, when there was one.
    pub network: Option<Nic>,
    /// The shares attached, in device order.
    pub shares: Vec<ShareShape>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Nic {
    pub mac: String,
    /// `shared`, or `bridged (en0)`.
    pub mode: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShareShape {
    pub tag: String,
    pub location: PathBuf,
    pub writable: bool,
    pub dax: bool,
}

pub fn dir(inst: &Path) -> PathBuf {
    inst.join(files::SUSPEND_DIR)
}

/// Completes a suspended state whose snapshot is in [`dir`]: flushes its
/// files, then writes the marker.
pub fn commit(inst: &Path, s: &Suspended) -> io::Result<()> {
    let d = dir(inst);
    for entry in fs::read_dir(&d)? {
        File::open(entry?.path())?.sync_all()?;
    }
    let tmp = d.join("suspend.json.tmp");
    let json = serde_json::to_vec_pretty(s).map_err(io::Error::other)?;
    fs::write(&tmp, json)?;
    File::open(&tmp)?.sync_all()?;
    fs::rename(&tmp, d.join(MARKER))?;
    File::open(&d)?.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_committed_state_has_its_marker_last() {
        let inst = tempfile::tempdir().unwrap();
        fs::create_dir(dir(inst.path())).unwrap();
        fs::write(dir(inst.path()).join("ram.img"), b"ram").unwrap();
        let s = Suspended {
            vmuc_version: "0.1.0".into(),
            boot_id: "b".into(),
            stopped_at_ms: 1,
            machine: Shape {
                cpus: 1,
                memory: 1 << 30,
                network: None,
                shares: Vec::new(),
            },
        };
        commit(inst.path(), &s).unwrap();
        let marker = fs::read(dir(inst.path()).join(MARKER)).unwrap();
        assert_eq!(serde_json::from_slice::<Suspended>(&marker).unwrap(), s);
        assert!(!dir(inst.path()).join("suspend.json.tmp").exists());
    }
}
