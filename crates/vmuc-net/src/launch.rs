// SPDX-License-Identifier: Apache-2.0
//! The VMM's side of the vmnet helper: check where it lives, start it
//! through `sudo -n` with one end of a datagram socketpair as stdin, and
//! read its hello. Also the sudoers rule `vmuc sudoers` prints.
//!
//! sudo may only ever run a helper that no one but root can replace: a
//! rule naming a user-writable file is a root shell for that user. So the
//! helper and every directory above it must be root-owned and not group-
//! or world-writable (Homebrew's prefix is user-owned, so the helper is
//! installed separately, as Lima does for socket_vmnet).

use std::io::Read;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::net::UnixDatagram;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use crate::helper::{Hello, Mode};

/// Where `vmuc sudoers` tells users to install the helper.
pub const DEFAULT_HELPER: &str = "/usr/local/libexec/vmuc/vmuc-vmnet-helper";

/// Bytes of frames queued per direction. A unix datagram socket holds
/// 4 KiB on macOS (`net.local.dgram.recvspace`), two frames, and a send
/// into a full one fails with ENOBUFS instead of waiting, so every burst
/// beyond two frames was dropped.
pub const SOCKET_BUFFER: usize = 4 << 20;

/// Send and receive buffer sizes of a socket.
pub trait SocketBuffers {
    fn set_buffers(&self, bytes: usize) -> std::io::Result<()>;
}

impl SocketBuffers for UnixDatagram {
    fn set_buffers(&self, bytes: usize) -> std::io::Result<()> {
        let v = libc::c_int::try_from(bytes).unwrap_or(libc::c_int::MAX);
        for opt in [libc::SO_SNDBUF, libc::SO_RCVBUF] {
            // SAFETY: a valid socket and a c_int option value.
            let rc = unsafe {
                libc::setsockopt(
                    self.as_raw_fd(),
                    libc::SOL_SOCKET,
                    opt,
                    (&raw const v).cast(),
                    size_of::<libc::c_int>() as libc::socklen_t,
                )
            };
            if rc != 0 {
                return Err(std::io::Error::last_os_error());
            }
        }
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum LaunchError {
    #[error("{0} does not exist; see `vmuc sudoers` to install it")]
    Missing(PathBuf),
    #[error("{path} must be owned by root and writable only by root ({why}); see `vmuc sudoers`")]
    Unsafe { path: PathBuf, why: String },
    #[error("starting the vmnet helper: {0}")]
    Spawn(#[from] std::io::Error),
    #[error(
        "the vmnet helper did not start ({0}); is the sudoers rule installed? see `vmuc sudoers`"
    )]
    NoHello(String),
}

/// The helper and each directory above it must be root-owned and not
/// writable by group or others.
pub fn check_root_owned(helper: &Path) -> Result<(), LaunchError> {
    check_owner(helper, 0)
}

fn check_owner(helper: &Path, owner: u32) -> Result<(), LaunchError> {
    if !helper.is_file() {
        return Err(LaunchError::Missing(helper.to_path_buf()));
    }
    let mut p = Some(helper);
    while let Some(path) = p {
        let m = std::fs::metadata(path).map_err(LaunchError::Spawn)?;
        let mode = m.permissions().mode();
        let why = if m.uid() != owner {
            Some(format!("owner uid {}", m.uid()))
        } else if mode & 0o022 != 0 {
            Some(format!("mode {:o}", mode & 0o777))
        } else {
            None
        };
        if let Some(why) = why {
            return Err(LaunchError::Unsafe {
                path: path.to_path_buf(),
                why,
            });
        }
        p = path.parent().filter(|q| !q.as_os_str().is_empty());
    }
    Ok(())
}

/// Lines for /etc/sudoers.d/vmuc: the two invocations the VMM uses.
pub fn sudoers_rule(helper: &Path) -> String {
    let h = helper.display();
    format!(
        "# vmuc: let admins start the vmnet relay helper without a password.\n\
         %admin ALL=(root:wheel) NOPASSWD:NOSETENV: {h} --mode shared\n\
         %admin ALL=(root:wheel) NOPASSWD:NOSETENV: {h} --mode bridged --interface *\n"
    )
}

/// A running helper: frames on `socket`, `child` exits when it closes.
#[derive(Debug)]
pub struct Launched {
    pub socket: UnixDatagram,
    pub hello: Hello,
    pub child: Child,
}

/// Starts the helper as root through `sudo -n` and waits for its hello.
pub fn launch(helper: &Path, mode: &Mode) -> Result<Launched, LaunchError> {
    check_root_owned(helper)?;
    launch_with(Command::new("sudo").arg("-n").arg(helper), mode)
}

fn launch_with(cmd: &mut Command, mode: &Mode) -> Result<Launched, LaunchError> {
    let (ours, theirs) = UnixDatagram::pair()?;
    // The helper's end must be sized here, before it is handed over (and
    // so an installed helper needs no update); the NIC sizes its own.
    theirs.set_buffers(SOCKET_BUFFER)?;
    let mut child = cmd
        .args(mode.to_args())
        .stdin(Stdio::from(std::os::fd::OwnedFd::from(theirs)))
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()?;
    // A datagram socket sees no EOF when the helper (or sudo) exits, so
    // watch the child while waiting for the hello.
    ours.set_read_timeout(Some(Duration::from_millis(100)))?;
    let mut buf = vec![0u8; 4096];
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    let hello = loop {
        match ours.recv(&mut buf) {
            Ok(n) => break serde_json::from_slice::<Hello>(&buf[..n]).map_err(|e| e.to_string()),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(e) => break Err(e.to_string()),
        }
        if let Some(status) = child.try_wait()? {
            break Err(format!("it exited with {status}"));
        }
        if std::time::Instant::now() > deadline {
            break Err("no hello within 15 s".into());
        }
    };
    let hello = match hello {
        Ok(h) => h,
        Err(e) => {
            let _ = child.kill();
            let mut stderr = String::new();
            if let Some(mut s) = child.stderr.take() {
                let _ = s.read_to_string(&mut stderr);
            }
            let _ = child.wait();
            let detail = stderr.trim();
            return Err(LaunchError::NoHello(if detail.is_empty() {
                e
            } else {
                detail.to_owned()
            }));
        }
    };
    ours.set_read_timeout(None)?;
    Ok(Launched {
        socket: ours,
        hello,
        child,
    })
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    /// macOS only: Linux caps datagram queues by count
    /// (`net.unix.max_dgram_qlen`), not bytes, and the helper is Mac-only.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_socketpair_queues_bursts() {
        let frame = [0u8; 1514];
        let fits = |a: &UnixDatagram| (0..2000).take_while(|_| a.send(&frame).is_ok()).count();
        let (a, _b) = UnixDatagram::pair().unwrap();
        a.set_nonblocking(true).unwrap();
        let default = fits(&a);
        assert!(default < 10, "default buffers held {default} frames");
        let (a, b) = UnixDatagram::pair().unwrap();
        a.set_buffers(SOCKET_BUFFER).unwrap();
        b.set_buffers(SOCKET_BUFFER).unwrap();
        a.set_nonblocking(true).unwrap();
        assert_eq!(fits(&a), 2000);
    }

    #[test]
    fn ownership_check_walks_every_ancestor() {
        let dir = tempfile::tempdir().unwrap();
        let helper = dir.path().join("vmuc-vmnet-helper");
        std::fs::write(&helper, b"#!/bin/sh\n").unwrap();
        // Owned by us, not root: refused.
        assert!(matches!(
            check_root_owned(&helper),
            Err(LaunchError::Unsafe { .. })
        ));
        // A missing file is reported as missing.
        assert!(matches!(
            check_root_owned(&dir.path().join("nope")),
            Err(LaunchError::Missing(_))
        ));
        // Owned by "root" (our uid here): a group-writable file is refused...
        // SAFETY: getuid has no preconditions.
        let me = unsafe { libc::getuid() };
        std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o775)).unwrap();
        let err = check_owner(&helper, me).unwrap_err().to_string();
        assert!(err.contains("mode 775"), "{err}");
        // ...and the system dirs above a temp dir are not ours.
        std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        let err = check_owner(&helper, me).unwrap_err().to_string();
        assert!(err.contains("owner uid"), "{err}");
    }

    #[test]
    fn sudoers_names_only_the_two_invocations() {
        let r = sudoers_rule(Path::new(DEFAULT_HELPER));
        assert!(r.contains(
            "NOPASSWD:NOSETENV: /usr/local/libexec/vmuc/vmuc-vmnet-helper --mode shared\n"
        ));
        assert!(r.contains("--mode bridged --interface *\n"));
        assert_eq!(r.lines().filter(|l| l.starts_with('%')).count(), 2);
    }

    #[test]
    fn hello_is_read_and_failures_carry_the_helpers_message() {
        // A stand-in helper: a shell script writing the hello to its stdin
        // socket is not possible, so drive `launch_with` with a program that
        // fails like a missing sudoers rule does.
        let mut cmd = Command::new("sh");
        cmd.args([
            "-c",
            "echo 'sudo: a password is required' >&2; exit 1",
            "sh",
        ]);
        let err = launch_with(&mut cmd, &Mode::Shared)
            .unwrap_err()
            .to_string();
        assert!(err.contains("a password is required"), "{err}");
    }
}
