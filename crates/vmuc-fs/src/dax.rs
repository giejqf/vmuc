// SPDX-License-Identifier: Apache-2.0
//! virtiofs DAX: the guest maps file ranges straight from a window in its
//! physical address space (the device's shared memory region 0) instead
//! of copying them into its page cache. The guest asks with
//! FUSE_SETUPMAPPING and FUSE_REMOVEMAPPING; the server checks the ranges
//! and hands them to a [`DaxWindow`], which the VMM implements over
//! Hypervisor.framework (`docs/experiments/2026-10-13-e2-virtiofs-dax.md`).

use std::io;
use std::os::fd::RawFd;

use crate::host::linux;
use crate::passthrough::R;

/// log2 of the alignment the server asks for (`map_alignment`): the
/// 16 KiB host page.
pub const MAP_ALIGNMENT_SHIFT: u16 = 14;
const ALIGN: u64 = 1 << MAP_ALIGNMENT_SHIFT;
/// `fuse_setupmapping_in.flags`.
pub const SETUPMAPPING_WRITE: u64 = 1 << 0;

/// Where SETUPMAPPING puts file ranges.
pub trait DaxWindow: Send + Sync + std::fmt::Debug {
    /// Window size in bytes.
    fn size(&self) -> u64;
    /// Maps `len` bytes of `fd` from `foffset` at window offset
    /// `moffset`, replacing whatever was mapped there.
    fn map(
        &self,
        fd: RawFd,
        foffset: u64,
        moffset: u64,
        len: u64,
        writable: bool,
    ) -> io::Result<()>;
    /// Unmaps `[moffset, moffset + len)`; ranges with nothing mapped are
    /// fine.
    fn unmap(&self, moffset: u64, len: u64) -> io::Result<()>;
}

/// A range the guest may ask for: aligned, non-empty, inside the window.
pub fn check(window: u64, foffset: u64, moffset: u64, len: u64) -> R<()> {
    let aligned = [foffset, moffset, len].iter().all(|v| v % ALIGN == 0);
    let inside = moffset.checked_add(len).is_some_and(|end| end <= window);
    if aligned && len > 0 && inside {
        Ok(())
    } else {
        Err(linux::EINVAL)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges_must_be_aligned_non_empty_and_inside() {
        let w = 64 << 20;
        assert!(check(w, 0, 0, 2 << 20).is_ok());
        assert!(check(w, 2 << 20, w - (2 << 20), 2 << 20).is_ok());
        assert_eq!(check(w, 4096, 0, 2 << 20), Err(linux::EINVAL));
        assert_eq!(check(w, 0, 0, 0), Err(linux::EINVAL));
        assert_eq!(check(w, 0, w - ALIGN, 2 * ALIGN), Err(linux::EINVAL));
        assert_eq!(check(w, 0, u64::MAX - ALIGN + 1, ALIGN), Err(linux::EINVAL));
    }
}
