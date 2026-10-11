// SPDX-License-Identifier: Apache-2.0
//! What does one trip out of the guest and back cost?
//!
//! A vCPU loops on an MMIO load (a data abort the VMM retires by moving PC
//! on) and then on HVC (which the framework retires), with the in-kernel
//! GIC present as in a real VM. Each virtio round trip costs the guest
//! several such exits (QueueNotify, InterruptStatus, InterruptACK), so
//! this is the floor under every device's latency
//! (docs/experiments/2026-10-12-exit-cost.md).

#![cfg(all(target_os = "macos", target_arch = "aarch64"))]
#![allow(clippy::cast_precision_loss)]

use std::thread;
use std::time::{Duration, Instant};

use vmuc_hv::{
    ExceptionClass, Gic, GicConfig, HvError, HvVcpu, HvVm, MemoryPermission, VcpuExit, reg, sys_reg,
};

const CODE_IPA: u64 = 0x4000_0000;
/// Not mapped: every load from it exits.
const MMIO_IPA: u64 = 0x0a00_0000;
const EXITS: u32 = 200_000;

/// `ldr w1, [x0]; b .-4`
const MMIO_LOOP: [u32; 2] = [0xb940_0001, 0x17ff_ffff];
/// `hvc #0; b .-4`
const HVC_LOOP: [u32; 2] = [0xd400_0002, 0x17ff_ffff];

/// Runs `EXITS` exits of the loop at `pc`; returns the mean round trip.
fn round_trip(pc: u64) -> Result<Duration, HvError> {
    thread::spawn(move || -> Result<Duration, HvError> {
        let vcpu = HvVcpu::new()?;
        vcpu.set_sys_reg(sys_reg::HV_SYS_REG_MPIDR_EL1, 0)?;
        vcpu.set_reg(reg::HV_REG_PC, pc)?;
        vcpu.set_reg(reg::HV_REG_CPSR, 0x3c5)?;
        vcpu.set_reg(reg::HV_REG_X0, MMIO_IPA)?;
        let t = Instant::now();
        for _ in 0..EXITS {
            match vcpu.run()? {
                VcpuExit::Exception {
                    class: ExceptionClass::DataAbort(_),
                    ..
                } => {
                    let pc = vcpu.get_reg(reg::HV_REG_PC)?;
                    vcpu.set_reg(reg::HV_REG_PC, pc + 4)?;
                }
                VcpuExit::Exception {
                    class: ExceptionClass::HypercallHvc(_),
                    ..
                } => {}
                other => panic!("unexpected exit {other:?}"),
            }
        }
        Ok(t.elapsed() / EXITS)
    })
    .join()
    .expect("vCPU thread panicked")
}

#[test]
fn exit_round_trips_are_microseconds() {
    let vm = match HvVm::new() {
        Err(HvError::Unsupported | HvError::Denied) => {
            eprintln!("Hypervisor.framework unavailable here; exit probe skipped");
            return;
        }
        other => other.unwrap(),
    };
    let _gic = Gic::new(GicConfig {
        distributor_base: 0x0800_0000,
        redistributor_base: 0x0c00_0000,
    })
    .unwrap();
    // SAFETY: fresh anonymous mapping, checked below.
    let code = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            0x4000,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_ANON | libc::MAP_PRIVATE,
            -1,
            0,
        )
    };
    assert_ne!(code, libc::MAP_FAILED);
    for (i, insn) in MMIO_LOOP.iter().chain(&HVC_LOOP).enumerate() {
        // SAFETY: within the code page.
        unsafe { code.cast::<u32>().add(i).write(*insn) };
    }
    // SAFETY: the mapping outlives the VM.
    unsafe {
        vm.map_memory(code.cast(), CODE_IPA, 0x4000, MemoryPermission::ALL)
            .unwrap();
    }
    let mmio = round_trip(CODE_IPA).unwrap();
    let hvc = round_trip(CODE_IPA + 8).unwrap();
    eprintln!("MMIO load exit: {mmio:?} per round trip; HVC: {hvc:?}");
    assert!(mmio < Duration::from_micros(100), "{mmio:?}");
    drop(vm);
    // SAFETY: the VM is gone.
    unsafe { libc::munmap(code, 0x4000) };
}
