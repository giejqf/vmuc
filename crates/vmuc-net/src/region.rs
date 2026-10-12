// SPDX-License-Identifier: Apache-2.0
//! The memory the VMM and the vmnet helper share for their frame rings
//! (ADR 0001): created by the VMM as an unnamed POSIX shared memory
//! object, handed to the helper as an fd, mapped by both.

use std::ffi::CString;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::sync::atomic::{AtomicU32, Ordering};

/// A shared mapping, unmapped on drop.
#[derive(Debug)]
pub struct Region {
    base: *mut u8,
    len: usize,
}

// SAFETY: the mapping is plain memory, valid until drop; all access to
// its contents goes through raw pointers and atomics (see `ring`).
unsafe impl Send for Region {}
// SAFETY: as above; `Region` itself is never mutated after creation.
unsafe impl Sync for Region {}

fn check(rc: libc::c_int) -> io::Result<()> {
    if rc < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

impl Region {
    /// A new zeroed region of `len` bytes and the fd to hand over. The
    /// shared memory object is unlinked at once: only fds reach it.
    pub fn create(len: usize) -> io::Result<(Self, OwnedFd)> {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let nanos = std::time::UNIX_EPOCH
            .elapsed()
            .map_or(0, |d| d.subsec_nanos());
        for _ in 0..16 {
            // macOS limits names to 31 bytes (PSHMNAMLEN); O_EXCL makes a
            // collision a retry, not a shared object.
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = CString::new(format!("/vmuc.{:x}.{nanos:x}.{n:x}", std::process::id()))
                .map_err(io::Error::other)?;
            // SAFETY: a valid C string; the result is checked.
            let fd = unsafe {
                libc::shm_open(
                    path.as_ptr(),
                    libc::O_RDWR | libc::O_CREAT | libc::O_EXCL,
                    0o600 as libc::c_uint,
                )
            };
            if fd < 0 {
                let e = io::Error::last_os_error();
                if e.raw_os_error() == Some(libc::EEXIST) {
                    continue;
                }
                return Err(e);
            }
            // SAFETY: a fresh descriptor we own.
            let fd = unsafe { OwnedFd::from_raw_fd(fd) };
            // SAFETY: unlinking the name we just created.
            unsafe { libc::shm_unlink(path.as_ptr()) };
            let size = libc::off_t::try_from(len).map_err(io::Error::other)?;
            // SAFETY: plain ftruncate on our descriptor.
            check(unsafe { libc::ftruncate(fd.as_raw_fd(), size) })?;
            let region = Self::map(&fd, len)?;
            return Ok((region, fd));
        }
        Err(io::Error::from_raw_os_error(libc::EEXIST))
    }

    /// Maps `len` bytes of a region received as `fd`. The other side chose
    /// the object's size, so it is checked, not trusted: at least `len`
    /// (macOS rounds a shared memory object up to whole 16 KiB pages).
    pub fn from_fd(fd: &OwnedFd, len: usize) -> io::Result<Self> {
        // SAFETY: a zeroed stat is a valid out-buffer.
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        // SAFETY: valid descriptor and out-buffer.
        check(unsafe { libc::fstat(fd.as_raw_fd(), &raw mut st) })?;
        if u64::try_from(st.st_size).map_or(true, |size| size < len as u64) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("shared region is {} bytes, expected {len}", st.st_size),
            ));
        }
        Self::map(fd, len)
    }

    /// Anonymous shared memory, for tests.
    pub fn anonymous(len: usize) -> io::Result<Self> {
        // SAFETY: a fresh anonymous mapping, checked below.
        let p = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_ANON | libc::MAP_SHARED,
                -1,
                0,
            )
        };
        if p == libc::MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            base: p.cast(),
            len,
        })
    }

    fn map(fd: &OwnedFd, len: usize) -> io::Result<Self> {
        // SAFETY: mapping `len` bytes of an object at least that long.
        let p = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                fd.as_raw_fd(),
                0,
            )
        };
        if p == libc::MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            base: p.cast(),
            len,
        })
    }

    pub fn base(&self) -> *mut u8 {
        self.base
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

impl Drop for Region {
    fn drop(&mut self) {
        // SAFETY: unmapping exactly what we mapped; nothing uses it after.
        unsafe { libc::munmap(self.base.cast(), self.len) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn created_regions_are_shared_through_the_fd_and_checked_for_size() {
        let (a, fd) = Region::create(1 << 16).unwrap();
        let b = Region::from_fd(&fd, 1 << 16).unwrap();
        // SAFETY: both mappings are 64 KiB; byte writes/reads in bounds.
        unsafe {
            a.base().add(100).write_volatile(42);
            assert_eq!(b.base().add(100).read_volatile(), 42);
        }
        assert!(
            Region::from_fd(&fd, 1 << 17).is_err(),
            "smaller than asked for"
        );
    }

    #[test]
    fn sizes_that_are_not_whole_pages_round_trip() {
        for len in [crate::ring::REGION_BYTES, 100_000, 4096 * 3] {
            let (_a, fd) = Region::create(len).unwrap();
            assert_eq!(Region::from_fd(&fd, len).unwrap().len(), len, "{len}");
        }
    }
}
