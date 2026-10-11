// SPDX-License-Identifier: Apache-2.0
//! A bounds-checked view of guest RAM for device models.
//!
//! Guest RAM is written by running vCPUs at the same time as device
//! threads read and write it, so this module never hands out a Rust
//! reference into it: every accessor copies through raw pointers, and the
//! virtqueue index fields go through atomics. The backing stays mapped as
//! long as any view exists (the view holds an `Arc` to its owner), so a
//! device thread that outlives the VM can never touch unmapped memory.
//!
//! Production views come from `vmuc-vmm`'s guest RAM mapping; tests build
//! one from a `Vec<u8>` with [`GuestMemory::from_vec`].

use std::any::Any;
use std::sync::Arc;
use std::sync::atomic::{AtomicU16, Ordering};

/// A guest physical address.
pub type Gpa = u64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum MemError {
    #[error("guest range {gpa:#x}+{len:#x} is outside RAM")]
    OutOfRange { gpa: Gpa, len: u64 },
    #[error("guest address {gpa:#x} is not aligned to {align}")]
    Misaligned { gpa: Gpa, align: u64 },
}

pub type MemResult<T> = Result<T, MemError>;

/// Plain data that may be copied to and from guest memory byte for byte.
///
/// # Safety
/// Every bit pattern must be a valid value and the type must have no
/// padding the guest could observe uninitialised.
pub unsafe trait Pod: Copy + Default + 'static {}

// SAFETY: integers accept every bit pattern and have no padding.
unsafe impl Pod for u8 {}
// SAFETY: as above.
unsafe impl Pod for u16 {}
// SAFETY: as above.
unsafe impl Pod for u32 {}
// SAFETY: as above.
unsafe impl Pod for u64 {}

/// A heap buffer reached only through raw pointers, so moving its owner
/// never invalidates the views' pointer (a `Box` move would).
struct HeapBacking(*mut [u8]);

// SAFETY: the buffer is plain bytes; access is synchronised by the views'
// contract, not by this owner.
unsafe impl Send for HeapBacking {}
// SAFETY: as above.
unsafe impl Sync for HeapBacking {}

impl Drop for HeapBacking {
    fn drop(&mut self) {
        // SAFETY: produced by `Box::into_raw` in `from_vec`, dropped once.
        drop(unsafe { Box::from_raw(self.0) });
    }
}

/// A cheap-to-clone window onto `[base, base + len)` of guest physical
/// space.
#[derive(Clone)]
pub struct GuestMemory {
    host: *mut u8,
    base: Gpa,
    len: u64,
    _owner: Arc<dyn Any + Send + Sync>,
}

impl std::fmt::Debug for GuestMemory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GuestMemory")
            .field("base", &format_args!("{:#x}", self.base))
            .field("len", &format_args!("{:#x}", self.len))
            .finish_non_exhaustive()
    }
}

// SAFETY: the pointer is only dereferenced through bounds-checked raw
// copies and atomics, which are sound from any thread; `_owner` keeps the
// backing alive for every clone.
unsafe impl Send for GuestMemory {}
// SAFETY: as above; no method hands out references into the backing.
unsafe impl Sync for GuestMemory {}

impl GuestMemory {
    /// A view of host memory the caller has mapped.
    ///
    /// # Safety
    /// `[host, host + len)` must be readable and writable for as long as
    /// `owner` is alive, and may only be replaced in place (same address,
    /// e.g. `mmap(MAP_FIXED)` during reclaim), never unmapped while
    /// `owner` lives.
    pub unsafe fn from_raw(
        host: *mut u8,
        base: Gpa,
        len: usize,
        owner: Arc<dyn Any + Send + Sync>,
    ) -> Self {
        Self {
            host,
            base,
            len: len as u64,
            _owner: owner,
        }
    }

    /// A view that owns its backing: guest RAM for unit tests.
    pub fn from_vec(base: Gpa, buf: Vec<u8>) -> Self {
        let owned = HeapBacking(Box::into_raw(buf.into_boxed_slice()));
        let (host, len) = (owned.0.cast::<u8>(), owned.0.len());
        // SAFETY: the allocation is owned by `HeapBacking`, freed only when
        // the last view drops it, and reached only through raw pointers.
        unsafe { Self::from_raw(host, base, len, Arc::new(owned)) }
    }

    pub fn base(&self) -> Gpa {
        self.base
    }

    pub fn len(&self) -> u64 {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Host pointer to `[gpa, gpa + len)` after a bounds check. Used for
    /// zero-copy I/O (`preadv` straight into a guest buffer); the caller
    /// must not keep it past the view's lifetime.
    pub fn host_ptr(&self, gpa: Gpa, len: u64) -> MemResult<*mut u8> {
        let off = gpa
            .checked_sub(self.base)
            .filter(|off| off.checked_add(len).is_some_and(|end| end <= self.len))
            .ok_or(MemError::OutOfRange { gpa, len })?;
        // SAFETY: `off + len <= self.len`, so the result stays inside the
        // allocation (or one past its end when `len == 0`).
        Ok(unsafe { self.host.add(off as usize) })
    }

    /// Copies guest memory at `gpa` into `buf`.
    pub fn read(&self, gpa: Gpa, buf: &mut [u8]) -> MemResult<()> {
        let src = self.host_ptr(gpa, buf.len() as u64)?;
        // SAFETY: `src` is valid for `buf.len()` bytes (checked above) and
        // cannot overlap a Rust-owned buffer.
        unsafe { std::ptr::copy_nonoverlapping(src, buf.as_mut_ptr(), buf.len()) };
        Ok(())
    }

    /// Copies `buf` into guest memory at `gpa`.
    pub fn write(&self, gpa: Gpa, buf: &[u8]) -> MemResult<()> {
        let dst = self.host_ptr(gpa, buf.len() as u64)?;
        // SAFETY: `dst` is valid for `buf.len()` bytes (checked above).
        unsafe { std::ptr::copy_nonoverlapping(buf.as_ptr(), dst, buf.len()) };
        Ok(())
    }

    /// Fills `[gpa, gpa + len)` with zeroes.
    pub fn zero(&self, gpa: Gpa, len: u64) -> MemResult<()> {
        let dst = self.host_ptr(gpa, len)?;
        // SAFETY: `dst` is valid for `len` bytes (checked above).
        unsafe { std::ptr::write_bytes(dst, 0, len as usize) };
        Ok(())
    }

    /// Reads a plain value (guest byte order is little-endian, as is the
    /// host's).
    pub fn read_obj<T: Pod>(&self, gpa: Gpa) -> MemResult<T> {
        let src = self.host_ptr(gpa, size_of::<T>() as u64)?;
        // SAFETY: bounds checked; `T: Pod` accepts any bytes; unaligned
        // read because guest structures need not be aligned for us.
        Ok(unsafe { src.cast::<T>().read_unaligned() })
    }

    pub fn write_obj<T: Pod>(&self, gpa: Gpa, value: T) -> MemResult<()> {
        let dst = self.host_ptr(gpa, size_of::<T>() as u64)?;
        // SAFETY: bounds checked; unaligned write of plain data.
        unsafe { dst.cast::<T>().write_unaligned(value) };
        Ok(())
    }

    fn atomic_u16(&self, gpa: Gpa) -> MemResult<&AtomicU16> {
        if !gpa.is_multiple_of(2) {
            return Err(MemError::Misaligned { gpa, align: 2 });
        }
        let p = self.host_ptr(gpa, 2)?;
        // SAFETY: in bounds and 2-byte aligned (the guest base is page
        // aligned); the reference lives only for one atomic operation.
        Ok(unsafe { AtomicU16::from_ptr(p.cast()) })
    }

    /// Acquire-load of a ring index the guest publishes (avail.idx).
    pub fn load_u16_acquire(&self, gpa: Gpa) -> MemResult<u16> {
        Ok(self.atomic_u16(gpa)?.load(Ordering::Acquire))
    }

    /// Release-store of a ring index the device publishes (used.idx),
    /// ordering every earlier write to the ring before it.
    pub fn store_u16_release(&self, gpa: Gpa, value: u16) -> MemResult<()> {
        self.atomic_u16(gpa)?.store(value, Ordering::Release);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounds_are_enforced() {
        let m = GuestMemory::from_vec(0x4000_0000, vec![0; 4096]);
        assert!(m.write(0x4000_0000, &[1, 2, 3]).is_ok());
        assert!(m.write(0x4000_0ffe, &[1, 2]).is_ok());
        assert_eq!(
            m.write(0x4000_0fff, &[1, 2]),
            Err(MemError::OutOfRange {
                gpa: 0x4000_0fff,
                len: 2
            })
        );
        assert!(m.read_obj::<u32>(0x3fff_fffe).is_err());
        assert!(m.host_ptr(u64::MAX, 2).is_err());
        assert!(m.host_ptr(0x4000_1000, 0).is_ok());
    }

    #[test]
    fn objects_round_trip_unaligned() {
        let m = GuestMemory::from_vec(0, vec![0; 64]);
        m.write_obj(3, 0x1122_3344_5566_7788u64).unwrap();
        assert_eq!(m.read_obj::<u64>(3).unwrap(), 0x1122_3344_5566_7788);
        let mut b = [0u8; 2];
        m.read(3, &mut b).unwrap();
        assert_eq!(b, [0x88, 0x77]);
        m.zero(3, 8).unwrap();
        assert_eq!(m.read_obj::<u64>(3).unwrap(), 0);
    }

    #[test]
    fn atomics_need_alignment() {
        let m = GuestMemory::from_vec(0x1000, vec![0; 16]);
        m.store_u16_release(0x1002, 7).unwrap();
        assert_eq!(m.load_u16_acquire(0x1002).unwrap(), 7);
        assert_eq!(m.read_obj::<u16>(0x1002).unwrap(), 7);
        assert!(matches!(
            m.load_u16_acquire(0x1001),
            Err(MemError::Misaligned { .. })
        ));
    }

    #[test]
    fn clones_share_backing() {
        let a = GuestMemory::from_vec(0, vec![0; 8]);
        let b = a.clone();
        drop(a);
        b.write_obj(0, 5u32).unwrap();
        assert_eq!(b.read_obj::<u32>(0).unwrap(), 5);
    }
}
