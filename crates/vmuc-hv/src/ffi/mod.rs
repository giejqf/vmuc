// SPDX-License-Identifier: Apache-2.0
//! Raw FFI surface for Hypervisor.framework (Apple silicon).
//!
//! Types and constants are shared by both backends. The functions come
//! from `darwin.rs` (the real `extern "C"` block) on `aarch64-apple-darwin`
//! and from `stub.rs` everywhere else. The two must stay signature-identical.
//!
//! Derived from ArcBox `virt/arcbox-hv/src/ffi.rs` (MIT OR Apache-2.0).

#![allow(non_camel_case_types, dead_code)]

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
mod darwin;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
pub use darwin::*;

#[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
mod stub;
#[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
pub use stub::*;

pub type hv_return_t = i32;
pub type hv_vcpu_t = u64;
pub type hv_vcpu_exit_t = *const HvVcpuExitInfo;
pub type hv_ipa_t = u64;
pub type hv_memory_flags_t = u64;

// Memory permission flags.
pub const HV_MEMORY_READ: u64 = 1 << 0;
pub const HV_MEMORY_WRITE: u64 = 1 << 1;
pub const HV_MEMORY_EXEC: u64 = 1 << 2;

// Exit reasons.
pub const HV_EXIT_REASON_CANCELED: u32 = 0;
pub const HV_EXIT_REASON_EXCEPTION: u32 = 1;
pub const HV_EXIT_REASON_VTIMER_ACTIVATED: u32 = 2;
pub const HV_EXIT_REASON_UNKNOWN: u32 = 3;

// General-purpose registers (hv_reg_t).
pub const HV_REG_X0: u32 = 0;
pub const HV_REG_X1: u32 = 1;
pub const HV_REG_X2: u32 = 2;
pub const HV_REG_X3: u32 = 3;
pub const HV_REG_X4: u32 = 4;
pub const HV_REG_X5: u32 = 5;
pub const HV_REG_X6: u32 = 6;
pub const HV_REG_X7: u32 = 7;
pub const HV_REG_X8: u32 = 8;
pub const HV_REG_X9: u32 = 9;
pub const HV_REG_X10: u32 = 10;
pub const HV_REG_X11: u32 = 11;
pub const HV_REG_X12: u32 = 12;
pub const HV_REG_X13: u32 = 13;
pub const HV_REG_X14: u32 = 14;
pub const HV_REG_X15: u32 = 15;
pub const HV_REG_X16: u32 = 16;
pub const HV_REG_X17: u32 = 17;
pub const HV_REG_X18: u32 = 18;
pub const HV_REG_X19: u32 = 19;
pub const HV_REG_X20: u32 = 20;
pub const HV_REG_X21: u32 = 21;
pub const HV_REG_X22: u32 = 22;
pub const HV_REG_X23: u32 = 23;
pub const HV_REG_X24: u32 = 24;
pub const HV_REG_X25: u32 = 25;
pub const HV_REG_X26: u32 = 26;
pub const HV_REG_X27: u32 = 27;
pub const HV_REG_X28: u32 = 28;
pub const HV_REG_X29: u32 = 29;
pub const HV_REG_X30: u32 = 30;
pub const HV_REG_PC: u32 = 31;
pub const HV_REG_FPCR: u32 = 32;
pub const HV_REG_FPSR: u32 = 33;
pub const HV_REG_CPSR: u32 = 34;

// System registers (hv_sys_reg_t).
// Encoding: (op0 << 14) | (op1 << 11) | (crn << 7) | (crm << 3) | op2
pub const HV_SYS_REG_ID_AA64MMFR0_EL1: u16 = 0xc038;
pub const HV_SYS_REG_MPIDR_EL1: u16 = 0xc005;
pub const HV_SYS_REG_SCTLR_EL1: u16 = 0xc080;
pub const HV_SYS_REG_TTBR0_EL1: u16 = 0xc100;
pub const HV_SYS_REG_TTBR1_EL1: u16 = 0xc101;
pub const HV_SYS_REG_TCR_EL1: u16 = 0xc102;
pub const HV_SYS_REG_MAIR_EL1: u16 = 0xc510;
pub const HV_SYS_REG_VBAR_EL1: u16 = 0xc600;
pub const HV_SYS_REG_SP_EL1: u16 = 0xe208;
pub const HV_SYS_REG_SPSR_EL1: u16 = 0xc200;
pub const HV_SYS_REG_ELR_EL1: u16 = 0xc201;
pub const HV_SYS_REG_CNTV_CTL_EL0: u16 = 0xdf19;
pub const HV_SYS_REG_CNTV_CVAL_EL0: u16 = 0xdf1a;

// Interrupts the framework reserves (`hv_gic_intid_t`, a uint16_t enum).
pub const HV_GIC_INT_PERFORMANCE_MONITOR: u16 = 23;
pub const HV_GIC_INT_MAINTENANCE: u16 = 25;
pub const HV_GIC_INT_EL2_PHYSICAL_TIMER: u16 = 26;
pub const HV_GIC_INT_EL1_VIRTUAL_TIMER: u16 = 27;
pub const HV_GIC_INT_EL1_PHYSICAL_TIMER: u16 = 30;

// Interrupt types for hv_vcpu_set_pending_interrupt.
pub const HV_INTERRUPT_TYPE_IRQ: u32 = 0;
pub const HV_INTERRUPT_TYPE_FIQ: u32 = 1;

/// Describes the exception that triggered a VM exit.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct HvVcpuExitException {
    pub syndrome: u64,
    pub virtual_address: u64,
    pub physical_address: u64,
}

/// Top-level exit information populated after each `hv_vcpu_run`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct HvVcpuExitInfo {
    pub reason: u32,
    pub exception: HvVcpuExitException,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `hv_sys_reg_t` is the MRS/MSR encoding: op0, op1, CRn, CRm, op2.
    const fn enc(op0: u16, op1: u16, crn: u16, crm: u16, op2: u16) -> u16 {
        (op0 << 14) | (op1 << 11) | (crn << 7) | (crm << 3) | op2
    }

    #[test]
    fn system_registers_carry_their_architectural_encoding() {
        let want = [
            (HV_SYS_REG_ID_AA64MMFR0_EL1, enc(3, 0, 0, 7, 0)),
            (HV_SYS_REG_MPIDR_EL1, enc(3, 0, 0, 0, 5)),
            (HV_SYS_REG_SCTLR_EL1, enc(3, 0, 1, 0, 0)),
            (HV_SYS_REG_TTBR0_EL1, enc(3, 0, 2, 0, 0)),
            (HV_SYS_REG_TTBR1_EL1, enc(3, 0, 2, 0, 1)),
            (HV_SYS_REG_TCR_EL1, enc(3, 0, 2, 0, 2)),
            (HV_SYS_REG_SPSR_EL1, enc(3, 0, 4, 0, 0)),
            (HV_SYS_REG_ELR_EL1, enc(3, 0, 4, 0, 1)),
            (HV_SYS_REG_SP_EL1, enc(3, 4, 4, 1, 0)),
            (HV_SYS_REG_MAIR_EL1, enc(3, 0, 10, 2, 0)),
            (HV_SYS_REG_VBAR_EL1, enc(3, 0, 12, 0, 0)),
            (HV_SYS_REG_CNTV_CTL_EL0, enc(3, 3, 14, 3, 1)),
            (HV_SYS_REG_CNTV_CVAL_EL0, enc(3, 3, 14, 3, 2)),
        ];
        for (ours, arch) in want {
            assert_eq!(ours, arch, "{ours:#x} should be {arch:#x}");
        }
    }
}
