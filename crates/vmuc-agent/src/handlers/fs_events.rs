// SPDX-License-Identifier: Apache-2.0
//! `FsEvents`: changes the host made in a share, replayed as the inotify
//! events a local change raises. virtiofs cannot tell the guest itself;
//! the vmuc kernel's `FUSE_VMUC_IOC_REPLAY` on the parent directory
//! (guest/kernel/patches/0002-*) drops what the guest cached for the
//! entry and notifies its watchers.

use std::collections::HashMap;
use std::ffi::{CString, OsStr};
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path};

use vmuc_proto::{AgentResponse, FsEvent, FsEventKind};

/// `_IOWR(229, 0x76, struct fuse_vmuc_replay)`, in the C library's
/// request type (`unsigned long` in glibc, `int` in musl).
const IOC_REPLAY: libc::Ioctl =
    (3 << 30) | ((size_of::<Replay>() as libc::Ioctl) << 16) | (229 << 8) | 0x76;

/// `struct fuse_vmuc_replay`.
#[repr(C)]
struct Replay {
    mask: u32,
    /// Pairs MOVED_FROM with MOVED_TO; 0 asks the kernel for a new one.
    cookie: u32,
    namelen: u32,
    padding: u32,
    name: u64,
}

pub fn fs_events(tag: &str, events: &[FsEvent]) -> io::Result<AgentResponse> {
    let mountinfo = std::fs::read_to_string("/proc/self/mountinfo")?;
    let root = mount_point(&mountinfo, tag)
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, format!("{tag} is not mounted")))?;
    let root = open_dir(libc::AT_FDCWD, root.as_os_str(), 0)?;
    let mut dirs: HashMap<&Path, OwnedFd> = HashMap::new();
    let (mut replayed, mut cookie) = (0, 0);
    for e in events {
        let Some((parent, name)) = split(&e.path) else {
            continue;
        };
        if !dirs.contains_key(parent) {
            let beneath = libc::RESOLVE_BENEATH | libc::RESOLVE_NO_SYMLINKS | libc::RESOLVE_NO_XDEV;
            let Ok(fd) = open_dir(root.as_raw_fd(), dot(parent), beneath) else {
                continue;
            };
            dirs.insert(parent, fd);
        }
        let dir = &dirs[parent];
        let carried = if e.kind == FsEventKind::MovedTo {
            cookie
        } else {
            0
        };
        cookie = 0;
        let mut ok = true;
        for &mask in masks(e.kind) {
            match replay(dir, mask, name, carried) {
                Ok(c) if e.kind == FsEventKind::MovedFrom => cookie = c,
                Ok(_) => {}
                Err(err) if err.raw_os_error() == Some(libc::ENOTTY) => {
                    return Ok(AgentResponse::FsEvents {
                        replayed,
                        unsupported: true,
                    });
                }
                // Gone again, or never seen by this guest: nothing to tell.
                Err(_) => ok = false,
            }
        }
        replayed += u32::from(ok);
    }
    Ok(AgentResponse::FsEvents {
        replayed,
        unsupported: false,
    })
}

/// The events a local change of this kind raises, in order.
fn masks(kind: FsEventKind) -> &'static [u32] {
    match kind {
        FsEventKind::Created => &[libc::IN_CREATE],
        FsEventKind::Removed => &[libc::IN_DELETE],
        FsEventKind::Modified => &[libc::IN_MODIFY, libc::IN_CLOSE_WRITE],
        FsEventKind::Attrib => &[libc::IN_ATTRIB],
        FsEventKind::MovedFrom => &[libc::IN_MOVED_FROM],
        FsEventKind::MovedTo => &[libc::IN_MOVED_TO],
    }
}

/// Replays one event on `dir`; returns the cookie the kernel used.
fn replay(dir: &OwnedFd, mask: u32, name: &OsStr, cookie: u32) -> io::Result<u32> {
    let name = name.as_bytes();
    let mut r = Replay {
        mask,
        cookie,
        namelen: name.len() as u32,
        padding: 0,
        name: name.as_ptr() as u64,
    };
    // SAFETY: `r` and the name it points to outlive the call; the kernel
    // reads namelen bytes and writes back only `cookie`.
    let rc = unsafe { libc::ioctl(dir.as_raw_fd(), IOC_REPLAY, &raw mut r) };
    if rc < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(r.cookie)
}

/// `struct open_how` (libc's is non-exhaustive).
#[repr(C)]
struct OpenHow {
    flags: u64,
    mode: u64,
    resolve: u64,
}

fn open_dir(at: libc::c_int, path: &OsStr, resolve: u64) -> io::Result<OwnedFd> {
    let path = CString::new(path.as_bytes()).map_err(|_| io::ErrorKind::InvalidInput)?;
    let how = OpenHow {
        flags: (libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC) as u64,
        mode: 0,
        resolve,
    };
    // SAFETY: valid C string and open_how, the size passed with it.
    let fd = unsafe {
        libc::syscall(
            libc::SYS_openat2,
            at,
            path.as_ptr(),
            &raw const how,
            size_of::<OpenHow>(),
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a descriptor we just opened and own.
    Ok(unsafe { OwnedFd::from_raw_fd(fd as libc::c_int) })
}

fn dot(p: &Path) -> &OsStr {
    if p.as_os_str().is_empty() {
        OsStr::new(".")
    } else {
        p.as_os_str()
    }
}

/// A relative path of plain names, as its parent and final name.
fn split(path: &str) -> Option<(&Path, &OsStr)> {
    let p = Path::new(path);
    if !p.components().all(|c| matches!(c, Component::Normal(_))) {
        return None;
    }
    Some((p.parent()?, p.file_name()?))
}

/// Where the virtiofs share `tag` is mounted (its root, not a bind mount
/// of a subdirectory).
fn mount_point(mountinfo: &str, tag: &str) -> Option<std::path::PathBuf> {
    mountinfo.lines().find_map(|line| {
        let (mount, fs) = line.split_once(" - ")?;
        let mut fs = fs.split(' ');
        let mut mount = mount.split(' ');
        let (root, point) = (mount.nth(3)?, mount.next()?);
        (fs.next()? == "virtiofs" && fs.next()? == tag && root == "/").then(|| unescape(point))
    })
}

/// Undoes mountinfo's octal escapes (`\040` for a space).
fn unescape(s: &str) -> std::path::PathBuf {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let oct = b
            .get(i + 1..i + 4)
            .filter(|d| d.iter().all(|c| (b'0'..=b'7').contains(c)));
        match (b[i], oct) {
            (b'\\', Some(d)) => {
                out.push(d.iter().fold(0u8, |n, c| n.wrapping_mul(8) + (c - b'0')));
                i += 4;
            }
            (c, _) => {
                out.push(c);
                i += 1;
            }
        }
    }
    OsStr::from_bytes(&out).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ioctl_matches_the_kernel_patch() {
        assert_eq!(size_of::<Replay>(), 24);
        assert_eq!(IOC_REPLAY as u32, 0xC018_E576);
    }

    #[test]
    fn finds_the_share_root_mount() {
        let info = "\
24 1 254:1 / / rw,relatime shared:1 - ext4 /dev/vda1 rw
40 24 0:40 /sub /mnt/sub rw,relatime shared:9 - virtiofs vmuc-1 rw
41 24 0:40 / /Users/me/My\\040Project rw,relatime shared:9 - virtiofs vmuc-1 rw
42 24 0:41 / /Users/me ro,relatime shared:10 - virtiofs vmuc-2 ro";
        assert_eq!(
            mount_point(info, "vmuc-1"),
            Some("/Users/me/My Project".into())
        );
        assert_eq!(mount_point(info, "vmuc-2"), Some("/Users/me".into()));
        assert_eq!(mount_point(info, "vmuc-3"), None);
    }

    #[test]
    fn only_plain_relative_paths_are_replayed() {
        assert_eq!(
            split("src/a.rs"),
            Some((Path::new("src"), OsStr::new("a.rs")))
        );
        assert_eq!(split("a.rs"), Some((Path::new(""), OsStr::new("a.rs"))));
        for bad in ["", "/etc/passwd", "../x", "src/../x", "./x"] {
            assert_eq!(split(bad), None, "{bad:?}");
        }
    }
}
