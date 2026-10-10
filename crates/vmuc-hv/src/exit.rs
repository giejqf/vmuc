// SPDX-License-Identifier: Apache-2.0
//! Decoding of `hv_vcpu_exit_t` into a typed [`VcpuExit`].
//!
//! Derived from ArcBox `virt/arcbox-hv/src/exit.rs` (MIT OR Apache-2.0).

use crate::ffi;

// Exception class values (ESR_EL2.EC).
const EC_WFI_WFE: u8 = 0x01;
const EC_HVC: u8 = 0x16;
const EC_SMC: u8 = 0x17;
const EC_SYS_REG: u8 = 0x18;
const EC_INSN_ABORT_LOWER: u8 = 0x20;
const EC_INSN_ABORT_SAME: u8 = 0x21;
const EC_DATA_ABORT_LOWER: u8 = 0x24;
const EC_DATA_ABORT_SAME: u8 = 0x25;
const EC_SOFTWARE_BREAKPOINT: u8 = 0x3c;

/// Why `hv_vcpu_run` returned.
#[derive(Debug)]
pub enum VcpuExit {
    /// Cancelled by `hv_vcpus_exit`.
    Canceled,
    /// A trapped exception.
    Exception {
        class: ExceptionClass,
        syndrome: u64,
    },
    /// The virtual timer fired while masked.
    VtimerActivated,
    /// A reason this crate does not know.
    Unknown(u32),
}

/// Decoded exception class.
#[derive(Debug)]
pub enum ExceptionClass {
    /// Access to an unmapped IPA: MMIO, or a reclaimed page being touched.
    DataAbort(MmioInfo),
    /// Instruction fetch from an unmapped or faulting IPA.
    InstructionAbort { address: u64, fault_code: u8 },
    /// `BRK #imm16`.
    SoftwareBreakpoint(u16),
    /// WFI or WFE. With the in-kernel GIC this is rarely seen; the
    /// framework idles inside `hv_vcpu_run`.
    WaitForInterrupt,
    /// `HVC #imm16`. The framework has already advanced PC.
    HypercallHvc(u16),
    /// `SMC #imm16`. The framework has already advanced PC.
    SmcCall(u16),
    /// Trapped MSR/MRS. The VMM must advance PC by 4 after handling.
    SystemRegister {
        op0: u8,
        op1: u8,
        crn: u8,
        crm: u8,
        op2: u8,
        /// `true` for MSR (guest writes the register).
        is_write: bool,
        /// The Xt register carrying the value.
        rt: u8,
    },
    /// An EC not handled here.
    Other(u8),
}

/// A guest data abort decoded from the ISS. For a data abort the VMM
/// must advance PC by 4 itself after emulating the access.
#[derive(Debug)]
pub struct MmioInfo {
    /// Faulting guest physical address.
    pub address: u64,
    /// Instruction Syndrome Valid (ISS[24]). When `false` the size,
    /// register and extension fields are UNKNOWN (LDP/STP, atomics);
    /// only `address` and `is_write` are meaningful.
    pub isv: bool,
    pub is_write: bool,
    /// 1, 2, 4 or 8.
    pub access_size: u8,
    /// Xt (31 is XZR).
    pub register: u8,
    /// Sign-extend the loaded value (ISS[21] SSE).
    pub sign_extend: bool,
    /// 64-bit destination register (ISS[15] SF).
    pub sixty_four: bool,
}

/// Parses the exit structure the framework filled after `hv_vcpu_run`.
pub fn parse_exit(info: &ffi::HvVcpuExitInfo) -> VcpuExit {
    match info.reason {
        ffi::HV_EXIT_REASON_CANCELED => VcpuExit::Canceled,
        ffi::HV_EXIT_REASON_EXCEPTION => {
            let syndrome = info.exception.syndrome;
            VcpuExit::Exception {
                class: parse_exception(syndrome, &info.exception),
                syndrome,
            }
        }
        ffi::HV_EXIT_REASON_VTIMER_ACTIVATED => VcpuExit::VtimerActivated,
        other => VcpuExit::Unknown(other),
    }
}

fn parse_exception(syndrome: u64, exc: &ffi::HvVcpuExitException) -> ExceptionClass {
    let ec = ((syndrome >> 26) & 0x3f) as u8;
    match ec {
        EC_DATA_ABORT_LOWER | EC_DATA_ABORT_SAME => ExceptionClass::DataAbort(MmioInfo {
            address: exc.physical_address,
            isv: (syndrome >> 24) & 1 != 0,
            is_write: (syndrome >> 6) & 1 != 0,
            access_size: 1u8 << ((syndrome >> 22) & 3),
            register: ((syndrome >> 16) & 0x1f) as u8,
            sign_extend: (syndrome >> 21) & 1 != 0,
            sixty_four: (syndrome >> 15) & 1 != 0,
        }),
        EC_INSN_ABORT_LOWER | EC_INSN_ABORT_SAME => ExceptionClass::InstructionAbort {
            address: exc.physical_address,
            fault_code: (syndrome & 0x3f) as u8,
        },
        EC_SOFTWARE_BREAKPOINT => ExceptionClass::SoftwareBreakpoint((syndrome & 0xffff) as u16),
        EC_WFI_WFE => ExceptionClass::WaitForInterrupt,
        EC_HVC => ExceptionClass::HypercallHvc((syndrome & 0xffff) as u16),
        EC_SMC => ExceptionClass::SmcCall((syndrome & 0xffff) as u16),
        EC_SYS_REG => ExceptionClass::SystemRegister {
            // ISS for EC=0x18: [21:20] Op0, [19:17] Op2, [16:14] Op1,
            // [13:10] CRn, [9:5] Rt, [4:1] CRm, [0] Direction (1 = MRS).
            op0: ((syndrome >> 20) & 0x3) as u8,
            op2: ((syndrome >> 17) & 0x7) as u8,
            op1: ((syndrome >> 14) & 0x7) as u8,
            crn: ((syndrome >> 10) & 0xf) as u8,
            rt: ((syndrome >> 5) & 0x1f) as u8,
            crm: ((syndrome >> 1) & 0xf) as u8,
            is_write: syndrome & 1 == 0,
        },
        other => ExceptionClass::Other(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exit(reason: u32, syndrome: u64, pa: u64) -> ffi::HvVcpuExitInfo {
        ffi::HvVcpuExitInfo {
            reason,
            exception: ffi::HvVcpuExitException {
                syndrome,
                virtual_address: 0,
                physical_address: pa,
            },
        }
    }

    fn data_abort(sas: u8, is_write: bool, reg: u8, sext: bool, sf: bool) -> u64 {
        (u64::from(EC_DATA_ABORT_LOWER) << 26)
            | (1 << 24)
            | (u64::from(sas) << 22)
            | (u64::from(sext) << 21)
            | (u64::from(reg) << 16)
            | (u64::from(sf) << 15)
            | (u64::from(is_write) << 6)
    }

    #[test]
    fn reasons() {
        assert!(matches!(parse_exit(&exit(0, 0, 0)), VcpuExit::Canceled));
        assert!(matches!(
            parse_exit(&exit(2, 0, 0)),
            VcpuExit::VtimerActivated
        ));
        assert!(matches!(
            parse_exit(&exit(0xff, 0, 0)),
            VcpuExit::Unknown(0xff)
        ));
    }

    #[test]
    fn data_abort_fields() {
        for (sas, size) in [(0u8, 1u8), (1, 2), (2, 4), (3, 8)] {
            let e = exit(1, data_abort(sas, true, 5, false, true), 0x0a00_0000);
            let VcpuExit::Exception {
                class: ExceptionClass::DataAbort(m),
                ..
            } = parse_exit(&e)
            else {
                panic!("expected data abort");
            };
            assert!(m.isv && m.is_write && m.sixty_four && !m.sign_extend);
            assert_eq!(
                (m.access_size, m.register, m.address),
                (size, 5, 0x0a00_0000)
            );
        }
    }

    #[test]
    fn data_abort_without_isv() {
        let s = data_abort(3, false, 7, false, false) & !(1 << 24);
        let VcpuExit::Exception {
            class: ExceptionClass::DataAbort(m),
            ..
        } = parse_exit(&exit(1, s, 0))
        else {
            panic!("expected data abort");
        };
        assert!(!m.isv && !m.is_write);
    }

    #[test]
    fn hvc_smc_wfi_brk() {
        let s = |ec: u8, imm: u64| (u64::from(ec) << 26) | imm;
        assert!(matches!(
            parse_exit(&exit(1, s(EC_HVC, 0x1234), 0)),
            VcpuExit::Exception {
                class: ExceptionClass::HypercallHvc(0x1234),
                ..
            }
        ));
        assert!(matches!(
            parse_exit(&exit(1, s(EC_SMC, 0xabcd), 0)),
            VcpuExit::Exception {
                class: ExceptionClass::SmcCall(0xabcd),
                ..
            }
        ));
        assert!(matches!(
            parse_exit(&exit(1, s(EC_WFI_WFE, 0), 0)),
            VcpuExit::Exception {
                class: ExceptionClass::WaitForInterrupt,
                ..
            }
        ));
        assert!(matches!(
            parse_exit(&exit(1, s(EC_SOFTWARE_BREAKPOINT, 0x42), 0)),
            VcpuExit::Exception {
                class: ExceptionClass::SoftwareBreakpoint(0x42),
                ..
            }
        ));
        assert!(matches!(
            parse_exit(&exit(1, s(EC_INSN_ABORT_LOWER, 0x04), 0x4000_0000)),
            VcpuExit::Exception {
                class: ExceptionClass::InstructionAbort {
                    address: 0x4000_0000,
                    fault_code: 4
                },
                ..
            }
        ));
        assert!(matches!(
            parse_exit(&exit(1, s(0x2f, 0), 0)),
            VcpuExit::Exception {
                class: ExceptionClass::Other(0x2f),
                ..
            }
        ));
    }

    #[test]
    fn sysreg_encoding() {
        // OSDLR_EL1 is op0=2 op1=0 crn=1 crm=3 op2=4; MSR from X9.
        let iss = (2u64 << 20) | (4 << 17) | (0 << 14) | (1 << 10) | (9 << 5) | (3 << 1);
        let s = (u64::from(EC_SYS_REG) << 26) | iss;
        let VcpuExit::Exception {
            class:
                ExceptionClass::SystemRegister {
                    op0,
                    op1,
                    crn,
                    crm,
                    op2,
                    is_write,
                    rt,
                },
            ..
        } = parse_exit(&exit(1, s, 0))
        else {
            panic!("expected sysreg");
        };
        assert_eq!((op0, op1, crn, crm, op2, rt), (2, 0, 1, 3, 4, 9));
        assert!(is_write);
        let VcpuExit::Exception {
            class: ExceptionClass::SystemRegister { is_write, .. },
            ..
        } = parse_exit(&exit(1, s | 1, 0))
        else {
            panic!("expected sysreg");
        };
        assert!(!is_write);
    }
}
