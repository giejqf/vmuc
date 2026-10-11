// SPDX-License-Identifier: Apache-2.0
//! Stage-2 release matrix: which host operations give back memory that a
//! vCPU dirtied through its stage-2 mapping.
//!
//! The memory manager rests on one claim (feasibility.md, from ArcBox's
//! `stage2_matrix` and mvm): only `hv_vm_unmap` → `mmap(MAP_FIXED)` over
//! the same host address → `hv_vm_map` lowers the process's
//! `phys_footprint` for guest-dirtied pages; `madvise` and a bare unmap
//! do not. A host-side `memset` would be the wrong experiment, so a real
//! vCPU dirties the pages here. Run after every macOS update.
//!
//! Footprint can lie. On macOS 26.6 a bare unmap/map of the same host
//! range drops `phys_footprint` by the full 64 MiB while every page still
//! holds the guest's data; the drop vanishes once the host touches the
//! pages. So the bare row reads the pages back before measuring, and the
//! real release must leave pages that read as zero.
//!
//! Needs the hypervisor entitlement and real hardware; passes with a note
//! where Hypervisor.framework is unavailable.

#![cfg(all(target_os = "macos", target_arch = "aarch64"))]
// Footprints are reported in MiB, where f64 has room to spare; every
// pointer cast is into page-aligned mmap memory at a page or word offset.
#![allow(clippy::cast_precision_loss, clippy::cast_ptr_alignment)]

use std::thread;

use vmuc_hv::{HvError, HvVcpu, HvVm, MemoryPermission, reg, sys_reg};

const CODE_IPA: u64 = 0x4000_0000;
const CODE_SIZE: usize = 0x4000;
const RAM_IPA: u64 = 0x4100_0000;
const RAM_SIZE: usize = 64 << 20;
const MIB: f64 = 1024.0 * 1024.0;

/// x0 = start, x1 = end, x2 = 4096: store to every 4 KiB page, then HVC.
const DIRTY_LOOP: [u32; 5] = [
    0xf900_0002, // str  x2, [x0]
    0x8b02_0000, // add  x0, x0, x2
    0xeb01_001f, // cmp  x0, x1
    0x54ff_ffa3, // b.lo -12
    0xd400_0002, // hvc  #0
];

fn footprint() -> u64 {
    // SAFETY: a zeroed rusage_info_v4 is a valid out-buffer for flavor V4.
    let mut info: libc::rusage_info_v4 = unsafe { std::mem::zeroed() };
    // SAFETY: valid pid and buffer of the flavor's type.
    let rc = unsafe {
        libc::proc_pid_rusage(libc::getpid(), libc::RUSAGE_INFO_V4, (&raw mut info).cast())
    };
    assert_eq!(rc, 0, "proc_pid_rusage");
    info.ri_phys_footprint
}

fn mmap_anon(len: usize) -> *mut u8 {
    // SAFETY: anonymous private mapping, checked below.
    let p = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            len,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_ANON | libc::MAP_PRIVATE,
            -1,
            0,
        )
    };
    assert_ne!(p, libc::MAP_FAILED);
    p.cast()
}

/// Runs the dirty loop over all of guest RAM on a fresh thread's vCPU.
fn dirty_from_vcpu() -> Result<(), HvError> {
    thread::spawn(|| -> Result<(), HvError> {
        let vcpu = HvVcpu::new()?;
        vcpu.set_sys_reg(sys_reg::HV_SYS_REG_MPIDR_EL1, 0)?;
        vcpu.set_reg(reg::HV_REG_PC, CODE_IPA)?;
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

struct Row {
    method: &'static str,
    released_mib: f64,
}

/// One method on a fresh 64 MiB mapping at its own IPA, so no method
/// inherits another's page state (MADV_FREE_REUSABLE marks are sticky).
fn measure(vm: &HvVm, method: &'static str, release: impl FnOnce(*mut u8)) -> Result<Row, HvError> {
    let ram = mmap_anon(RAM_SIZE);
    // SAFETY: the mapping outlives its VM mapping (unmapped below).
    unsafe { vm.map_memory(ram, RAM_IPA, RAM_SIZE, MemoryPermission::ALL)? };
    let start = footprint();
    dirty_from_vcpu()?;
    let dirtied = footprint();
    release(ram);
    let after = footprint();
    thread::sleep(std::time::Duration::from_millis(200));
    let settled = footprint();
    let mib = |a: u64, b: u64| (a as f64 - b as f64) / MIB;
    eprintln!(
        "{method:<36} dirtied {:6.1}  released {:6.1}  after 200 ms {:6.1} MiB",
        mib(dirtied, start),
        mib(dirtied, after),
        mib(dirtied, settled)
    );
    vm.unmap_memory(RAM_IPA, RAM_SIZE)?;
    // SAFETY: no longer mapped into the VM.
    unsafe { libc::munmap(ram.cast(), RAM_SIZE) };
    Ok(Row {
        method,
        released_mib: mib(dirtied, settled),
    })
}

fn refresh(ram: *mut u8) {
    HvVm::unmap(RAM_IPA, RAM_SIZE).unwrap();
    // SAFETY: replaces our own mapping in place; nothing maps it now.
    let p = unsafe {
        libc::mmap(
            ram.cast(),
            RAM_SIZE,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_FIXED | libc::MAP_ANON | libc::MAP_PRIVATE,
            -1,
            0,
        )
    };
    assert_eq!(p.cast::<u8>(), ram);
    // SAFETY: same host address as before, freshly mapped.
    unsafe { HvVm::remap(ram, RAM_IPA, RAM_SIZE, MemoryPermission::ALL).unwrap() };
}

fn matrix() -> Result<Vec<Row>, HvError> {
    let vm = HvVm::new()?;
    let code = mmap_anon(CODE_SIZE);
    for (i, insn) in DIRTY_LOOP.iter().enumerate() {
        // SAFETY: within the code page.
        unsafe { code.cast::<u32>().add(i).write(*insn) };
    }
    // SAFETY: the code page outlives the VM (unmapped after drop below).
    unsafe { vm.map_memory(code, CODE_IPA, CODE_SIZE, MemoryPermission::ALL)? };
    let rows = vec![
        measure(&vm, "nothing (control)", |_| {})?,
        // SAFETY (madvise rows): `ram` is a live mapping of RAM_SIZE bytes.
        measure(&vm, "madvise(MADV_DONTNEED)", |ram| unsafe {
            libc::madvise(ram.cast(), RAM_SIZE, libc::MADV_DONTNEED);
        })?,
        measure(&vm, "madvise(MADV_FREE_REUSABLE)", |ram| unsafe {
            libc::madvise(ram.cast(), RAM_SIZE, libc::MADV_FREE_REUSABLE);
        })?,
        measure(&vm, "hv_vm_unmap + hv_vm_map (same host)", |ram| {
            HvVm::unmap(RAM_IPA, RAM_SIZE).unwrap();
            // SAFETY: same host range, still mapped in this process.
            unsafe { HvVm::remap(ram, RAM_IPA, RAM_SIZE, MemoryPermission::ALL).unwrap() };
            // The guest wrote 4096 at the start of every page. Reading the
            // pages back also undoes the footprint drop a bare unmap/map
            // shows without releasing anything.
            // SAFETY: in bounds of the mapping.
            let kept = (0..RAM_SIZE / 4096)
                .filter(|i| unsafe { ram.add(i * 4096).cast::<u64>().read_volatile() } == 4096)
                .count();
            eprintln!(
                "  (guest data still present in {kept} of {} pages)",
                RAM_SIZE / 4096
            );
            assert_eq!(
                kept,
                RAM_SIZE / 4096,
                "a bare unmap/map must not lose guest data"
            );
        })?,
        measure(&vm, "unmap + mmap(MAP_FIXED) + map", refresh)?,
    ];

    // Released pages read as zero and are billed again when re-touched.
    let ram = mmap_anon(RAM_SIZE);
    // SAFETY: as in `measure`.
    unsafe { vm.map_memory(ram, RAM_IPA, RAM_SIZE, MemoryPermission::ALL)? };
    dirty_from_vcpu()?;
    refresh(ram);
    // SAFETY: in bounds of our mapping.
    assert_eq!(
        unsafe { ram.cast::<u64>().read_volatile() },
        0,
        "released pages read as zero"
    );
    let before = footprint();
    dirty_from_vcpu()?;
    let refault = (footprint() as f64 - before as f64) / MIB;
    eprintln!("guest re-dirties released RAM: billed {refault:.1} MiB again");
    vm.unmap_memory(RAM_IPA, RAM_SIZE)?;
    drop(vm);
    // SAFETY: the VM is gone; nothing maps these any more.
    unsafe {
        libc::munmap(ram.cast(), RAM_SIZE);
        libc::munmap(code.cast(), CODE_SIZE);
    }
    let mut rows = rows;
    rows.push(Row {
        method: "re-dirty after release",
        released_mib: -refault,
    });
    Ok(rows)
}

#[test]
fn only_unmap_refresh_remap_returns_guest_dirtied_memory() {
    let rows = match matrix() {
        Err(HvError::Unsupported | HvError::Denied) => {
            eprintln!("Hypervisor.framework unavailable here; stage-2 matrix skipped");
            return;
        }
        other => other.expect("matrix run failed"),
    };
    let got = |m: &str| rows.iter().find(|r| r.method == m).unwrap().released_mib;
    assert!(
        got("unmap + mmap(MAP_FIXED) + map") >= 0.9 * 64.0,
        "the release sequence vmuc relies on stopped working"
    );
    assert!(
        got("madvise(MADV_DONTNEED)") < 16.0,
        "MADV_DONTNEED started releasing guest-dirtied pages: revisit the memory manager"
    );
    assert!(
        got("hv_vm_unmap + hv_vm_map (same host)") < 16.0,
        "a bare unmap/map started releasing pages: revisit the memory manager"
    );
    assert!(
        -got("re-dirty after release") >= 0.9 * 64.0,
        "re-touched pages must be billed again"
    );
}
