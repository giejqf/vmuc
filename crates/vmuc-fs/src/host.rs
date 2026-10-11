// SPDX-License-Identifier: Apache-2.0
//! The host side of the Linux ABI the guest speaks: errno and open-flag
//! translation, `stat` to FUSE attributes, and xattr calls (Darwin's take
//! a position and options; Linux's are `l*xattr`). On a Linux host most
//! of this is the identity, which keeps the server testable there too.

use std::ffi::CString;
use std::io;

use crate::abi::Attr;

/// Linux errno values (asm-generic), what the guest expects.
pub mod linux {
    pub const EPERM: i32 = 1;
    pub const ENOENT: i32 = 2;
    pub const EIO: i32 = 5;
    pub const EBADF: i32 = 9;
    pub const EAGAIN: i32 = 11;
    pub const ENOMEM: i32 = 12;
    pub const EACCES: i32 = 13;
    pub const EBUSY: i32 = 16;
    pub const EEXIST: i32 = 17;
    pub const EXDEV: i32 = 18;
    pub const ENOTDIR: i32 = 20;
    pub const EISDIR: i32 = 21;
    pub const EINVAL: i32 = 22;
    pub const EMFILE: i32 = 24;
    pub const EFBIG: i32 = 27;
    pub const ENOSPC: i32 = 28;
    pub const EROFS: i32 = 30;
    pub const EMLINK: i32 = 31;
    pub const ERANGE: i32 = 34;
    pub const EDEADLK: i32 = 35;
    pub const ENAMETOOLONG: i32 = 36;
    pub const ENOSYS: i32 = 38;
    pub const ENOTEMPTY: i32 = 39;
    pub const ELOOP: i32 = 40;
    pub const ENODATA: i32 = 61;
    pub const EOVERFLOW: i32 = 75;
    pub const EOPNOTSUPP: i32 = 95;
    pub const ETIMEDOUT: i32 = 110;
    pub const ESTALE: i32 = 116;
    pub const EDQUOT: i32 = 122;
}

/// A host errno as the Linux value the guest understands.
pub fn to_linux(errno: i32) -> i32 {
    match errno {
        libc::EPERM => linux::EPERM,
        libc::ENOENT => linux::ENOENT,
        libc::EBADF => linux::EBADF,
        libc::EAGAIN => linux::EAGAIN,
        libc::ENOMEM => linux::ENOMEM,
        libc::EACCES => linux::EACCES,
        libc::EBUSY => linux::EBUSY,
        libc::EEXIST => linux::EEXIST,
        libc::EXDEV => linux::EXDEV,
        libc::ENOTDIR => linux::ENOTDIR,
        libc::EISDIR => linux::EISDIR,
        libc::EINVAL => linux::EINVAL,
        libc::EMFILE => linux::EMFILE,
        libc::EFBIG => linux::EFBIG,
        libc::ENOSPC => linux::ENOSPC,
        libc::EROFS => linux::EROFS,
        libc::EMLINK => linux::EMLINK,
        libc::ERANGE => linux::ERANGE,
        libc::EDEADLK => linux::EDEADLK,
        libc::ENAMETOOLONG => linux::ENAMETOOLONG,
        libc::ENOSYS => linux::ENOSYS,
        libc::ENOTEMPTY => linux::ENOTEMPTY,
        libc::ELOOP => linux::ELOOP,
        libc::EOVERFLOW => linux::EOVERFLOW,
        libc::ETIMEDOUT => linux::ETIMEDOUT,
        libc::ESTALE => linux::ESTALE,
        libc::EDQUOT => linux::EDQUOT,
        #[cfg(target_os = "macos")]
        libc::ENOATTR => linux::ENODATA,
        #[cfg(target_os = "linux")]
        libc::ENODATA => linux::ENODATA,
        libc::ENOTSUP => linux::EOPNOTSUPP,
        _ => linux::EIO,
    }
}

/// The Linux errno for an I/O error.
pub fn errno_of(e: &io::Error) -> i32 {
    e.raw_os_error().map_or(linux::EIO, to_linux)
}

/// Linux aarch64 open flags (`asm/fcntl.h`) the guest sends.
mod lflag {
    pub const O_ACCMODE: u32 = 0o3;
    pub const O_CREAT: u32 = 0o100;
    pub const O_EXCL: u32 = 0o200;
    pub const O_TRUNC: u32 = 0o1000;
    pub const O_APPEND: u32 = 0o2000;
    pub const O_NONBLOCK: u32 = 0o4000;
    pub const O_DSYNC: u32 = 0o10_000;
    pub const O_DIRECTORY: u32 = 0o40_000;
    pub const O_NOFOLLOW: u32 = 0o100_000;
    pub const O_SYNC: u32 = 0o4_010_000;
}

/// Guest open flags as host flags. Flags the host has no use for
/// (`O_DIRECT`, `O_LARGEFILE`, `O_NOATIME`, `O_CLOEXEC`) are dropped;
/// the server always sets close-on-exec itself.
pub fn open_flags(guest: u32) -> i32 {
    let mut f = (guest & lflag::O_ACCMODE) as i32;
    for (l, h) in [
        (lflag::O_CREAT, libc::O_CREAT),
        (lflag::O_EXCL, libc::O_EXCL),
        (lflag::O_TRUNC, libc::O_TRUNC),
        (lflag::O_APPEND, libc::O_APPEND),
        (lflag::O_NONBLOCK, libc::O_NONBLOCK),
        (lflag::O_DIRECTORY, libc::O_DIRECTORY),
        (lflag::O_NOFOLLOW, libc::O_NOFOLLOW),
    ] {
        if guest & l != 0 {
            f |= h;
        }
    }
    if guest & lflag::O_SYNC == lflag::O_SYNC {
        f |= libc::O_SYNC;
    } else if guest & lflag::O_DSYNC != 0 {
        f |= libc::O_DSYNC;
    }
    f | libc::O_CLOEXEC
}

/// FUSE attributes from a host `lstat`.
pub fn attr(st: &libc::stat) -> Attr {
    let ts = |s: i64, ns: i64| (s.max(0) as u64, ns.clamp(0, 999_999_999) as u32);
    // stat's field widths differ between Darwin and Linux (st_mode is u16
    // or u32, st_nlink u16 or u64), so these casts are not uniform.
    #[allow(clippy::unnecessary_cast, clippy::cast_lossless)]
    Attr {
        ino: st.st_ino as u64,
        size: st.st_size as u64,
        blocks: st.st_blocks as u64,
        atime: ts(st.st_atime as i64, st.st_atime_nsec as i64),
        mtime: ts(st.st_mtime as i64, st.st_mtime_nsec as i64),
        ctime: ts(st.st_ctime as i64, st.st_ctime_nsec as i64),
        mode: st.st_mode as u32,
        nlink: st.st_nlink as u32,
        uid: st.st_uid,
        gid: st.st_gid,
        rdev: st.st_rdev as u32,
        blksize: st.st_blksize as u32,
    }
}

/// `DT_*` type of a file mode, for directory entries.
pub fn dirent_type(mode: u32) -> u32 {
    (mode >> 12) & 0xf
}

fn cstr(b: &[u8]) -> io::Result<CString> {
    CString::new(b).map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))
}

fn check(n: isize) -> io::Result<usize> {
    if n < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(n as usize)
    }
}

/// Linux `XATTR_CREATE`/`XATTR_REPLACE` as host flags.
fn xattr_flags(linux: u32) -> i32 {
    #[cfg(target_os = "macos")]
    let (create, replace) = (libc::XATTR_CREATE, libc::XATTR_REPLACE);
    #[cfg(not(target_os = "macos"))]
    let (create, replace) = (libc::XATTR_CREATE, libc::XATTR_REPLACE);
    let mut f = 0;
    if linux & 1 != 0 {
        f |= create;
    }
    if linux & 2 != 0 {
        f |= replace;
    }
    f
}

/// `lgetxattr`: reads into `buf`, or returns the size when `buf` is empty.
pub fn getxattr(path: &[u8], name: &[u8], buf: &mut [u8]) -> io::Result<usize> {
    let (p, n) = (cstr(path)?, cstr(name)?);
    let ptr = if buf.is_empty() {
        std::ptr::null_mut()
    } else {
        buf.as_mut_ptr().cast()
    };
    // SAFETY: valid C strings; `ptr` is null or `buf` of `buf.len()` bytes.
    #[cfg(target_os = "macos")]
    let n = unsafe {
        libc::getxattr(
            p.as_ptr(),
            n.as_ptr(),
            ptr,
            buf.len(),
            0,
            libc::XATTR_NOFOLLOW,
        )
    };
    // SAFETY: as above.
    #[cfg(not(target_os = "macos"))]
    let n = unsafe { libc::lgetxattr(p.as_ptr(), n.as_ptr(), ptr, buf.len()) };
    check(n)
}

/// `llistxattr`: NUL-separated names, or the size when `buf` is empty.
pub fn listxattr(path: &[u8], buf: &mut [u8]) -> io::Result<usize> {
    let p = cstr(path)?;
    let ptr = if buf.is_empty() {
        std::ptr::null_mut()
    } else {
        buf.as_mut_ptr().cast()
    };
    // SAFETY: valid C string; `ptr` is null or `buf`.
    #[cfg(target_os = "macos")]
    let n = unsafe { libc::listxattr(p.as_ptr(), ptr, buf.len(), libc::XATTR_NOFOLLOW) };
    // SAFETY: as above.
    #[cfg(not(target_os = "macos"))]
    let n = unsafe { libc::llistxattr(p.as_ptr(), ptr, buf.len()) };
    check(n)
}

pub fn setxattr(path: &[u8], name: &[u8], value: &[u8], linux_flags: u32) -> io::Result<()> {
    let (p, n) = (cstr(path)?, cstr(name)?);
    let flags = xattr_flags(linux_flags);
    // SAFETY: valid C strings and value slice.
    #[cfg(target_os = "macos")]
    let rc = unsafe {
        libc::setxattr(
            p.as_ptr(),
            n.as_ptr(),
            value.as_ptr().cast(),
            value.len(),
            0,
            flags | libc::XATTR_NOFOLLOW,
        )
    };
    // SAFETY: as above.
    #[cfg(not(target_os = "macos"))]
    let rc = unsafe {
        libc::lsetxattr(
            p.as_ptr(),
            n.as_ptr(),
            value.as_ptr().cast(),
            value.len(),
            flags,
        )
    };
    check(rc as isize).map(|_| ())
}

pub fn removexattr(path: &[u8], name: &[u8]) -> io::Result<()> {
    let (p, n) = (cstr(path)?, cstr(name)?);
    // SAFETY: valid C strings.
    #[cfg(target_os = "macos")]
    let rc = unsafe { libc::removexattr(p.as_ptr(), n.as_ptr(), libc::XATTR_NOFOLLOW) };
    // SAFETY: as above.
    #[cfg(not(target_os = "macos"))]
    let rc = unsafe { libc::lremovexattr(p.as_ptr(), n.as_ptr()) };
    check(rc as isize).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errno_translation_targets_linux_numbers() {
        assert_eq!(to_linux(libc::ENOENT), 2);
        assert_eq!(to_linux(libc::ENOTEMPTY), 39);
        assert_eq!(to_linux(libc::ELOOP), 40);
        assert_eq!(to_linux(libc::ENAMETOOLONG), 36);
        assert_eq!(to_linux(libc::EAGAIN), 11);
        #[cfg(target_os = "macos")]
        assert_eq!(to_linux(libc::ENOATTR), 61);
        assert_eq!(to_linux(9999), linux::EIO);
    }

    #[test]
    fn linux_open_flags_become_host_flags() {
        let f = open_flags(0o2 | 0o100 | 0o1000 | 0o400_000);
        assert_eq!(f & libc::O_ACCMODE, libc::O_RDWR);
        assert_ne!(f & libc::O_CREAT, 0);
        assert_ne!(f & libc::O_TRUNC, 0);
        assert_ne!(f & libc::O_CLOEXEC, 0);
        let d = open_flags(0o40_000 | 0o100_000);
        assert_ne!(d & libc::O_DIRECTORY, 0);
        assert_ne!(d & libc::O_NOFOLLOW, 0);
        assert_eq!(open_flags(0o2000) & libc::O_APPEND, libc::O_APPEND);
    }

    #[test]
    fn xattrs_round_trip_on_a_real_file() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("x");
        std::fs::write(&f, b"").unwrap();
        let p = f.as_os_str().as_encoded_bytes();
        match setxattr(p, b"user.vmuc", b"1", 0) {
            Ok(()) => {}
            Err(e) if e.raw_os_error() == Some(libc::ENOTSUP) => return, // tmpfs without xattrs
            Err(e) => panic!("{e}"),
        }
        assert_eq!(getxattr(p, b"user.vmuc", &mut []).unwrap(), 1);
        let mut v = [0u8; 4];
        assert_eq!(getxattr(p, b"user.vmuc", &mut v).unwrap(), 1);
        assert_eq!(v[0], b'1');
        let mut names = vec![0u8; listxattr(p, &mut []).unwrap()];
        listxattr(p, &mut names).unwrap();
        assert!(names.split(|&b| b == 0).any(|n| n == b"user.vmuc"));
        removexattr(p, b"user.vmuc").unwrap();
        let e = getxattr(p, b"user.vmuc", &mut v).unwrap_err();
        assert_eq!(errno_of(&e), linux::ENODATA);
    }
}
