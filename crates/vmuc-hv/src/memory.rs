// SPDX-License-Identifier: Apache-2.0
//! Guest physical memory permission flags.
//!
//! Derived from ArcBox `virt/arcbox-hv/src/memory.rs` (MIT OR Apache-2.0).

use crate::ffi;

/// Permission flags for an IPA mapping (`HV_MEMORY_READ|WRITE|EXEC`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryPermission(u64);

impl MemoryPermission {
    pub const READ: Self = Self(ffi::HV_MEMORY_READ);
    pub const WRITE: Self = Self(ffi::HV_MEMORY_WRITE);
    pub const EXEC: Self = Self(ffi::HV_MEMORY_EXEC);
    pub const READ_WRITE: Self = Self(Self::READ.0 | Self::WRITE.0);
    pub const ALL: Self = Self(Self::READ.0 | Self::WRITE.0 | Self::EXEC.0);

    #[inline]
    pub const fn bits(self) -> u64 {
        self.0
    }

    #[inline]
    pub const fn contains(self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }
}

impl std::ops::BitOr for MemoryPermission {
    type Output = Self;
    #[inline]
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

/// Host page size on Apple silicon. Every IPA mapping, unmapping and
/// reclaim range must be aligned to this, not to the guest's 4 KiB page.
pub const HOST_PAGE_SIZE: usize = 16 * 1024;

/// Aligns `[start, start + len)` *inward* to [`HOST_PAGE_SIZE`]: the
/// returned range is the largest host-page-aligned range fully contained
/// in the input. Returns `None` when nothing aligned fits.
///
/// Inward is the only safe direction for reclaim: XNU would round a
/// misaligned range outward and discard the neighbours' data.
#[must_use]
pub fn align_inward(start: u64, len: u64) -> Option<(u64, u64)> {
    align_inward_to(start, len, HOST_PAGE_SIZE as u64)
}

/// [`align_inward`] to any power-of-two `page`.
#[must_use]
pub fn align_inward_to(start: u64, len: u64, page: u64) -> Option<(u64, u64)> {
    let end = start.checked_add(len)?;
    let a_start = start.div_ceil(page) * page;
    let a_end = (end / page) * page;
    // `then`, not `then_some`: the length must not be computed (and
    // underflow) when the range sits strictly inside one host page.
    (a_end > a_start).then(|| (a_start, a_end - a_start))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bits_match_framework_constants() {
        assert_eq!(MemoryPermission::READ.bits(), 1);
        assert_eq!(MemoryPermission::WRITE.bits(), 2);
        assert_eq!(MemoryPermission::EXEC.bits(), 4);
        assert_eq!(
            MemoryPermission::READ | MemoryPermission::WRITE,
            MemoryPermission::READ_WRITE
        );
        assert!(MemoryPermission::ALL.contains(MemoryPermission::EXEC));
        assert!(!MemoryPermission::READ.contains(MemoryPermission::WRITE));
    }

    #[test]
    fn align_inward_keeps_only_full_host_pages() {
        let p = HOST_PAGE_SIZE as u64;
        assert_eq!(align_inward(0, 2 * p), Some((0, 2 * p)));
        assert_eq!(align_inward(4096, 2 * p), Some((p, p)));
        assert_eq!(align_inward(4096, p), None);
        assert_eq!(align_inward(p, 4096), None);
        assert_eq!(align_inward(p + 4096, 3 * p), Some((2 * p, 2 * p)));
        assert_eq!(align_inward(u64::MAX - 10, 100), None);
        // Strictly inside one host page: nothing, and no underflow.
        assert_eq!(align_inward(4096, 4096), None);
        assert_eq!(align_inward(p + 4096, 0x2000), None);
    }
}
