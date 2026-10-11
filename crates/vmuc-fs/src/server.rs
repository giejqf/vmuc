// SPDX-License-Identifier: Apache-2.0
//! FUSE request dispatch: one request's bytes in, one reply's bytes out.

use std::os::fd::AsRawFd;

use crate::abi::{self, InHeader, Reader, Writer, init, opcode};
use crate::host::{self, linux};
use crate::passthrough::{Handle, Passthrough, R, SetAttr};

/// How long the guest may cache entries and attributes, seconds. Short,
/// because the host can change the share behind the guest's back.
const TIMEOUT_S: u64 = 1;
/// Largest READ or WRITE payload offered (MAX_PAGES × 4 KiB).
pub const MAX_WRITE: u32 = 1 << 20;
const MAX_PAGES: u16 = 256;
/// File type bits of the guest's Linux-encoded mode (same as Darwin's).
const S_IFMT: u32 = 0o170_000;
const S_IFREG: u32 = 0o100_000;

#[derive(Debug)]
pub struct Server {
    fs: Passthrough,
    xattr: bool,
}

impl Server {
    /// Extended attributes are off: the guest kernel asks for
    /// `security.capability` before every write and for ACLs and labels
    /// on every `ls -l` entry, a round trip each, and macOS has no use for
    /// Linux's namespaces. The first ENOSYS makes it stop asking.
    pub fn new(fs: Passthrough) -> Self {
        Self { fs, xattr: false }
    }

    /// Passes extended attributes through to the host.
    pub fn with_xattrs(fs: Passthrough) -> Self {
        Self { fs, xattr: true }
    }

    pub fn fs(&self) -> &Passthrough {
        &self.fs
    }

    /// Handles one request; `None` for those the protocol answers with
    /// silence (FORGET, BATCH_FORGET).
    pub fn handle(&self, req: &[u8]) -> Option<Vec<u8>> {
        let mut r = Reader::new(req);
        let Ok(h) = InHeader::read(&mut r) else {
            return None;
        };
        match h.opcode {
            opcode::FORGET => {
                if let Ok(n) = r.u64() {
                    self.fs.forget(h.nodeid, n);
                }
                return None;
            }
            opcode::BATCH_FORGET => {
                let count = r.u32().unwrap_or(0);
                let _ = r.u32();
                for _ in 0..count {
                    let (Ok(node), Ok(n)) = (r.u64(), r.u64()) else {
                        break;
                    };
                    self.fs.forget(node, n);
                }
                return None;
            }
            _ => {}
        }
        let out = self.dispatch(&h, &mut r);
        Some(match out {
            Ok(body) => abi::reply(h.unique, 0, &body),
            Err(e) => abi::reply(h.unique, -e, &[]),
        })
    }

    #[allow(clippy::too_many_lines)]
    fn dispatch(&self, h: &InHeader, r: &mut Reader) -> R<Vec<u8>> {
        let bad = |_| linux::EINVAL;
        let fs = &self.fs;
        let mut w = Writer::default();
        match h.opcode {
            opcode::GETXATTR | opcode::LISTXATTR | opcode::SETXATTR | opcode::REMOVEXATTR
                if !self.xattr =>
            {
                return Err(linux::ENOSYS);
            }
            opcode::INIT => return init_reply(r).map_err(bad),
            opcode::DESTROY | opcode::SYNCFS | opcode::INTERRUPT | opcode::FLUSH => {}
            opcode::LOOKUP => {
                let name = r.cstr().map_err(bad)?;
                return Ok(entry(fs.lookup_rel(fs.child(h.nodeid, name)?)?));
            }
            opcode::GETATTR => abi::attr_out(&mut w, &fs.getattr(h.nodeid)?, TIMEOUT_S),
            opcode::SETATTR => {
                let valid = r.u32().map_err(bad)?;
                r.skip(4).map_err(bad)?;
                let fh = r.u64().map_err(bad)?;
                let size = r.u64().map_err(bad)?;
                r.skip(8).map_err(bad)?; // lock_owner
                let (atime, mtime) = (r.u64().map_err(bad)?, r.u64().map_err(bad)?);
                r.skip(8).map_err(bad)?; // ctime
                let (atimensec, mtimensec) = (r.u32().map_err(bad)?, r.u32().map_err(bad)?);
                r.skip(4).map_err(bad)?;
                let mode = r.u32().map_err(bad)?;
                r.skip(4).map_err(bad)?;
                let (uid, gid) = (r.u32().map_err(bad)?, r.u32().map_err(bad)?);
                let a = SetAttr {
                    fh,
                    size,
                    atime: (atime, atimensec),
                    mtime: (mtime, mtimensec),
                    mode,
                    uid,
                    gid,
                };
                abi::attr_out(&mut w, &fs.setattr(h.nodeid, valid, &a)?, TIMEOUT_S);
            }
            opcode::READLINK => return fs.readlink(h.nodeid),
            opcode::SYMLINK => {
                let name = r.cstr().map_err(bad)?.to_vec();
                let target =
                    std::ffi::CString::new(r.cstr().map_err(bad)?).map_err(|_| linux::EINVAL)?;
                // SAFETY: valid dir fd and C strings.
                let created = fs.make(h.nodeid, &name, &|d, n| unsafe {
                    libc::symlinkat(target.as_ptr(), d, n.as_ptr())
                })?;
                return Ok(entry(created));
            }
            opcode::MKNOD => {
                let mode = r.u32().map_err(bad)?;
                r.skip(12).map_err(bad)?;
                let name = r.cstr().map_err(bad)?;
                if mode & S_IFMT != S_IFREG {
                    return Err(linux::EPERM);
                }
                let perm = (mode & 0o7777) as libc::c_uint;
                let created = fs.make(h.nodeid, name, &|d, n| {
                    // SAFETY: valid dir fd and name; the fd is closed at once.
                    let fd = unsafe {
                        libc::openat(
                            d,
                            n.as_ptr(),
                            libc::O_CREAT | libc::O_EXCL | libc::O_WRONLY | libc::O_CLOEXEC,
                            perm,
                        )
                    };
                    if fd >= 0 {
                        // SAFETY: closing the descriptor just opened.
                        unsafe { libc::close(fd) };
                    }
                    fd.min(0)
                })?;
                return Ok(entry(created));
            }
            opcode::MKDIR => {
                let mode = r.u32().map_err(bad)?;
                r.skip(4).map_err(bad)?;
                let name = r.cstr().map_err(bad)?;
                let perm = (mode & 0o7777) as libc::mode_t;
                // SAFETY: valid dir fd and name.
                let created = fs.make(h.nodeid, name, &|d, n| unsafe {
                    libc::mkdirat(d, n.as_ptr(), perm)
                })?;
                return Ok(entry(created));
            }
            opcode::UNLINK | opcode::RMDIR => {
                fs.remove(h.nodeid, r.cstr().map_err(bad)?, h.opcode == opcode::RMDIR)?;
            }
            opcode::RENAME | opcode::RENAME2 => {
                let newdir = r.u64().map_err(bad)?;
                let flags = if h.opcode == opcode::RENAME2 {
                    let f = r.u32().map_err(bad)?;
                    r.skip(4).map_err(bad)?;
                    f
                } else {
                    0
                };
                let (old, new) = (r.cstr().map_err(bad)?, r.cstr().map_err(bad)?);
                fs.rename(h.nodeid, old, newdir, new, flags)?;
            }
            opcode::LINK => {
                let old = r.u64().map_err(bad)?;
                return Ok(entry(fs.link(old, h.nodeid, r.cstr().map_err(bad)?)?));
            }
            opcode::OPEN => {
                let flags = host::open_flags(r.u32().map_err(bad)?);
                w.u64(fs.open(h.nodeid, flags)?).u32(0).u32(0);
            }
            opcode::CREATE => {
                let flags = host::open_flags(r.u32().map_err(bad)?);
                let mode = r.u32().map_err(bad)?;
                r.skip(8).map_err(bad)?;
                let (id, attr, fh) = fs.create(h.nodeid, r.cstr().map_err(bad)?, flags, mode)?;
                abi::entry_out(&mut w, id, &attr, TIMEOUT_S);
                w.u64(fh).u32(0).u32(0);
            }
            opcode::READ => {
                let (fh, offset, size) = (
                    r.u64().map_err(bad)?,
                    r.u64().map_err(bad)?,
                    r.u32().map_err(bad)?,
                );
                return fs.read(fh, offset, size.min(MAX_WRITE));
            }
            opcode::WRITE => {
                let (fh, offset, size) = (
                    r.u64().map_err(bad)?,
                    r.u64().map_err(bad)?,
                    r.u32().map_err(bad)?,
                );
                r.skip(20).map_err(bad)?; // write_flags, lock_owner, flags, padding
                let data = r.rest();
                let data = data.get(..size as usize).ok_or(linux::EINVAL)?;
                w.u32(fs.write(fh, offset, data)?).u32(0);
            }
            opcode::STATFS => return Ok(statfs_out(&fs.statfs()?)),
            opcode::RELEASE | opcode::RELEASEDIR => fs.release(r.u64().map_err(bad)?),
            opcode::FSYNC | opcode::FSYNCDIR => fs.fsync(r.u64().map_err(bad)?)?,
            opcode::OPENDIR => {
                w.u64(fs.opendir(h.nodeid)?).u32(0).u32(0);
            }
            opcode::READDIR | opcode::READDIRPLUS => {
                let (fh, offset, size) = (
                    r.u64().map_err(bad)?,
                    r.u64().map_err(bad)?,
                    r.u32().map_err(bad)?,
                );
                return self.readdir(
                    h.nodeid,
                    fh,
                    offset,
                    size as usize,
                    h.opcode == opcode::READDIRPLUS,
                );
            }
            opcode::ACCESS => {
                let mask = r.u32().map_err(bad)?;
                if mask & 2 != 0 {
                    fs.writable()?;
                }
                fs.getattr(h.nodeid)?;
            }
            opcode::GETXATTR | opcode::LISTXATTR => {
                let size = r.u32().map_err(bad)?;
                r.skip(4).map_err(bad)?;
                let path = fs.full_path(h.nodeid)?;
                let mut buf = vec![0u8; size as usize];
                let n = if h.opcode == opcode::GETXATTR {
                    host::getxattr(&path, r.cstr().map_err(bad)?, &mut buf)
                } else {
                    host::listxattr(&path, &mut buf)
                }
                .map_err(|e| host::errno_of(&e))?;
                if size == 0 {
                    w.u32(n as u32).u32(0);
                } else {
                    buf.truncate(n);
                    return Ok(buf);
                }
            }
            opcode::SETXATTR => {
                fs.writable()?;
                let (size, flags) = (r.u32().map_err(bad)?, r.u32().map_err(bad)?);
                let name = r.cstr().map_err(bad)?;
                let value = r.rest().get(..size as usize).ok_or(linux::EINVAL)?;
                host::setxattr(&fs.full_path(h.nodeid)?, name, value, flags)
                    .map_err(|e| host::errno_of(&e))?;
            }
            opcode::REMOVEXATTR => {
                fs.writable()?;
                host::removexattr(&fs.full_path(h.nodeid)?, r.cstr().map_err(bad)?)
                    .map_err(|e| host::errno_of(&e))?;
            }
            opcode::LSEEK => {
                let (fh, offset, whence) = (
                    r.u64().map_err(bad)?,
                    r.u64().map_err(bad)?,
                    r.u32().map_err(bad)?,
                );
                let h = fs.file(fh)?;
                let Handle::File(f) = &*h else {
                    return Err(linux::EBADF);
                };
                // SAFETY: plain lseek on our own descriptor.
                let off =
                    unsafe { libc::lseek(f.as_raw_fd(), offset as libc::off_t, whence as i32) };
                if off < 0 {
                    return Err(host::errno_of(&std::io::Error::last_os_error()));
                }
                w.u64(off as u64);
            }
            _ => return Err(linux::ENOSYS),
        }
        Ok(w.0)
    }

    /// READDIR / READDIRPLUS from the listing taken at OPENDIR; the
    /// offset is the index to resume at. READDIRPLUS looks entries up
    /// (counting a lookup) only for the entries it emits.
    fn readdir(&self, node: u64, fh: u64, offset: u64, size: usize, plus: bool) -> R<Vec<u8>> {
        let h = self.fs.handle(fh)?;
        let Handle::Dir(entries) = &*h else {
            return Err(linux::ENOTDIR);
        };
        let mut w = Writer::default();
        for (i, e) in entries.iter().enumerate().skip(offset as usize) {
            let need = abi::dirent_size(&e.name) + if plus { 128 } else { 0 };
            if w.0.len() + need > size {
                break;
            }
            let next = i as u64 + 1;
            if plus {
                let dots = e.name == b"." || e.name == b"..";
                let looked = if dots {
                    None
                } else {
                    self.fs.lookup_rel(self.fs.child(node, &e.name)?).ok()
                };
                match looked {
                    Some((id, attr)) => abi::entry_out(&mut w, id, &attr, TIMEOUT_S),
                    None => abi::entry_out(
                        &mut w,
                        0,
                        &abi::Attr {
                            ino: e.ino,
                            ..abi::Attr::default()
                        },
                        0,
                    ),
                }
            }
            abi::dirent(&mut w, e.ino, next, e.kind, &e.name);
        }
        Ok(w.0)
    }
}

/// `fuse_kstatfs`. Block and file counts are u32 on Darwin and u64 on
/// Linux, so the widening is not uniform across hosts.
#[allow(clippy::useless_conversion)]
fn statfs_out(s: &libc::statvfs) -> Vec<u8> {
    let mut w = Writer::default();
    w.u64(u64::from(s.f_blocks))
        .u64(u64::from(s.f_bfree))
        .u64(u64::from(s.f_bavail));
    w.u64(u64::from(s.f_files)).u64(u64::from(s.f_ffree));
    w.u32(s.f_bsize as u32)
        .u32(255)
        .u32(s.f_frsize as u32)
        .u32(0);
    w.bytes(&[0; 24]);
    w.0
}

/// `fuse_entry_out` for a looked-up node.
fn entry((id, attr): (u64, abi::Attr)) -> Vec<u8> {
    let mut w = Writer::default();
    abi::entry_out(&mut w, id, &attr, TIMEOUT_S);
    w.0
}

fn init_reply(r: &mut Reader) -> Result<Vec<u8>, abi::Truncated> {
    let (_major, minor, max_readahead) = (r.u32()?, r.u32()?, r.u32()?);
    let flags = u64::from(r.u32()?);
    let flags2 = if flags & init::INIT_EXT != 0 {
        u64::from(r.u32().unwrap_or(0)) << 32
    } else {
        0
    };
    let offered = flags | flags2;
    let ours = init::ASYNC_READ
        | init::ATOMIC_O_TRUNC
        | init::BIG_WRITES
        | init::AUTO_INVAL_DATA
        | init::DO_READDIRPLUS
        | init::READDIRPLUS_AUTO
        | init::PARALLEL_DIROPS
        | init::MAX_PAGES
        | init::CACHE_SYMLINKS;
    let granted = offered & ours;
    let mut w = Writer::default();
    w.u32(abi::KERNEL_VERSION)
        .u32(minor.min(abi::KERNEL_MINOR_VERSION))
        .u32(max_readahead);
    w.u32(granted as u32).u16(64).u16(48).u32(MAX_WRITE).u32(1);
    w.u16(MAX_PAGES).u16(0).u32((granted >> 32) as u32).u32(0);
    w.bytes(&[0; 24]);
    Ok(w.0)
}

#[cfg(test)]
mod tests;
