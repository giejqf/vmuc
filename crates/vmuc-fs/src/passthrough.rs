// SPDX-License-Identifier: Apache-2.0
//! The passthrough backend: guest file operations carried out on a host
//! directory, as the user the VMM runs as.
//!
//! Nothing may escape the share. Every operation opens the directory
//! that holds its target with `O_NOFOLLOW_ANY` (no symlink in any
//! component, macOS 11+) and acts on the final name with the `*at` calls,
//! which do not follow it either; names from the guest may not contain
//! `/` or be `.` or `..`. (On a Linux host, where the tests also run, only
//! the final component is protected.)

use std::collections::HashMap;
use std::ffi::{CStr, CString, OsStr};
use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::{FileExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use crate::abi::{Attr, ROOT_ID, setattr};
use crate::host::{self, linux};
use crate::inodes::Inodes;

pub type R<T> = Result<T, i32>;

/// RENAME2 flags (Linux `RENAME_NOREPLACE`, `RENAME_EXCHANGE`).
const NOREPLACE: u32 = 1;
const EXCHANGE: u32 = 2;

#[cfg(target_os = "macos")]
const NOFOLLOW_ANY: i32 = libc::O_NOFOLLOW_ANY;
#[cfg(not(target_os = "macos"))]
const NOFOLLOW_ANY: i32 = 0;

/// An open file or a directory listing taken at OPENDIR.
#[derive(Debug)]
pub enum Handle {
    File(File),
    Dir(Vec<DirEntry>),
}

#[derive(Debug, Clone)]
pub struct DirEntry {
    pub name: Vec<u8>,
    pub ino: u64,
    pub kind: u32,
}

fn errno() -> i32 {
    host::to_linux(
        io::Error::last_os_error()
            .raw_os_error()
            .unwrap_or(libc::EIO),
    )
}

fn check(rc: libc::c_int) -> R<()> {
    if rc < 0 { Err(errno()) } else { Ok(()) }
}

fn cpath(p: &Path) -> R<CString> {
    CString::new(p.as_os_str().as_bytes()).map_err(|_| linux::EINVAL)
}

fn key(st: &libc::stat) -> (u64, u64) {
    #[allow(clippy::unnecessary_cast)]
    (st.st_dev as u64, st.st_ino as u64)
}

#[derive(Debug)]
pub struct Passthrough {
    root: PathBuf,
    read_only: bool,
    inodes: Mutex<Inodes>,
    handles: Mutex<HashMap<u64, Arc<Handle>>>,
    next_fh: AtomicU64,
}

impl Passthrough {
    /// Shares `root` (canonicalized, so its own path holds no symlink).
    pub fn new(root: &Path, read_only: bool) -> io::Result<Self> {
        let root = root.canonicalize()?;
        let meta = std::fs::symlink_metadata(&root)?;
        if !meta.is_dir() {
            return Err(io::Error::from_raw_os_error(libc::ENOTDIR));
        }
        Ok(Self {
            root,
            read_only,
            inodes: Mutex::new(Inodes::new((meta.dev(), meta.ino()))),
            handles: Mutex::new(HashMap::new()),
            next_fh: AtomicU64::new(1),
        })
    }

    pub fn read_only(&self) -> bool {
        self.read_only
    }

    fn inodes(&self) -> MutexGuard<'_, Inodes> {
        self.inodes.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn rel(&self, node: u64) -> R<PathBuf> {
        self.inodes()
            .get(node)
            .map(|n| n.rel.clone())
            .ok_or(linux::ESTALE)
    }

    /// `parent/name` after rejecting names that could leave the share.
    pub fn child(&self, parent: u64, name: &[u8]) -> R<PathBuf> {
        if name.is_empty()
            || name.len() > 255
            || name.contains(&b'/')
            || name == b"."
            || name == b".."
        {
            return Err(linux::EINVAL);
        }
        Ok(self.rel(parent)?.join(OsStr::from_bytes(name)))
    }

    pub fn writable(&self) -> R<()> {
        if self.read_only {
            Err(linux::EROFS)
        } else {
            Ok(())
        }
    }

    /// The directory holding `rel`, opened without following any symlink,
    /// and the final name to use with the `*at` calls.
    fn parent(&self, rel: &Path) -> R<(OwnedFd, CString)> {
        let name = rel.file_name().ok_or(linux::EINVAL)?;
        let dir = self.root.join(rel.parent().unwrap_or(Path::new("")));
        let c = cpath(&dir)?;
        // SAFETY: valid C string; the result is checked and owned below.
        let fd = unsafe {
            libc::open(
                c.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | NOFOLLOW_ANY,
            )
        };
        if fd < 0 {
            return Err(errno());
        }
        // SAFETY: a fresh descriptor we own.
        let fd = unsafe { OwnedFd::from_raw_fd(fd) };
        let name = CString::new(name.as_bytes()).map_err(|_| linux::EINVAL)?;
        Ok((fd, name))
    }

    pub fn stat(&self, rel: &Path) -> R<libc::stat> {
        // SAFETY: zeroed stat is a valid out-buffer.
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        if rel.as_os_str().is_empty() {
            let c = cpath(&self.root)?;
            // SAFETY: valid C string and out-buffer.
            check(unsafe { libc::lstat(c.as_ptr(), &raw mut st) })?;
        } else {
            let (dir, name) = self.parent(rel)?;
            // SAFETY: valid dir fd, name and out-buffer.
            check(unsafe {
                libc::fstatat(
                    dir.as_raw_fd(),
                    name.as_ptr(),
                    &raw mut st,
                    libc::AT_SYMLINK_NOFOLLOW,
                )
            })?;
        }
        Ok(st)
    }

    /// Opens a node's file; the final component must not be a symlink.
    pub fn open_rel(&self, rel: &Path, flags: i32, mode: u32) -> R<File> {
        let fd = if rel.as_os_str().is_empty() {
            let c = cpath(&self.root)?;
            // SAFETY: valid C string.
            unsafe { libc::open(c.as_ptr(), flags | libc::O_CLOEXEC, mode) }
        } else {
            let (dir, name) = self.parent(rel)?;
            // SAFETY: valid dir fd and name.
            unsafe {
                libc::openat(
                    dir.as_raw_fd(),
                    name.as_ptr(),
                    flags | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                    mode,
                )
            }
        };
        if fd < 0 {
            return Err(errno());
        }
        // SAFETY: a fresh descriptor we own.
        Ok(unsafe { File::from_raw_fd(fd) })
    }

    /// Looks `rel` up and counts a guest reference to it.
    pub fn lookup_rel(&self, rel: PathBuf) -> R<(u64, Attr)> {
        let st = self.stat(&rel)?;
        let id = self.inodes().intern(rel, key(&st));
        Ok((id, host::attr(&st)))
    }

    pub fn forget(&self, node: u64, n: u64) {
        self.inodes().forget(node, n);
    }

    pub fn getattr(&self, node: u64) -> R<Attr> {
        Ok(host::attr(&self.stat(&self.rel(node)?)?))
    }

    pub fn add_handle(&self, h: Handle) -> u64 {
        let fh = self.next_fh.fetch_add(1, Ordering::Relaxed);
        self.handles
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(fh, Arc::new(h));
        fh
    }

    pub fn handle(&self, fh: u64) -> R<Arc<Handle>> {
        self.handles
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&fh)
            .cloned()
            .ok_or(linux::EBADF)
    }

    pub fn file(&self, fh: u64) -> R<Arc<Handle>> {
        let h = self.handle(fh)?;
        matches!(*h, Handle::File(_))
            .then_some(h)
            .ok_or(linux::EBADF)
    }

    pub fn release(&self, fh: u64) {
        self.handles
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&fh);
    }

    pub fn setattr(&self, node: u64, valid: u32, a: &SetAttr) -> R<Attr> {
        self.writable()?;
        let rel = self.rel(node)?;
        let at = |f: &dyn Fn(i32, &CStr) -> libc::c_int| -> R<()> {
            if rel.as_os_str().is_empty() {
                let dir = self.open_rel(&rel, libc::O_RDONLY, 0)?;
                check(f(dir.as_raw_fd(), c"."))
            } else {
                let (dir, name) = self.parent(&rel)?;
                check(f(dir.as_raw_fd(), &name))
            }
        };
        if valid & setattr::MODE != 0 {
            let mode = (a.mode & 0o7777) as libc::mode_t;
            // SAFETY (closures below): valid dir fds and names.
            at(&|d, n| unsafe { libc::fchmodat(d, n.as_ptr(), mode, 0) })?;
        }
        if valid & (setattr::UID | setattr::GID) != 0 {
            let uid = if valid & setattr::UID != 0 {
                a.uid
            } else {
                u32::MAX
            };
            let gid = if valid & setattr::GID != 0 {
                a.gid
            } else {
                u32::MAX
            };
            at(&|d, n| unsafe {
                libc::fchownat(d, n.as_ptr(), uid, gid, libc::AT_SYMLINK_NOFOLLOW)
            })?;
        }
        if valid & setattr::SIZE != 0 {
            let size = i64::try_from(a.size).map_err(|_| linux::EFBIG)?;
            let truncated = match (valid & setattr::FH != 0).then(|| self.file(a.fh)) {
                Some(Ok(h)) => match &*h {
                    Handle::File(f) => f.set_len(size as u64).map_err(|e| host::errno_of(&e)),
                    Handle::Dir(_) => Err(linux::EISDIR),
                },
                _ => self
                    .open_rel(&rel, libc::O_WRONLY, 0)?
                    .set_len(size as u64)
                    .map_err(|e| host::errno_of(&e)),
            };
            truncated?;
        }
        if valid & (setattr::ATIME | setattr::MTIME) != 0 {
            let t = |set: u32, now: u32, (s, ns): (u64, u32)| libc::timespec {
                tv_sec: if valid & set != 0 && valid & now == 0 {
                    s as _
                } else {
                    0
                },
                tv_nsec: if valid & set == 0 {
                    libc::UTIME_OMIT
                } else if valid & now != 0 {
                    libc::UTIME_NOW
                } else {
                    i64::from(ns) as _
                },
            };
            let times = [
                t(setattr::ATIME, setattr::ATIME_NOW, a.atime),
                t(setattr::MTIME, setattr::MTIME_NOW, a.mtime),
            ];
            at(&|d, n| unsafe {
                libc::utimensat(d, n.as_ptr(), times.as_ptr(), libc::AT_SYMLINK_NOFOLLOW)
            })?;
        }
        self.getattr(node)
    }

    pub fn readlink(&self, node: u64) -> R<Vec<u8>> {
        let rel = self.rel(node)?;
        let (dir, name) = self.parent(&rel)?;
        let mut buf = vec![0u8; 4096];
        // SAFETY: valid dir fd, name and buffer.
        let n = unsafe {
            libc::readlinkat(
                dir.as_raw_fd(),
                name.as_ptr(),
                buf.as_mut_ptr().cast(),
                buf.len(),
            )
        };
        if n < 0 {
            return Err(errno());
        }
        buf.truncate(n as usize);
        Ok(buf)
    }

    /// mkdir, symlink, link, regular-file mknod: runs `make` on the new
    /// name's directory, then looks the result up.
    pub fn make(
        &self,
        parent: u64,
        name: &[u8],
        make: &dyn Fn(i32, &CStr) -> libc::c_int,
    ) -> R<(u64, Attr)> {
        self.writable()?;
        let rel = self.child(parent, name)?;
        let (dir, cname) = self.parent(&rel)?;
        check(make(dir.as_raw_fd(), &cname))?;
        self.lookup_rel(rel)
    }

    pub fn remove(&self, parent: u64, name: &[u8], dir: bool) -> R<()> {
        self.writable()?;
        let rel = self.child(parent, name)?;
        let (d, cname) = self.parent(&rel)?;
        let flags = if dir { libc::AT_REMOVEDIR } else { 0 };
        // SAFETY: valid dir fd and name.
        check(unsafe { libc::unlinkat(d.as_raw_fd(), cname.as_ptr(), flags) })
    }

    pub fn rename(
        &self,
        parent: u64,
        name: &[u8],
        newparent: u64,
        newname: &[u8],
        flags: u32,
    ) -> R<()> {
        self.writable()?;
        let (from, to) = (self.child(parent, name)?, self.child(newparent, newname)?);
        let ((fd, fname), (td, tname)) = (self.parent(&from)?, self.parent(&to)?);
        // SAFETY (all arms): valid dir fds and names.
        let rc = match flags {
            0 => unsafe {
                libc::renameat(
                    fd.as_raw_fd(),
                    fname.as_ptr(),
                    td.as_raw_fd(),
                    tname.as_ptr(),
                )
            },
            #[cfg(target_os = "macos")]
            NOREPLACE | EXCHANGE => {
                let f = if flags == NOREPLACE {
                    libc::RENAME_EXCL
                } else {
                    libc::RENAME_SWAP
                };
                unsafe {
                    libc::renameatx_np(
                        fd.as_raw_fd(),
                        fname.as_ptr(),
                        td.as_raw_fd(),
                        tname.as_ptr(),
                        f,
                    )
                }
            }
            #[cfg(target_os = "linux")]
            NOREPLACE | EXCHANGE => unsafe {
                libc::renameat2(
                    fd.as_raw_fd(),
                    fname.as_ptr(),
                    td.as_raw_fd(),
                    tname.as_ptr(),
                    flags,
                )
            },
            _ => return Err(linux::EINVAL),
        };
        check(rc)?;
        let mut t = self.inodes();
        if flags == EXCHANGE {
            t.exchange(&from, &to);
        } else {
            t.rename(&from, &to);
        }
        Ok(())
    }

    /// FUSE_LINK: another name for `node`.
    pub fn link(&self, node: u64, newparent: u64, newname: &[u8]) -> R<(u64, Attr)> {
        let from = self.rel(node)?;
        let (fd, fname) = self.parent(&from)?;
        self.make(newparent, newname, &|d, n| {
            // SAFETY: valid dir fds and names.
            unsafe { libc::linkat(fd.as_raw_fd(), fname.as_ptr(), d, n.as_ptr(), 0) }
        })
    }

    /// FUSE_CREATE: create and open, then look the file up.
    pub fn create(&self, parent: u64, name: &[u8], flags: i32, mode: u32) -> R<(u64, Attr, u64)> {
        self.writable()?;
        let rel = self.child(parent, name)?;
        let file = self.open_rel(&rel, flags | libc::O_CREAT, mode & 0o7777)?;
        let (id, attr) = self.lookup_rel(rel)?;
        Ok((id, attr, self.add_handle(Handle::File(file))))
    }

    pub fn open(&self, node: u64, flags: i32) -> R<u64> {
        if self.read_only && flags & libc::O_ACCMODE != libc::O_RDONLY {
            return Err(linux::EROFS);
        }
        let file = self.open_rel(&self.rel(node)?, flags & !libc::O_CREAT, 0)?;
        Ok(self.add_handle(Handle::File(file)))
    }

    pub fn read(&self, fh: u64, offset: u64, size: u32) -> R<Vec<u8>> {
        let h = self.file(fh)?;
        let Handle::File(f) = &*h else {
            return Err(linux::EBADF);
        };
        let mut buf = vec![0u8; size as usize];
        let n = f
            .read_at(&mut buf, offset)
            .map_err(|e| host::errno_of(&e))?;
        buf.truncate(n);
        Ok(buf)
    }

    pub fn write(&self, fh: u64, offset: u64, data: &[u8]) -> R<u32> {
        self.writable()?;
        let h = self.file(fh)?;
        let Handle::File(f) = &*h else {
            return Err(linux::EBADF);
        };
        f.write_at(data, offset)
            .map(|n| n as u32)
            .map_err(|e| host::errno_of(&e))
    }

    pub fn fsync(&self, fh: u64) -> R<()> {
        if let Handle::File(f) = &*self.handle(fh)? {
            f.sync_data().map_err(|e| host::errno_of(&e))?;
        }
        Ok(())
    }

    /// OPENDIR: the listing is taken now, so READDIR offsets stay stable.
    pub fn opendir(&self, node: u64) -> R<u64> {
        let dir = self.open_rel(&self.rel(node)?, libc::O_RDONLY | libc::O_DIRECTORY, 0)?;
        let entries = read_dir(dir)?;
        Ok(self.add_handle(Handle::Dir(entries)))
    }

    pub fn statfs(&self) -> R<libc::statvfs> {
        let c = cpath(&self.root)?;
        // SAFETY: zeroed statvfs is a valid out-buffer; valid C string.
        let mut s: libc::statvfs = unsafe { std::mem::zeroed() };
        check(unsafe { libc::statvfs(c.as_ptr(), &raw mut s) })?;
        Ok(s)
    }

    pub fn full_path(&self, node: u64) -> R<Vec<u8>> {
        Ok(self.root.join(self.rel(node)?).into_os_string().into_vec())
    }

    pub fn is_root(node: u64) -> bool {
        node == ROOT_ID
    }
}

/// Values a SETATTR may carry.
#[derive(Debug, Default, Clone, Copy)]
pub struct SetAttr {
    pub fh: u64,
    pub size: u64,
    pub atime: (u64, u32),
    pub mtime: (u64, u32),
    pub mode: u32,
    pub uid: u32,
    pub gid: u32,
}

/// Every entry of a directory, `.` and `..` included.
fn read_dir(dir: File) -> R<Vec<DirEntry>> {
    use std::os::fd::IntoRawFd;
    let fd = dir.into_raw_fd();
    // SAFETY: `fd` is an open directory we hand over to the DIR stream.
    let d = unsafe { libc::fdopendir(fd) };
    if d.is_null() {
        // SAFETY: fdopendir failed, so the fd is still ours to close.
        unsafe { libc::close(fd) };
        return Err(errno());
    }
    let mut out = Vec::new();
    loop {
        // SAFETY: `d` is a live DIR stream.
        let e = unsafe { libc::readdir(d) };
        if e.is_null() {
            break;
        }
        // SAFETY: readdir returned a valid entry with a NUL-terminated name.
        let (name, ino, kind) = unsafe {
            (
                CStr::from_ptr((*e).d_name.as_ptr()).to_bytes().to_vec(),
                (*e).d_ino,
                (*e).d_type,
            )
        };
        #[allow(clippy::unnecessary_cast)]
        out.push(DirEntry {
            name,
            ino: ino as u64,
            kind: u32::from(kind),
        });
    }
    // SAFETY: closes the stream and its fd.
    unsafe { libc::closedir(d) };
    Ok(out)
}
