// SPDX-License-Identifier: Apache-2.0
//! Safe Rust bindings for Apple Hypervisor.framework on Apple silicon.
//!
//! The wrappers cover what a Linux VMM needs: VM and vCPU lifecycle,
//! guest memory mapping, exit decoding, the vtimer, and the in-kernel
//! GICv3 (`hv_gic_*`, macOS 15+).
//!
//! On targets other than `aarch64-apple-darwin` the `ffi` module is a set
//! of stubs that return `HV_UNSUPPORTED`, so every call site in the
//! workspace compiles and is covered by clippy on a Linux dev box. Only
//! the real thing links against the framework.
//!
//! Derived from ArcBox `virt/arcbox-hv` (MIT OR Apache-2.0); see NOTICE.

mod error;
mod exit;
pub mod ffi;
mod gic;
pub mod guest_memory;
mod memory;
pub mod state;
mod vcpu;
mod vm;

pub use error::{HvError, HvResult, check};
pub use exit::{ExceptionClass, MmioInfo, VcpuExit};
pub use gic::{Gic, GicConfig, ReservedIntid};
pub use memory::{HOST_PAGE_SIZE, MemoryPermission, align_inward, align_inward_to};
pub use state::VcpuState;
pub use vcpu::HvVcpu;
pub use vm::HvVm;

/// `true` when this build can actually talk to Hypervisor.framework.
pub const NATIVE: bool = cfg!(all(target_os = "macos", target_arch = "aarch64"));

/// ARM64 general-purpose and special register IDs (`hv_reg_t`).
pub mod reg {
    pub use crate::ffi::{
        HV_REG_CPSR, HV_REG_FPCR, HV_REG_FPSR, HV_REG_PC, HV_REG_X0, HV_REG_X1, HV_REG_X2,
        HV_REG_X3, HV_REG_X4, HV_REG_X5, HV_REG_X6, HV_REG_X7, HV_REG_X8, HV_REG_X9, HV_REG_X10,
        HV_REG_X11, HV_REG_X12, HV_REG_X13, HV_REG_X14, HV_REG_X15, HV_REG_X16, HV_REG_X17,
        HV_REG_X18, HV_REG_X19, HV_REG_X20, HV_REG_X21, HV_REG_X22, HV_REG_X23, HV_REG_X24,
        HV_REG_X25, HV_REG_X26, HV_REG_X27, HV_REG_X28, HV_REG_X29, HV_REG_X30,
    };
}

/// ARM64 system register IDs (`hv_sys_reg_t`).
pub mod sys_reg {
    pub use crate::ffi::{
        HV_SYS_REG_CNTV_CTL_EL0, HV_SYS_REG_CNTV_CVAL_EL0, HV_SYS_REG_ELR_EL1,
        HV_SYS_REG_ID_AA64MMFR0_EL1, HV_SYS_REG_MAIR_EL1, HV_SYS_REG_MPIDR_EL1,
        HV_SYS_REG_SCTLR_EL1, HV_SYS_REG_SP_EL1, HV_SYS_REG_SPSR_EL1, HV_SYS_REG_TCR_EL1,
        HV_SYS_REG_TTBR0_EL1, HV_SYS_REG_TTBR1_EL1, HV_SYS_REG_VBAR_EL1,
    };
}
