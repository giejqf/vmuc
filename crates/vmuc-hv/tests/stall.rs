// SPDX-License-Identifier: Apache-2.0
//! Does releasing memory stall vCPUs that never touch it?
//!
//! A vCPU spins reading the virtual counter and keeps the largest gap
//! between two reads; meanwhile a host thread runs the reclaim sequence
//! (unmap, fresh granules, map) over a different range, as the balloon worker
//! does. The plan's target is no reclaim-induced stall above 5 ms; on an
//! M4 Pro the worst gap rises from ~27 µs to ~40 µs while ~140k releases
//! a second run (docs/experiments/2026-10-11-reclaim-stall.md).

#![cfg(all(target_os = "macos", target_arch = "aarch64"))]
#![allow(clippy::cast_precision_loss, clippy::cast_ptr_alignment)]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use vmuc_hv::guest_memory;
use vmuc_hv::{HvError, HvVcpu, HvVm, MemoryPermission, reg, sys_reg};

const CODE_IPA: u64 = 0x4000_0000;
const RAM_IPA: u64 = 0x4100_0000;
const RAM_SIZE: usize = 64 << 20;
/// The architected counter on Apple silicon ticks at 24 MHz.
const TICK_NS: f64 = 1e9 / 24e6;

/// x5 = iterations; returns the largest counter delta in x4.
const SPIN: [u32; 9] = [
    0xd53b_e042, // mrs  x2, cntvct_el0
    0xd53b_e043, // mrs  x3, cntvct_el0      <- loop
    0xcb02_0066, // sub  x6, x3, x2
    0xeb04_00df, // cmp  x6, x4
    0x9a84_80c4, // csel x4, x6, x4, hi
    0xaa03_03e2, // mov  x2, x3
    0xf100_04a5, // subs x5, x5, #1
    0x54ff_ff41, // b.ne loop
    0xd400_0002, // hvc  #0
];

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

/// Spins for `iters` iterations; returns (max gap µs, wall time).
fn spin(iters: u64) -> Result<(f64, Duration), HvError> {
    thread::spawn(move || -> Result<(f64, Duration), HvError> {
        let vcpu = HvVcpu::new()?;
        vcpu.set_sys_reg(sys_reg::HV_SYS_REG_MPIDR_EL1, 0)?;
        vcpu.set_reg(reg::HV_REG_PC, CODE_IPA)?;
        vcpu.set_reg(reg::HV_REG_CPSR, 0x3c5)?;
        vcpu.set_reg(reg::HV_REG_X4, 0)?;
        vcpu.set_reg(reg::HV_REG_X5, iters)?;
        let t = Instant::now();
        let exit = vcpu.run()?;
        assert!(format!("{exit:?}").contains("HypercallHvc"), "{exit:?}");
        let max = vcpu.get_reg(reg::HV_REG_X4)? as f64 * TICK_NS / 1000.0;
        Ok((max, t.elapsed()))
    })
    .join()
    .expect("vCPU thread panicked")
}

/// Unmap/refresh/map `chunk`-sized pieces of RAM in a loop until `stop`.
fn churn(ram: usize, chunk: usize, stop: &AtomicBool) -> u64 {
    let ram = ram as *mut u8;
    let mut ops = 0;
    while !stop.load(Ordering::Relaxed) {
        for off in (0..RAM_SIZE).step_by(chunk) {
            // SAFETY: inside our mapping; the guest never touches it.
            unsafe {
                // Dirty every host page so each release has work to do.
                for page in (0..chunk).step_by(16 << 10) {
                    ram.add(off + page).write_volatile(1);
                }
                HvVm::unmap(RAM_IPA + off as u64, chunk).unwrap();
                guest_memory::refresh(ram.add(off), chunk).unwrap();
                HvVm::remap(
                    ram.add(off),
                    RAM_IPA + off as u64,
                    chunk,
                    MemoryPermission::ALL,
                )
                .unwrap();
            }
            ops += 1;
        }
    }
    ops
}

#[test]
fn reclaim_does_not_stall_unrelated_vcpus_long() {
    let vm = match HvVm::new() {
        Err(HvError::Unsupported | HvError::Denied) => {
            eprintln!("Hypervisor.framework unavailable here; stall probe skipped");
            return;
        }
        other => other.unwrap(),
    };
    let code = mmap_anon(0x4000);
    for (i, insn) in SPIN.iter().enumerate() {
        // SAFETY: within the code page.
        unsafe { code.cast::<u32>().add(i).write(*insn) };
    }
    let ram = guest_memory::allocate(RAM_SIZE).unwrap();
    // SAFETY: both mappings outlive the VM.
    unsafe {
        vm.map_memory(code, CODE_IPA, 0x4000, MemoryPermission::ALL)
            .unwrap();
        vm.map_memory(ram, RAM_IPA, RAM_SIZE, MemoryPermission::ALL)
            .unwrap();
    }
    let iters = 4_000_000_000;
    let (quiet, wall) = spin(iters).unwrap();
    eprintln!("quiet:                    max gap {quiet:8.1} us over {wall:?}");
    for chunk in [128 << 10, 2 << 20] {
        let stop = Arc::new(AtomicBool::new(false));
        let s = Arc::clone(&stop);
        let ram_addr = ram as usize;
        let churner = thread::spawn(move || churn(ram_addr, chunk, &s));
        let (busy, wall) = spin(iters).unwrap();
        stop.store(true, Ordering::Relaxed);
        let ops = churner.join().unwrap();
        eprintln!(
            "reclaiming {:>4} KiB ranges: max gap {busy:8.1} us over {wall:?} ({ops} releases, {:.0} us each)",
            chunk >> 10,
            wall.as_micros() as f64 / ops as f64
        );
        assert!(
            busy < 5000.0,
            "reclaim stalled an unrelated vCPU for {busy:.0} us"
        );
    }
    drop(vm);
    // SAFETY: the VM is gone.
    unsafe {
        libc::munmap(ram.cast(), RAM_SIZE);
        libc::munmap(code.cast(), 0x4000);
    }
}
