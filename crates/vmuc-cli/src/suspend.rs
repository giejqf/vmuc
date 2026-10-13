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

impl Shape {
    /// How `now` differs from this, the suspended shape, in words. Whether
    /// there is a NIC is not compared: a resume follows the snapshot.
    pub fn differences(&self, now: &Shape) -> Vec<String> {
        let mut out = Vec::new();
        if self.cpus != now.cpus {
            out.push(format!("vCPUs {} → {}", self.cpus, now.cpus));
        }
        if self.memory != now.memory {
            out.push(format!(
                "memory {} MiB → {} MiB",
                self.memory >> 20,
                now.memory >> 20
            ));
        }
        if let (Some(a), Some(b)) = (&self.network, &now.network) {
            if a.mac != b.mac {
                out.push(format!("MAC address {} → {}", a.mac, b.mac));
            }
            if a.mode != b.mode {
                out.push(format!("network {} → {}", a.mode, b.mode));
            }
        }
        if self.shares != now.shares {
            let list = |s: &[ShareShape]| {
                let names: Vec<String> = s
                    .iter()
                    .map(|m| {
                        let w = if m.writable { " (writable)" } else { "" };
                        format!("{}{w}", m.location.display())
                    })
                    .collect();
                if names.is_empty() {
                    "none".to_owned()
                } else {
                    names.join(", ")
                }
            };
            out.push(format!(
                "shares {} → {}",
                list(&self.shares),
                list(&now.shares)
            ));
        }
        out
    }
}

pub fn dir(inst: &Path) -> PathBuf {
    inst.join(files::SUSPEND_DIR)
}

/// Whether the instance holds a complete suspended state.
pub fn exists(inst: &Path) -> bool {
    dir(inst).join(MARKER).exists()
}

/// The suspended state, if complete. What a suspend that did not finish
/// left behind is removed.
pub fn load(inst: &Path) -> io::Result<Option<Suspended>> {
    let d = dir(inst);
    match fs::read(d.join(MARKER)) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            if d.exists() {
                fs::remove_dir_all(&d)?;
            }
            Ok(None)
        }
        Err(e) => Err(e),
    }
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

/// Ends a suspended state (resumed, or discarded): the marker first, so
/// that nothing resumes it again, then the rest. A RAM image a resumed VM
/// has mapped lives on until the VM ends.
pub fn remove(inst: &Path) -> io::Result<()> {
    let d = dir(inst);
    match fs::remove_file(d.join(MARKER)) {
        Ok(()) => File::open(&d)?.sync_all()?,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    match fs::remove_dir_all(&d) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shape() -> Shape {
        Shape {
            cpus: 4,
            memory: 4 << 30,
            network: Some(Nic {
                mac: "52:55:55:00:00:01".into(),
                mode: "shared".into(),
            }),
            shares: vec![ShareShape {
                tag: "t1".into(),
                location: "/Users/me".into(),
                writable: false,
                dax: false,
            }],
        }
    }

    #[test]
    fn a_changed_machine_is_described() {
        let then = shape();
        assert_eq!(then.differences(&then), Vec::<String>::new());
        let mut now = shape();
        now.cpus = 2;
        now.network = None;
        now.shares[0].writable = true;
        assert_eq!(
            then.differences(&now),
            vec![
                "vCPUs 4 → 2".to_owned(),
                "shares /Users/me → /Users/me (writable)".to_owned()
            ]
        );
        now = shape();
        now.network.as_mut().unwrap().mode = "bridged (en0)".into();
        now.shares.clear();
        assert_eq!(
            then.differences(&now),
            vec![
                "network shared → bridged (en0)".to_owned(),
                "shares /Users/me → none".to_owned()
            ]
        );
    }

    #[test]
    fn a_state_is_complete_once_committed_and_ends_once() {
        let inst = tempfile::tempdir().unwrap();
        assert_eq!(load(inst.path()).unwrap(), None);
        fs::create_dir(dir(inst.path())).unwrap();
        fs::write(dir(inst.path()).join("ram.img"), b"ram").unwrap();
        assert!(!exists(inst.path()));
        assert_eq!(load(inst.path()).unwrap(), None, "unfinished");
        assert!(!dir(inst.path()).exists(), "and cleared away");

        fs::create_dir(dir(inst.path())).unwrap();
        fs::write(dir(inst.path()).join("ram.img"), b"ram").unwrap();
        let s = Suspended {
            vmuc_version: "0.1.0".into(),
            boot_id: "b".into(),
            stopped_at_ms: 1,
            machine: shape(),
        };
        commit(inst.path(), &s).unwrap();
        assert!(exists(inst.path()));
        assert_eq!(load(inst.path()).unwrap(), Some(s));
        remove(inst.path()).unwrap();
        assert!(!exists(inst.path()) && !dir(inst.path()).exists());
        remove(inst.path()).unwrap();
    }
}
