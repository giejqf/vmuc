// SPDX-License-Identifier: Apache-2.0
//! Guest RAM built the way `hv_vm_allocate` builds it, in small pieces.
//!
//! macOS bills a page of plain anonymous memory twice in `phys_footprint`
//! once both the VMM (a device writing guest RAM) and a vCPU (through
//! stage 2) have touched it. `hv_vm_allocate` avoids that by mapping
//! named, ledger-tagged memory entries (`MAP_MEM_NAMED_CREATE |
//! MAP_MEM_LEDGER_TAGGED`, 128 MiB each, as disassembled on macOS 26.6),
//! whose pages are billed once. But a page of such an entry is freed only
//! when the whole entry goes: `madvise`, `MADV_ZERO` and overwriting part
//! of the entry all leave it billed. So guest RAM here is one entry per
//! [`RELEASE_GRANULE`], and a release replaces whole granules with fresh
//! entries (docs/experiments/2026-10-13-guest-ram-accounting.md,
//! `tests/accounting.rs`).
//!
//! Off macOS (checks and unit tests) both are plain anonymous mappings.

use std::io;

/// The unit guest memory is billed and released in: 128 KiB, the size of
/// the smallest block the guest reports free (order 5 of 4 KiB pages).
pub const RELEASE_GRANULE: usize = 128 << 10;

/// Reserves `len` bytes (a multiple of the 16 KiB host page), aligned to
/// [`RELEASE_GRANULE`], and backs them with fresh granules. Untouched
/// pages cost nothing. Free it with `munmap`.
pub fn allocate(len: usize) -> io::Result<*mut u8> {
    let base = imp::reserve(len)?;
    // SAFETY: the reservation is ours and nothing uses it yet.
    if let Err(e) = unsafe { refresh(base, len) } {
        // SAFETY: undoing our own reservation.
        unsafe { libc::munmap(base.cast(), len) };
        return Err(e);
    }
    Ok(base)
}

/// Replaces `[addr, addr + len)` with fresh granules: the pages there are
/// freed, are billed no more, and read as zero.
///
/// # Safety
/// The range lies in memory from [`allocate`], starts on a granule
/// boundary, and nothing may touch it during the call (for guest RAM: it
/// is unmapped from the VM).
pub unsafe fn refresh(addr: *mut u8, len: usize) -> io::Result<()> {
    if !(addr as usize).is_multiple_of(RELEASE_GRANULE) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{addr:p} is not on a {RELEASE_GRANULE}-byte granule"),
        ));
    }
    let mut off = 0;
    while off < len {
        let n = RELEASE_GRANULE.min(len - off);
        // SAFETY: within the caller's range, per the contract.
        unsafe { imp::granule(addr.add(off), n)? };
        off += n;
    }
    Ok(())
}

#[cfg(target_os = "macos")]
mod imp {
    use std::io;

    use super::RELEASE_GRANULE;

    const VM_FLAGS_ANYWHERE: i32 = 0x1;
    const VM_FLAGS_OVERWRITE: i32 = 0x4000;
    const MAP_MEM_LEDGER_TAGGED: i32 = 0x2000;
    const MAP_MEM_NAMED_CREATE: i32 = 0x2_0000;
    const VM_PROT_NONE: i32 = 0;
    const VM_PROT_RW: i32 = 0x3;
    const VM_PROT_ALL: i32 = 0x7;
    const VM_INHERIT_COPY: u32 = 1;

    unsafe extern "C" {
        static mach_task_self_: u32;
        fn mach_make_memory_entry_64(
            task: u32,
            size: *mut u64,
            offset: u64,
            permission: i32,
            handle: *mut u32,
            parent: u32,
        ) -> i32;
        fn mach_vm_map(
            task: u32,
            address: *mut u64,
            size: u64,
            mask: u64,
            flags: i32,
            object: u32,
            offset: u64,
            copy: i32,
            cur: i32,
            max: i32,
            inheritance: u32,
        ) -> i32;
        fn mach_port_deallocate(task: u32, name: u32) -> i32;
    }

    fn kr(what: &str, kr: i32) -> io::Result<()> {
        if kr == 0 {
            Ok(())
        } else {
            Err(io::Error::other(format!("{what}: kern_return {kr}")))
        }
    }

    pub fn reserve(len: usize) -> io::Result<*mut u8> {
        let mut at = 0u64;
        // SAFETY: an inaccessible reservation anywhere in our own task.
        let r = unsafe {
            mach_vm_map(
                mach_task_self_,
                &raw mut at,
                len as u64,
                RELEASE_GRANULE as u64 - 1,
                VM_FLAGS_ANYWHERE,
                0,
                0,
                0,
                VM_PROT_NONE,
                VM_PROT_NONE,
                VM_INHERIT_COPY,
            )
        };
        kr("reserving guest RAM", r)?;
        Ok(at as *mut u8)
    }

    /// One fresh ledger-tagged entry mapped over `[addr, addr + len)`.
    pub unsafe fn granule(addr: *mut u8, len: usize) -> io::Result<()> {
        let (mut size, mut entry) = (len as u64, 0u32);
        // SAFETY: plain Mach calls on our own task with local out-params.
        unsafe {
            kr(
                "mach_make_memory_entry_64",
                mach_make_memory_entry_64(
                    mach_task_self_,
                    &raw mut size,
                    0,
                    MAP_MEM_NAMED_CREATE | MAP_MEM_LEDGER_TAGGED | VM_PROT_ALL,
                    &raw mut entry,
                    0,
                ),
            )?;
            let mut at = addr as u64;
            // The caller owns the range, so replacing whatever is there
            // (VM_FLAGS_OVERWRITE at a fixed address) is what it asked for.
            let r = mach_vm_map(
                mach_task_self_,
                &raw mut at,
                len as u64,
                0,
                VM_FLAGS_OVERWRITE,
                entry,
                0,
                0,
                VM_PROT_RW,
                VM_PROT_ALL,
                VM_INHERIT_COPY,
            );
            // The mapping holds its own reference to the entry.
            mach_port_deallocate(mach_task_self_, entry);
            kr("mach_vm_map", r)
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    use std::io;

    pub fn reserve(len: usize) -> io::Result<*mut u8> {
        // SAFETY: anonymous private mapping at a kernel-chosen address.
        let p = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len + super::RELEASE_GRANULE,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_ANON | libc::MAP_PRIVATE,
                -1,
                0,
            )
        };
        if p == libc::MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        // Align like the macOS reservation (the slack stays mapped).
        let aligned = (p as usize).next_multiple_of(super::RELEASE_GRANULE);
        Ok(aligned as *mut u8)
    }

    pub unsafe fn granule(addr: *mut u8, len: usize) -> io::Result<()> {
        // SAFETY: the caller owns the range.
        let p = unsafe {
            libc::mmap(
                addr.cast(),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_FIXED | libc::MAP_ANON | libc::MAP_PRIVATE,
                -1,
                0,
            )
        };
        if p == libc::MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refresh_zeroes_whole_granules_and_keeps_the_rest() {
        let len = 4 * RELEASE_GRANULE + (16 << 10);
        let p = allocate(len).unwrap();
        assert!((p as usize).is_multiple_of(RELEASE_GRANULE));
        // SAFETY: in bounds of our allocation throughout.
        unsafe {
            for off in (0..len).step_by(4096) {
                p.add(off).write(7);
            }
            refresh(p.add(RELEASE_GRANULE), 2 * RELEASE_GRANULE).unwrap();
            for off in (0..len).step_by(4096) {
                let inside = (RELEASE_GRANULE..3 * RELEASE_GRANULE).contains(&off);
                assert_eq!(p.add(off).read(), if inside { 0 } else { 7 }, "{off:#x}");
            }
            assert!(refresh(p.add(4096), RELEASE_GRANULE).is_err());
        }
    }
}
