// SPDX-License-Identifier: Apache-2.0
//! E5's first question: does a vCPU's and the GIC's state survive a
//! capture and a restore, into the same VM and into a new one, and can
//! the guest's counter be made to continue where it stopped?
//!
//! A vCPU gets distinctive values (a general register, a Q register,
//! TPIDR_EL0), is captured, clobbered and restored, and then runs code
//! that reads them back along with CNTVCT_EL0. The GIC's state is taken
//! from one VM and given to the next.

#![cfg(all(target_os = "macos", target_arch = "aarch64"))]
#![allow(clippy::cast_precision_loss)]

use std::time::Duration;

use vmuc_hv::state::{SYS_REGS, host_counter};
use vmuc_hv::{
    ExceptionClass, Gic, GicConfig, HvError, HvVcpu, HvVm, MemoryPermission, VcpuExit, reg,
};

const CODE_IPA: u64 = 0x4000_0000;
const TPIDR_EL0: u16 = 0xde82;
const CPACR_EL1: u16 = 0xc082;

const READ_BACK: [u32; 4] = [
    0xd53b_d040, // mrs  x0, tpidr_el0
    0x9e66_0061, // fmov x1, d3
    0xd53b_e042, // mrs  x2, cntvct_el0
    0xd400_0002, // hvc  #0
];

struct Vm {
    _gic: Gic,
    hv: HvVm,
    code: *mut libc::c_void,
}

impl Vm {
    fn new() -> Option<Self> {
        let vm = match HvVm::new() {
            Err(HvError::Unsupported | HvError::Denied) => return None,
            other => other.unwrap(),
        };
        let gic = Gic::new(GicConfig {
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
        for (i, insn) in READ_BACK.iter().enumerate() {
            // SAFETY: within the code page.
            unsafe { code.cast::<u32>().add(i).write(*insn) };
        }
        // SAFETY: the mapping outlives the VM (dropped after it).
        unsafe {
            vm.map_memory(code.cast(), CODE_IPA, 0x4000, MemoryPermission::ALL)
                .unwrap();
        }
        Some(Self {
            _gic: gic,
            hv: vm,
            code,
        })
    }
}

impl Drop for Vm {
    fn drop(&mut self) {
        let _ = self.hv.unmap_memory(CODE_IPA, 0x4000);
        // SAFETY: no longer mapped into the VM.
        unsafe { libc::munmap(self.code, 0x4000) };
    }
}

/// Runs the read-back code; returns (x0, x1, x2).
fn read_back(vcpu: &HvVcpu) -> (u64, u64, u64) {
    vcpu.set_reg(reg::HV_REG_PC, CODE_IPA).unwrap();
    vcpu.set_reg(reg::HV_REG_CPSR, 0x3c5).unwrap();
    match vcpu.run().unwrap() {
        VcpuExit::Exception {
            class: ExceptionClass::HypercallHvc(_),
            ..
        } => {}
        other => panic!("unexpected exit {other:?}"),
    }
    let r = |x| vcpu.get_reg(x).unwrap();
    (r(reg::HV_REG_X0), r(reg::HV_REG_X1), r(reg::HV_REG_X2))
}

#[test]
fn vcpu_and_gic_state_survive_a_new_vm() {
    let Some(vm) = Vm::new() else {
        eprintln!("Hypervisor.framework unavailable here; snapshot probe skipped");
        return;
    };
    // Phase 1: capture, clobber, restore into the same vCPU.
    let (state, gic, counter_at_capture) = std::thread::spawn(|| {
        let vcpu = HvVcpu::new().unwrap();
        vcpu.set_sys_reg(CPACR_EL1, 3 << 20).unwrap();
        vcpu.set_sys_reg(TPIDR_EL0, 0x5151).unwrap();
        vcpu.set_reg(reg::HV_REG_X9, 0x99).unwrap();
        set_q3(&vcpu, [7; 16]);
        let s = vcpu.capture().unwrap();
        let at = host_counter();
        assert_eq!(s.regs[9], 0x99);
        assert_eq!(s.simd[3], [7; 16]);
        assert!(s.sys_regs.contains(&(TPIDR_EL0, 0x5151)));
        eprintln!(
            "captured {} of {} system registers, {} GIC CPU registers",
            s.sys_regs.len(),
            SYS_REGS.len(),
            s.icc_regs.len()
        );
        let gic = Gic::save_state().unwrap();

        vcpu.set_sys_reg(TPIDR_EL0, 0).unwrap();
        vcpu.set_reg(reg::HV_REG_X9, 0).unwrap();
        set_q3(&vcpu, [0; 16]);
        vcpu.restore(&s, s.vtimer_offset).unwrap();
        assert_eq!(vcpu.capture().unwrap(), s, "restore then capture differs");
        let (x0, x1, _) = read_back(&vcpu);
        assert_eq!((x0, x1), (0x5151, 0x0707_0707_0707_0707));
        (s, gic, at)
    })
    .join()
    .unwrap();
    eprintln!("GIC state: {} bytes", gic.len());
    drop(vm);

    // Phase 2: a new VM, 50 ms later; the guest's counter continues from
    // the capture.
    std::thread::sleep(Duration::from_millis(50));
    let vm = Vm::new().unwrap();
    std::thread::spawn(move || {
        let vcpu = HvVcpu::new().unwrap();
        Gic::restore_state(&gic).unwrap();
        let shift = host_counter().wrapping_sub(counter_at_capture);
        vcpu.restore(&state, state.vtimer_offset.wrapping_add(shift))
            .unwrap();
        let (x0, x1, cntvct) = read_back(&vcpu);
        assert_eq!((x0, x1), (0x5151, 0x0707_0707_0707_0707));
        let at_capture = counter_at_capture.wrapping_sub(state.vtimer_offset);
        let gap_ms = cntvct.wrapping_sub(at_capture) as f64 / 24e3;
        eprintln!("guest counter moved {gap_ms:.3} ms across a 50 ms gap");
        assert!(gap_ms < 5.0, "the guest saw the gap: {gap_ms} ms");
    })
    .join()
    .unwrap();
    drop(vm);
}

fn set_q3(vcpu: &HvVcpu, v: [u8; 16]) {
    let mut s = vcpu.capture().unwrap();
    s.simd[3] = v;
    vcpu.restore(&s, s.vtimer_offset).unwrap();
}
