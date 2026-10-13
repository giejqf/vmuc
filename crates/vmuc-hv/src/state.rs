// SPDX-License-Identifier: Apache-2.0
//! What a snapshot needs of the framework: a vCPU's registers (general,
//! SIMD/FP, system, the GIC CPU interface's, the vtimer), the GIC's
//! device state, and the host counter that the vtimer runs on.

use std::ffi::c_void;

use crate::error::{self, HvResult};
use crate::ffi;
use crate::{Gic, HvVcpu};

/// System registers a guest kernel owns: EL1 and EL0 state, the timers,
/// pointer authentication keys. Not the ID registers or MPIDR (the VMM's),
/// not the debug registers (trapped and emulated as RAZ/WI), not EL2's
/// (no nested virtualization). Order is restore order. A register this
/// host's framework does not have (SME before macOS 15.2) is skipped.
pub const SYS_REGS: &[u16] = &[
    0xc080, // SCTLR_EL1
    0xc081, // ACTLR_EL1
    0xc082, // CPACR_EL1
    0xc100, // TTBR0_EL1
    0xc101, // TTBR1_EL1
    0xc102, // TCR_EL1
    0xc108, // APIAKEYLO_EL1
    0xc109, // APIAKEYHI_EL1
    0xc10a, // APIBKEYLO_EL1
    0xc10b, // APIBKEYHI_EL1
    0xc110, // APDAKEYLO_EL1
    0xc111, // APDAKEYHI_EL1
    0xc112, // APDBKEYLO_EL1
    0xc113, // APDBKEYHI_EL1
    0xc118, // APGAKEYLO_EL1
    0xc119, // APGAKEYHI_EL1
    0xc200, // SPSR_EL1
    0xc201, // ELR_EL1
    0xc208, // SP_EL0
    0xe208, // SP_EL1
    0xc288, // AFSR0_EL1
    0xc289, // AFSR1_EL1
    0xc290, // ESR_EL1
    0xc300, // FAR_EL1
    0xc3a0, // PAR_EL1
    0xc510, // MAIR_EL1
    0xc518, // AMAIR_EL1
    0xc600, // VBAR_EL1
    0xc681, // CONTEXTIDR_EL1
    0xc684, // TPIDR_EL1
    0xc687, // SCXTNUM_EL1
    0xc708, // CNTKCTL_EL1
    0xd000, // CSSELR_EL1
    0xde82, // TPIDR_EL0
    0xde83, // TPIDRRO_EL0
    0xde85, // TPIDR2_EL0
    0xde87, // SCXTNUM_EL0
    0x8010, // MDCCINT_EL1
    0x8012, // MDSCR_EL1
    0xc094, // SMPRI_EL1
    0xc096, // SMCR_EL1
    0xdf11, // CNTP_CTL_EL0
    0xdf12, // CNTP_CVAL_EL0
    0xdf19, // CNTV_CTL_EL0
    0xdf1a, // CNTV_CVAL_EL0
];

/// GIC CPU interface registers (`hv_gic_icc_reg_t`), in restore order:
/// system register enable first, the priority state, then the group
/// enables. RPR is read-only and SRE_EL2 is EL2's.
pub const ICC_REGS: &[u16] = &[
    0xc665, // ICC_SRE_EL1
    0xc664, // ICC_CTLR_EL1
    0xc230, // ICC_PMR_EL1
    0xc643, // ICC_BPR0_EL1
    0xc663, // ICC_BPR1_EL1
    0xc644, // ICC_AP0R0_EL1
    0xc648, // ICC_AP1R0_EL1
    0xc666, // ICC_IGRPEN0_EL1
    0xc667, // ICC_IGRPEN1_EL1
];

/// X0-X30, PC, FPCR, FPSR, CPSR (`hv_reg_t` 0..=34).
const GP_REGS: u32 = 35;
/// Q0-Q31.
const SIMD_REGS: u32 = 32;

/// Everything a stopped vCPU holds that the guest can see.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VcpuState {
    /// `hv_reg_t` 0..=34.
    pub regs: Vec<u64>,
    pub simd: Vec<[u8; 16]>,
    /// The [`SYS_REGS`] this host has, as `(register, value)`.
    pub sys_regs: Vec<(u16, u64)>,
    /// [`ICC_REGS`], as `(register, value)`.
    pub icc_regs: Vec<(u16, u64)>,
    /// Host counter minus guest counter; see [`host_counter`].
    pub vtimer_offset: u64,
    pub vtimer_masked: bool,
}

impl HvVcpu {
    /// Reads the vCPU's state. On its own thread, outside `hv_vcpu_run`.
    pub fn capture(&self) -> HvResult<VcpuState> {
        let id = self.id();
        let mut s = VcpuState {
            regs: (0..GP_REGS)
                .map(|r| self.get_reg(r))
                .collect::<HvResult<_>>()?,
            vtimer_offset: self.vtimer_offset()?,
            vtimer_masked: self.vtimer_mask()?,
            ..VcpuState::default()
        };
        for r in 0..SIMD_REGS {
            let mut v = ffi::simd_from_bytes([0; 16]);
            // SAFETY: a valid out-pointer for this vCPU's thread.
            error::check(unsafe { ffi::hv_vcpu_get_simd_fp_reg(id, r, &raw mut v) })?;
            s.simd.push(ffi::simd_to_bytes(v));
        }
        for &r in SYS_REGS {
            if let Ok(v) = self.get_sys_reg(r) {
                s.sys_regs.push((r, v));
            }
        }
        for &r in ICC_REGS {
            let mut v = 0;
            // SAFETY: as above.
            error::check(unsafe { ffi::hv_gic_get_icc_reg(id, r, &raw mut v) })?;
            s.icc_regs.push((r, v));
        }
        Ok(s)
    }

    /// Writes a captured state back, with the vtimer offset given (the
    /// caller shifts it by the time the VM was not running). On the
    /// vCPU's own thread, after the GIC state, before it first runs.
    pub fn restore(&self, s: &VcpuState, vtimer_offset: u64) -> HvResult<()> {
        let id = self.id();
        self.set_vtimer_offset(vtimer_offset)?;
        for (r, &v) in (0..).zip(&s.regs) {
            self.set_reg(r, v)?;
        }
        for (r, &v) in (0..).zip(&s.simd) {
            // SAFETY: plain value for this vCPU's thread.
            error::check(unsafe { ffi::hv_vcpu_set_simd_fp_reg(id, r, ffi::simd_from_bytes(v)) })?;
        }
        for &(r, v) in &s.sys_regs {
            self.set_sys_reg(r, v)?;
        }
        for &(r, v) in &s.icc_regs {
            // SAFETY: as above.
            error::check(unsafe { ffi::hv_gic_set_icc_reg(id, r, v) })?;
        }
        self.set_vtimer_mask(s.vtimer_masked)
    }
}

impl Gic {
    /// The GIC's device state (distributor and redistributors; not the CPU
    /// interfaces, which [`HvVcpu::capture`] reads). Every vCPU must be
    /// out of `hv_vcpu_run`. The data is versioned by the framework.
    pub fn save_state() -> HvResult<Vec<u8>> {
        // SAFETY: returns a retained os_object or NULL.
        let st = unsafe { ffi::hv_gic_state_create() };
        if st.is_null() {
            return Err(crate::HvError::Error);
        }
        let mut size = 0;
        // SAFETY: `st` is live; valid out-pointer.
        let mut rc = error::check(unsafe { ffi::hv_gic_state_get_size(st, &raw mut size) });
        let mut data = vec![0u8; size];
        if rc.is_ok() {
            // SAFETY: `data` holds the `size` bytes the framework asked for.
            rc = error::check(unsafe {
                ffi::hv_gic_state_get_data(st, data.as_mut_ptr().cast::<c_void>())
            });
        }
        // SAFETY: returned retained, not used again.
        unsafe { ffi::os_release(st) };
        rc.map(|()| data)
    }

    /// Restores [`Gic::save_state`] data: after every vCPU exists, before
    /// any runs. Fails if a macOS update changed the format.
    pub fn restore_state(data: &[u8]) -> HvResult<()> {
        // SAFETY: `data` is valid for its length; the framework copies it.
        error::check(unsafe { ffi::hv_gic_set_state(data.as_ptr().cast(), data.len()) })
    }
}

/// The host counter the vtimer derives from (the guest sees this minus
/// its vtimer offset): `mach_absolute_time`, which on Apple silicon reads
/// the generic timer. It restarts at host boot, so offsets are shifted
/// with wrapping arithmetic.
pub fn host_counter() -> u64 {
    #[cfg(target_os = "macos")]
    {
        unsafe extern "C" {
            fn mach_absolute_time() -> u64;
        }
        // SAFETY: no arguments, no side effects.
        unsafe { mach_absolute_time() }
    }
    #[cfg(not(target_os = "macos"))]
    {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saved_registers_are_el1_or_el0_and_unique() {
        for regs in [SYS_REGS, ICC_REGS] {
            let mut sorted = regs.to_vec();
            sorted.sort_unstable();
            sorted.dedup();
            assert_eq!(sorted.len(), regs.len(), "a register listed twice");
        }
        for &r in SYS_REGS.iter().chain(ICC_REGS) {
            // op1 4 and 6 are EL2 and EL3 (SP_EL1 is the exception: it
            // is encoded with op1 4 but is EL1 state).
            let op1 = (r >> 11) & 7;
            assert!(
                op1 != 6 && (op1 != 4 || r == 0xe208),
                "{r:#x} is not EL1/EL0"
            );
        }
    }
}
