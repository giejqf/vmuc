// SPDX-License-Identifier: Apache-2.0
//! Guest RAM accounting: what `phys_footprint` bills when the host and a
//! vCPU both touch guest RAM, as every device does (a virtio-blk read is
//! a `preadv` into guest RAM that the guest then reads).
//!
//! Plain anonymous memory is billed twice for such pages; memory from
//! `hv_vm_allocate` once, because it maps ledger-tagged memory entries.
//! Those entries free a page only when the whole entry goes (madvise,
//! `MADV_ZERO` and overwriting part of an entry leave it billed), so
//! vmuc builds guest RAM from one entry per 128 KiB granule
//! ([`vmuc_hv::guest_memory`]) and releases whole granules. This probe
//! prints the table behind that (docs/experiments/2026-10-13-guest-ram-accounting.md)
//! and fails if what vmuc relies on stops holding. Run after every macOS
//! update:
//!
//! `cargo test -p vmuc-hv --test accounting -- --nocapture`

#![cfg(all(target_os = "macos", target_arch = "aarch64"))]
#![allow(clippy::cast_precision_loss, clippy::cast_ptr_alignment)]

use std::ffi::c_void;
use std::thread;

use vmuc_hv::guest_memory::{self, RELEASE_GRANULE};
use vmuc_hv::{HvError, HvVcpu, HvVm, MemoryPermission, reg, sys_reg};

const CODE_IPA: u64 = 0x4000_0000;
const CODE_SIZE: usize = 0x4000;
const RAM_IPA: u64 = 0x4100_0000;
const RAM_SIZE: usize = 64 << 20;
const MIB: f64 = 1024.0 * 1024.0;
/// The sub-range released in the middle of RAM.
const SUB_AT: usize = 24 << 20;
const SUB: usize = 16 << 20;

/// x0 = start, x1 = end, x2 = 4096: store to every 4 KiB page, then HVC.
const STORE_LOOP: [u32; 5] = [
    0xf900_0002,
    0x8b02_0000,
    0xeb01_001f,
    0x54ff_ffa3,
    0xd400_0002,
];
/// The same, loading (`ldr x3, [x0]`) instead of storing.
const LOAD_LOOP: [u32; 5] = [
    0xf940_0003,
    0x8b02_0000,
    0xeb01_001f,
    0x54ff_ffa3,
    0xd400_0002,
];
const LOAD_AT: usize = 0x100;

#[link(name = "Hypervisor", kind = "framework")]
unsafe extern "C" {
    fn hv_vm_allocate(uvap: *mut *mut c_void, size: usize, flags: u64) -> i32;
    fn hv_vm_deallocate(uva: *mut c_void, size: usize) -> i32;
}

fn footprint() -> u64 {
    // SAFETY: a zeroed rusage_info_v4 is a valid out-buffer for flavor V4.
    let mut info: libc::rusage_info_v4 = unsafe { std::mem::zeroed() };
    // SAFETY: valid pid and buffer of the flavor's type.
    let rc = unsafe {
        libc::proc_pid_rusage(libc::getpid(), libc::RUSAGE_INFO_V4, (&raw mut info).cast())
    };
    assert_eq!(rc, 0);
    info.ri_phys_footprint
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Alloc {
    Mmap,
    HvAllocate,
    Granules,
}

fn alloc(a: Alloc) -> *mut u8 {
    match a {
        Alloc::Mmap => {
            // SAFETY: anonymous private mapping, checked below.
            let p = unsafe {
                libc::mmap(
                    std::ptr::null_mut(),
                    RAM_SIZE,
                    libc::PROT_READ | libc::PROT_WRITE,
                    libc::MAP_ANON | libc::MAP_PRIVATE,
                    -1,
                    0,
                )
            };
            assert_ne!(p, libc::MAP_FAILED);
            p.cast()
        }
        Alloc::HvAllocate => {
            let mut p = std::ptr::null_mut();
            // SAFETY: out-pointer to a local; size is a page multiple.
            assert_eq!(unsafe { hv_vm_allocate(&raw mut p, RAM_SIZE, 0) }, 0);
            p.cast()
        }
        Alloc::Granules => guest_memory::allocate(RAM_SIZE).unwrap(),
    }
}

fn free(a: Alloc, p: *mut u8) {
    // SAFETY: `p` came from `alloc(a)` and nothing maps it any more.
    unsafe {
        if a == Alloc::HvAllocate {
            hv_vm_deallocate(p.cast(), RAM_SIZE);
        } else {
            libc::munmap(p.cast(), RAM_SIZE);
        }
    }
}

/// Runs the store or load loop over all of guest RAM on a fresh vCPU.
fn vcpu_touch(store: bool) -> Result<(), HvError> {
    thread::spawn(move || -> Result<(), HvError> {
        let vcpu = HvVcpu::new()?;
        vcpu.set_sys_reg(sys_reg::HV_SYS_REG_MPIDR_EL1, 0)?;
        let pc = CODE_IPA + if store { 0 } else { LOAD_AT as u64 };
        vcpu.set_reg(reg::HV_REG_PC, pc)?;
        vcpu.set_reg(reg::HV_REG_CPSR, 0x3c5)?;
        vcpu.set_reg(reg::HV_REG_X0, RAM_IPA)?;
        vcpu.set_reg(reg::HV_REG_X1, RAM_IPA + RAM_SIZE as u64)?;
        vcpu.set_reg(reg::HV_REG_X2, 4096)?;
        let exit = vcpu.run()?;
        assert!(format!("{exit:?}").contains("HypercallHvc"), "{exit:?}");
        Ok(())
    })
    .join()
    .expect("vCPU thread panicked")
}

fn host_store(ram: *mut u8) {
    for i in 0..RAM_SIZE / 4096 {
        // SAFETY: in bounds of the RAM_SIZE mapping.
        unsafe { ram.add(i * 4096).cast::<u64>().write_volatile(7) };
    }
}

fn mib_since(base: u64) -> f64 {
    (footprint() as f64 - base as f64) / MIB
}

/// Billed MiB once the host stores to every page and the guest loads
/// them (a device writes, the guest reads), or the guest stores and the
/// host loads (`guest_first`: the guest writes, a device reads).
fn shared_touch(vm: &HvVm, a: Alloc, guest_first: bool) -> Result<f64, HvError> {
    let ram = alloc(a);
    // SAFETY: the memory outlives its VM mapping (unmapped below).
    unsafe { vm.map_memory(ram, RAM_IPA, RAM_SIZE, MemoryPermission::ALL)? };
    let base = footprint();
    if guest_first {
        vcpu_touch(true)?;
        for i in 0..RAM_SIZE / 4096 {
            // SAFETY: in bounds of the RAM_SIZE mapping.
            unsafe { ram.add(i * 4096).cast::<u64>().read_volatile() };
        }
    } else {
        host_store(ram);
        vcpu_touch(false)?;
    }
    let billed = mib_since(base);
    vm.unmap_memory(RAM_IPA, RAM_SIZE)?;
    free(a, ram);
    Ok(billed)
}

/// MiB released, pages in the released range that read zero, and pages
/// elsewhere that kept the guest's data.
type Released = (f64, usize, usize);

/// A 16 MiB sub-range of guest-dirtied `a` memory after unmap + `method`
/// + map.
fn release_sub(vm: &HvVm, a: Alloc, method: impl Fn(*mut u8, usize)) -> Result<Released, HvError> {
    let ram = alloc(a);
    // SAFETY: the memory outlives its VM mapping (unmapped below).
    unsafe { vm.map_memory(ram, RAM_IPA, RAM_SIZE, MemoryPermission::ALL)? };
    vcpu_touch(true)?;
    let base = footprint();
    let ipa = RAM_IPA + SUB_AT as u64;
    HvVm::unmap(ipa, SUB)?;
    // SAFETY: inside our allocation.
    let sub = unsafe { ram.add(SUB_AT) };
    method(sub, SUB);
    // SAFETY: the same host address as before.
    unsafe { HvVm::remap(sub, ipa, SUB, MemoryPermission::ALL)? };
    thread::sleep(std::time::Duration::from_millis(100));
    let released = -mib_since(base);
    // SAFETY: in bounds; the guest stored 4096 at the start of each page.
    let page = |i: usize| unsafe { ram.add(i * 4096).cast::<u64>().read_volatile() };
    let inside = SUB_AT / 4096..(SUB_AT + SUB) / 4096;
    let zero = inside.clone().filter(|&i| page(i) == 0).count();
    let kept = (0..RAM_SIZE / 4096)
        .filter(|i| !inside.contains(i) && page(*i) == 4096)
        .count();
    vm.unmap_memory(RAM_IPA, RAM_SIZE)?;
    free(a, ram);
    Ok((released, zero, kept))
}

fn madvise(p: *mut u8, n: usize, advice: i32) {
    // SAFETY: a live sub-range of our mapping.
    unsafe { libc::madvise(p.cast(), n, advice) };
}

fn mmap_fixed(p: *mut u8, n: usize) {
    // SAFETY: replaces a sub-range of our own mapping; nothing maps it now.
    let q = unsafe {
        libc::mmap(
            p.cast(),
            n,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_FIXED | libc::MAP_ANON | libc::MAP_PRIVATE,
            -1,
            0,
        )
    };
    assert_eq!(q.cast::<u8>(), p);
}

fn refresh(p: *mut u8, n: usize) {
    // SAFETY: granule-aligned memory of ours, unmapped from the VM.
    unsafe { guest_memory::refresh(p, n).unwrap() };
}

/// Fresh tagged entries over the granule-aligned part of `[p, p + n)`.
fn refresh_aligned_part(p: *mut u8, n: usize) {
    let skip = (p as usize).next_multiple_of(RELEASE_GRANULE) - p as usize;
    // SAFETY: inside the range, which is ours and unmapped from the VM.
    refresh(unsafe { p.add(skip) }, n - RELEASE_GRANULE);
}

/// `MADV_ZERO` (macOS 15): zero pages without faulting more in.
const MADV_ZERO: i32 = 11;

struct Results {
    shared: Vec<(Alloc, f64, f64)>,
    releases: Vec<(&'static str, Released)>,
    refault: f64,
    /// Building 4 GiB of granules, and mapping it into the VM.
    build_4g: (std::time::Duration, std::time::Duration),
}

fn run() -> Result<Results, HvError> {
    let vm = HvVm::new()?;
    let code = alloc(Alloc::Mmap);
    // SAFETY: both loops fit in the first page of the code mapping, which
    // outlives the VM.
    unsafe {
        for (i, insn) in STORE_LOOP.iter().enumerate() {
            code.cast::<u32>().add(i).write(*insn);
        }
        for (i, insn) in LOAD_LOOP.iter().enumerate() {
            code.add(LOAD_AT).cast::<u32>().add(i).write(*insn);
        }
        vm.map_memory(code, CODE_IPA, CODE_SIZE, MemoryPermission::ALL)?;
    }
    let mut shared = Vec::new();
    for a in [Alloc::Mmap, Alloc::HvAllocate, Alloc::Granules] {
        shared.push((a, shared_touch(&vm, a, false)?, shared_touch(&vm, a, true)?));
    }
    let hv = Alloc::HvAllocate;
    let releases = vec![
        (
            "mmap: mmap(MAP_FIXED) (vmuc before)",
            release_sub(&vm, Alloc::Mmap, mmap_fixed)?,
        ),
        (
            "hv_vm_allocate: madvise(DONTNEED)",
            release_sub(&vm, hv, |p, n| madvise(p, n, libc::MADV_DONTNEED))?,
        ),
        (
            "hv_vm_allocate: madvise(FREE_REUSABLE)",
            release_sub(&vm, hv, |p, n| madvise(p, n, libc::MADV_FREE_REUSABLE))?,
        ),
        (
            "hv_vm_allocate: madvise(MADV_ZERO)",
            release_sub(&vm, hv, |p, n| madvise(p, n, MADV_ZERO))?,
        ),
        (
            "hv_vm_allocate: fresh tagged entries over it",
            release_sub(&vm, hv, refresh_aligned_part)?,
        ),
        (
            "granules: fresh granules (vmuc now)",
            release_sub(&vm, Alloc::Granules, refresh)?,
        ),
    ];
    // Released granules are billed honestly again: once, not twice.
    let ram = alloc(Alloc::Granules);
    // SAFETY: as in `shared_touch`.
    unsafe { vm.map_memory(ram, RAM_IPA, RAM_SIZE, MemoryPermission::ALL)? };
    vcpu_touch(true)?;
    HvVm::unmap(RAM_IPA, RAM_SIZE)?;
    refresh(ram, RAM_SIZE);
    // SAFETY: the same host address.
    unsafe { HvVm::remap(ram, RAM_IPA, RAM_SIZE, MemoryPermission::ALL)? };
    let base = footprint();
    host_store(ram);
    vcpu_touch(false)?;
    let refault = mib_since(base);
    vm.unmap_memory(RAM_IPA, RAM_SIZE)?;
    free(Alloc::Granules, ram);
    let big = 4usize << 30;
    let t = std::time::Instant::now();
    let ram = guest_memory::allocate(big).unwrap();
    let built = t.elapsed();
    let t = std::time::Instant::now();
    // SAFETY: unmapped and freed right below.
    unsafe { vm.map_memory(ram, 0x1_0000_0000, big, MemoryPermission::ALL)? };
    let build_4g = (built, t.elapsed());
    vm.unmap_memory(0x1_0000_0000, big)?;
    // SAFETY: nothing maps it any more.
    unsafe { libc::munmap(ram.cast(), big) };
    drop(vm);
    free(Alloc::Mmap, code);
    Ok(Results {
        shared,
        releases,
        refault,
        build_4g,
    })
}

#[test]
fn guest_ram_is_billed_once_and_granules_release() {
    let r = match run() {
        Err(HvError::Unsupported | HvError::Denied) => {
            eprintln!("Hypervisor.framework unavailable here; skipped");
            return;
        }
        other => other.expect("accounting run failed"),
    };
    eprintln!(
        "64 MiB touched by both: billed MiB, host store + guest load / guest store + host load"
    );
    for (a, hg, gh) in &r.shared {
        eprintln!("  {a:?}: {hg:.1} / {gh:.1}");
    }
    eprintln!("16 of 64 guest-dirtied MiB released: MiB released, zero /4096, kept /12288");
    for (name, (mib, zero, kept)) in &r.releases {
        eprintln!("  {name:<44} {mib:5.1}  {zero:4}  {kept:5}");
    }
    eprintln!(
        "released granules, host store + guest load again: billed {:.1}",
        r.refault
    );
    eprintln!(
        "4 GiB of granules: built in {:?}, hv_vm_map in {:?}",
        r.build_4g.0, r.build_4g.1
    );
    let shared = |a| {
        r.shared
            .iter()
            .find(|(x, ..)| *x == a)
            .map_or(0.0, |s| s.1.max(s.2))
    };
    assert!(
        shared(Alloc::Granules) < 1.1 * 64.0,
        "granules are billed twice for pages host and guest share"
    );
    let (mib, zero, kept) = r.releases.last().map(|x| x.1).unwrap_or_default();
    assert!(
        mib >= 0.9 * 16.0,
        "releasing granules stopped freeing memory"
    );
    assert_eq!((zero, kept), (SUB / 4096, (RAM_SIZE - SUB) / 4096));
    assert!(r.refault < 1.1 * 64.0, "released granules are billed twice");
    assert!(RELEASE_GRANULE.is_power_of_two());
}
