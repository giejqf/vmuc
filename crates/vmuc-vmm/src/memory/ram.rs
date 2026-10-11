// SPDX-License-Identifier: Apache-2.0
//! Guest RAM: one anonymous private mapping, never pre-touched, mapped
//! into the VM once. The host pays only for pages the guest touches.

use std::io;
use std::sync::Arc;

use vmuc_hv::{HOST_PAGE_SIZE, HvResult, HvVm, MemoryPermission};
use vmuc_virtio::GuestMemory;

/// Owns the host mapping; unmapped when the last view is gone.
struct Mapping {
    ptr: *mut u8,
    len: usize,
}

// SAFETY: the mapping is plain memory; concurrent access goes through
// GuestMemory's raw-pointer accessors.
unsafe impl Send for Mapping {}
// SAFETY: as above.
unsafe impl Sync for Mapping {}

impl Drop for Mapping {
    fn drop(&mut self) {
        // SAFETY: `ptr`/`len` came from a successful mmap and nothing maps
        // them into the VM any more (the VM is destroyed before RAM drops).
        unsafe { libc::munmap(self.ptr.cast(), self.len) };
    }
}

pub struct GuestRam {
    base: u64,
    mapping: Arc<Mapping>,
}

impl std::fmt::Debug for GuestRam {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GuestRam")
            .field("base", &format_args!("{:#x}", self.base))
            .field("size", &format_args!("{:#x}", self.mapping.len))
            .finish()
    }
}

impl GuestRam {
    /// Reserves `size` bytes (a multiple of the 16 KiB host page).
    pub fn new(base: u64, size: u64) -> io::Result<Self> {
        let len = usize::try_from(size).map_err(io::Error::other)?;
        if len == 0 || !len.is_multiple_of(HOST_PAGE_SIZE) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("guest RAM size {size:#x} is not a multiple of 16 KiB"),
            ));
        }
        // SAFETY: anonymous private mapping at a kernel-chosen address.
        let ptr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_ANON | libc::MAP_PRIVATE,
                -1,
                0,
            )
        };
        if ptr == libc::MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            base,
            mapping: Arc::new(Mapping {
                ptr: ptr.cast(),
                len,
            }),
        })
    }

    pub fn base(&self) -> u64 {
        self.base
    }

    pub fn size(&self) -> u64 {
        self.mapping.len as u64
    }

    /// Start of the host mapping, for the reclaimer.
    pub(crate) fn host_ptr(&self) -> *mut u8 {
        self.mapping.ptr
    }

    /// A view for loaders and device models; keeps the mapping alive.
    pub fn memory(&self) -> GuestMemory {
        // SAFETY: the mapping lives as long as the Arc in the view and is
        // only ever replaced in place (reclaim), never unmapped early.
        unsafe {
            GuestMemory::from_raw(
                self.mapping.ptr,
                self.base,
                self.mapping.len,
                Arc::clone(&self.mapping) as _,
            )
        }
    }

    /// Maps the whole region into the VM, read/write/execute.
    pub fn map_into(&self, vm: &HvVm) -> HvResult<()> {
        // SAFETY: the mapping outlives the VM: `Vm` destroys the VM before
        // dropping its `GuestRam`, and views keep the mapping alive anyway.
        unsafe {
            vm.map_memory(
                self.mapping.ptr,
                self.base,
                self.mapping.len,
                MemoryPermission::ALL,
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unaligned_sizes_and_starts_zeroed() {
        assert!(GuestRam::new(0x4000_0000, 4096).is_err());
        assert!(GuestRam::new(0x4000_0000, 0).is_err());
        let ram = GuestRam::new(0x4000_0000, 1 << 20).unwrap();
        let mem = ram.memory();
        assert_eq!(mem.read_obj::<u64>(0x4000_0000 + 4096).unwrap(), 0);
        mem.write_obj(0x4000_0000, 9u32).unwrap();
        drop(ram);
        assert_eq!(mem.read_obj::<u32>(0x4000_0000).unwrap(), 9);
    }
}
