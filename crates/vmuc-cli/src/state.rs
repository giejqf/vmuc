// SPDX-License-Identifier: Apache-2.0
//! Instance state on disk: the VMM's pid (trusted only within the host
//! boot session that wrote it, so a stale file from before a Mac reboot
//! never reads as running) and the `events.jsonl` log.

use std::fs;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use vmuc_config::paths::instance as files;
use vmuc_proto::{Event, EventKind};

/// The host's boot session (`kern.bootsessionuuid`); changes on reboot.
pub fn host_boot_session() -> String {
    crate::host::sysctl_text("kern.bootsessionuuid").unwrap_or_default()
}

pub fn pid_alive(pid: u32) -> bool {
    // SAFETY: signal 0 only checks that the process exists.
    let rc = unsafe { libc::kill(pid as libc::pid_t, 0) };
    rc == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// SIGKILLs `pid` and waits (up to 5 s) for it to be gone.
pub fn kill(pid: u32) -> std::io::Result<()> {
    // SAFETY: plain kill(2) on a pid we just checked is our VMM.
    if unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    let started = std::time::Instant::now();
    while pid_alive(pid) && started.elapsed() < std::time::Duration::from_secs(5) {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    Ok(())
}

/// The running VMM's pid, if the pid file is from this host boot and the
/// process is alive.
pub fn running_pid(dir: &Path) -> Option<u32> {
    let pid: u32 = fs::read_to_string(dir.join(files::VMM_PID))
        .ok()?
        .trim()
        .parse()
        .ok()?;
    let session = fs::read_to_string(dir.join(files::BOOT_SESSION)).unwrap_or_default();
    (session.trim() == host_boot_session() && pid_alive(pid)).then_some(pid)
}

/// Records this process as the instance's VMM (atomic renames).
pub fn write_pid(dir: &Path) -> std::io::Result<()> {
    write_atomic(
        &dir.join(files::BOOT_SESSION),
        &format!("{}\n", host_boot_session()),
    )?;
    write_atomic(
        &dir.join(files::VMM_PID),
        &format!("{}\n", std::process::id()),
    )
}

pub fn remove_pid(dir: &Path) {
    let _ = fs::remove_file(dir.join(files::VMM_PID));
    let _ = fs::remove_file(dir.join(files::VMM_SOCK));
}

fn write_atomic(path: &Path, text: &str) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, text)?;
    fs::rename(tmp, path)
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

/// Appends events, one JSON object per line.
#[derive(Debug)]
pub struct EventLog {
    path: PathBuf,
}

impl EventLog {
    /// Starts a fresh log for this boot.
    pub fn create(dir: &Path) -> std::io::Result<Self> {
        let path = dir.join(files::EVENTS);
        fs::write(&path, "")?;
        Ok(Self { path })
    }

    pub fn push(&self, kind: EventKind) {
        let line = serde_json::to_string(&Event {
            time: now_ms(),
            kind,
        })
        .unwrap_or_default();
        let r = fs::OpenOptions::new()
            .append(true)
            .open(&self.path)
            .and_then(|mut f| writeln!(f, "{line}"));
        if let Err(e) = r {
            tracing::warn!("{}: {e}", self.path.display());
        }
    }
}

/// Every event in the log, skipping lines that do not parse.
pub fn read_events(dir: &Path) -> Vec<Event> {
    let Ok(f) = fs::File::open(dir.join(files::EVENTS)) else {
        return Vec::new();
    };
    std::io::BufReader::new(f)
        .lines()
        .map_while(Result::ok)
        .filter_map(|l| serde_json::from_str(&l).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pid_files_and_events_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(running_pid(dir.path()), None);
        write_pid(dir.path()).unwrap();
        assert_eq!(running_pid(dir.path()), Some(std::process::id()));
        // A pid file from another host boot is stale.
        fs::write(dir.path().join(files::BOOT_SESSION), "old-session\n").unwrap();
        assert_eq!(running_pid(dir.path()), None);
        remove_pid(dir.path());
        assert!(!dir.path().join(files::VMM_PID).exists());

        let log = EventLog::create(dir.path()).unwrap();
        log.push(EventKind::Booting {
            boot_id: "b1".into(),
        });
        fs::OpenOptions::new()
            .append(true)
            .open(dir.path().join(files::EVENTS))
            .unwrap()
            .write_all(b"garbage\n")
            .unwrap();
        log.push(EventKind::Exiting {
            reason: "done".into(),
        });
        let ev = read_events(dir.path());
        assert_eq!(ev.len(), 2);
        assert_eq!(
            ev[0].kind,
            EventKind::Booting {
                boot_id: "b1".into()
            }
        );
    }

    #[test]
    fn liveness() {
        assert!(pid_alive(std::process::id()));
        assert!(!pid_alive(999_999));
    }
}
