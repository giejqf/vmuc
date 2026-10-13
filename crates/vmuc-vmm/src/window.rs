// SPDX-License-Identifier: Apache-2.0
//! A window of guest physical address space backed by host file ranges:
//! virtiofs DAX. Each range is mapped `MAP_SHARED` and given to the VM
//! with its own `hv_vm_map`, so the guest reads and writes the host's
//! file cache directly (billed to the file cache, not to the VMM). A
//! range is never mapped over a live one (the framework refuses that):
//! whatever overlaps is unmapped first, and a range cut in two keeps its
//! remainders mapped from the same host addresses
//! (`docs/experiments/2026-10-13-e2-virtiofs-dax.md`).

use std::collections::BTreeMap;
use std::io;
use std::os::fd::RawFd;
use std::sync::{Mutex, PoisonError};

use vmuc_hv::{HvVm, MemoryPermission};

/// Stage-2 mapping, behind a trait so the bookkeeping is testable off HV.
pub trait Stage2Map: Send + Sync {
    /// # Safety
    /// `host..host+len` stays mapped in this process while mapped here.
    unsafe fn map(&self, host: *mut u8, ipa: u64, len: u64, writable: bool) -> io::Result<()>;
    fn unmap(&self, ipa: u64, len: u64) -> io::Result<()>;
}

/// Hypervisor.framework's stage 2.
#[derive(Debug, Default)]
pub struct HvMap;

impl Stage2Map for HvMap {
    unsafe fn map(&self, host: *mut u8, ipa: u64, len: u64, writable: bool) -> io::Result<()> {
        let perm = if writable {
            MemoryPermission::ALL
        } else {
            MemoryPermission::READ | MemoryPermission::EXEC
        };
        // SAFETY: the caller keeps the host range mapped while it is here.
        unsafe { HvVm::remap(host, ipa, len as usize, perm) }.map_err(io::Error::other)
    }

    fn unmap(&self, ipa: u64, len: u64) -> io::Result<()> {
        HvVm::unmap(ipa, len as usize).map_err(io::Error::other)
    }
}

/// One mapped range: window offset → host mapping.
#[derive(Debug, Clone, Copy)]
struct Piece {
    host: usize,
    len: u64,
    writable: bool,
}

pub struct FileWindow {
    base: u64,
    size: u64,
    stage2: Box<dyn Stage2Map>,
    pieces: Mutex<BTreeMap<u64, Piece>>,
}

impl std::fmt::Debug for FileWindow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileWindow")
            .field("base", &format_args!("{:#x}", self.base))
            .field("size", &format_args!("{:#x}", self.size))
            .finish_non_exhaustive()
    }
}

impl FileWindow {
    pub fn new(base: u64, size: u64, stage2: Box<dyn Stage2Map>) -> Self {
        Self {
            base,
            size,
            stage2,
            pieces: Mutex::new(BTreeMap::new()),
        }
    }

    /// Guest physical base.
    pub fn base(&self) -> u64 {
        self.base
    }

    pub fn size(&self) -> u64 {
        self.size
    }

    /// Maps `len` bytes of `fd` from `foffset` at window offset `moffset`,
    /// replacing whatever overlaps. The caller checked the range.
    pub fn map(
        &self,
        fd: RawFd,
        foffset: u64,
        moffset: u64,
        len: u64,
        writable: bool,
    ) -> io::Result<()> {
        let mut pieces = self.pieces.lock().unwrap_or_else(PoisonError::into_inner);
        self.cut(&mut pieces, moffset, len)?;
        let prot = libc::PROT_READ | if writable { libc::PROT_WRITE } else { 0 };
        // SAFETY: a fresh shared mapping of an open file; checked below.
        let host = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len as usize,
                prot,
                libc::MAP_SHARED,
                fd,
                foffset as libc::off_t,
            )
        };
        if host == libc::MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        // Read ahead into the host mapping: a guest's first touch of each
        // page is then a stage-2 fault on a page already there, three times
        // faster for a cold 2 GiB read (E2).
        // SAFETY: our own fresh mapping.
        unsafe { libc::madvise(host, len as usize, libc::MADV_WILLNEED) };
        // SAFETY: the mapping stays until the piece is cut or dropped.
        if let Err(e) = unsafe {
            self.stage2
                .map(host.cast(), self.base + moffset, len, writable)
        } {
            // SAFETY: our own mapping, never handed out.
            unsafe { libc::munmap(host, len as usize) };
            return Err(e);
        }
        let host = host as usize;
        pieces.insert(
            moffset,
            Piece {
                host,
                len,
                writable,
            },
        );
        Ok(())
    }

    /// Unmaps `[moffset, moffset + len)`; nothing mapped there is fine.
    pub fn unmap(&self, moffset: u64, len: u64) -> io::Result<()> {
        let mut pieces = self.pieces.lock().unwrap_or_else(PoisonError::into_inner);
        self.cut(&mut pieces, moffset, len)
    }

    /// Takes `[start, start + len)` out of the window: pieces inside go,
    /// pieces that stick out keep their outer parts.
    fn cut(&self, pieces: &mut BTreeMap<u64, Piece>, start: u64, len: u64) -> io::Result<()> {
        let end = start + len;
        let hit: Vec<u64> = pieces
            .range(..end)
            .filter(|(off, p)| **off + p.len > start)
            .map(|(off, _)| *off)
            .collect();
        for off in hit {
            let Some(p) = pieces.remove(&off) else {
                continue;
            };
            self.stage2.unmap(self.base + off, p.len)?;
            let (lo, hi) = (start.max(off), end.min(off + p.len));
            // SAFETY: the overlap lies inside the piece's own mapping.
            unsafe {
                libc::munmap(
                    (p.host + (lo - off) as usize) as *mut libc::c_void,
                    (hi - lo) as usize,
                )
            };
            for (from, to) in [(off, lo), (hi, off + p.len)] {
                if to > from {
                    let rest = Piece {
                        host: p.host + (from - off) as usize,
                        len: to - from,
                        writable: p.writable,
                    };
                    // SAFETY: still mapped: only the overlap was unmapped.
                    unsafe {
                        self.stage2.map(
                            rest.host as *mut u8,
                            self.base + from,
                            rest.len,
                            rest.writable,
                        )?;
                    }
                    pieces.insert(from, rest);
                }
            }
        }
        Ok(())
    }
}

impl Drop for FileWindow {
    fn drop(&mut self) {
        let pieces = std::mem::take(
            self.pieces
                .get_mut()
                .unwrap_or_else(PoisonError::into_inner),
        );
        for (off, p) in pieces {
            // After hv_vm_destroy this fails, which is fine.
            let _ = self.stage2.unmap(self.base + off, p.len);
            // SAFETY: the piece's own mapping, no longer in the VM.
            unsafe { libc::munmap(p.host as *mut libc::c_void, p.len as usize) };
        }
    }
}

#[cfg(test)]
mod tests;
