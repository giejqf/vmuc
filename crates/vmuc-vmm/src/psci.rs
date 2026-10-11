// SPDX-License-Identifier: Apache-2.0
//! PSCI 1.0 over HVC: the subset a Linux guest uses to bring up
//! secondary CPUs, take them down, and power off or reset the machine.
//!
//! Decoding is pure; the vCPU loop acts on the decoded call.

/// Function IDs (SMC Calling Convention, PSCI spec DEN0022).
pub mod func {
    pub const PSCI_VERSION: u32 = 0x8400_0000;
    pub const CPU_SUSPEND_32: u32 = 0x8400_0001;
    pub const CPU_SUSPEND_64: u32 = 0xc400_0001;
    pub const CPU_OFF: u32 = 0x8400_0002;
    pub const CPU_ON_32: u32 = 0x8400_0003;
    pub const CPU_ON_64: u32 = 0xc400_0003;
    pub const AFFINITY_INFO_32: u32 = 0x8400_0004;
    pub const AFFINITY_INFO_64: u32 = 0xc400_0004;
    pub const MIGRATE_INFO_TYPE: u32 = 0x8400_0006;
    pub const SYSTEM_OFF: u32 = 0x8400_0008;
    pub const SYSTEM_RESET: u32 = 0x8400_0009;
    pub const PSCI_FEATURES: u32 = 0x8400_000a;
    pub const SMCCC_VERSION: u32 = 0x8000_0000;
}

/// Return codes, as the signed values X0 carries.
pub mod ret {
    pub const SUCCESS: i64 = 0;
    pub const NOT_SUPPORTED: i64 = -1;
    pub const INVALID_PARAMETERS: i64 = -2;
    pub const ALREADY_ON: i64 = -4;
    pub const ON_PENDING: i64 = -5;
}

/// PSCI 1.0.
pub const VERSION: u64 = 1 << 16;

/// `MIGRATE_INFO_TYPE`: no trusted OS that would need migrating.
pub const MIGRATE_NOT_REQUIRED: u64 = 2;

/// `AFFINITY_INFO` states.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u64)]
pub enum AffinityState {
    On = 0,
    Off = 1,
    OnPending = 2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PsciCall {
    Version,
    CpuOff,
    CpuOn {
        target_mpidr: u64,
        entry: u64,
        context: u64,
    },
    AffinityInfo {
        target_mpidr: u64,
        lowest_level: u64,
    },
    MigrateInfoType,
    SystemOff,
    SystemReset,
    Features(u32),
    /// Anything else, including CPU_SUSPEND (the guest kernel has no
    /// cpuidle; plain WFI is its idle).
    Unsupported(u32),
}

impl PsciCall {
    /// Decodes X0..X3 of an HVC/SMC. 32-bit calls only pass 32-bit
    /// arguments, so their upper halves are discarded.
    pub fn decode(x: [u64; 4]) -> Self {
        let id = x[0] as u32;
        let narrow = |v: u64| {
            if id & 0x4000_0000 == 0 {
                v & 0xffff_ffff
            } else {
                v
            }
        };
        match id {
            func::PSCI_VERSION => Self::Version,
            func::CPU_OFF => Self::CpuOff,
            func::CPU_ON_32 | func::CPU_ON_64 => Self::CpuOn {
                target_mpidr: narrow(x[1]),
                entry: narrow(x[2]),
                context: narrow(x[3]),
            },
            func::AFFINITY_INFO_32 | func::AFFINITY_INFO_64 => Self::AffinityInfo {
                target_mpidr: narrow(x[1]),
                lowest_level: narrow(x[2]),
            },
            func::MIGRATE_INFO_TYPE => Self::MigrateInfoType,
            func::SYSTEM_OFF => Self::SystemOff,
            func::SYSTEM_RESET => Self::SystemReset,
            func::PSCI_FEATURES => Self::Features(x[1] as u32),
            other => Self::Unsupported(other),
        }
    }
}

/// `PSCI_FEATURES` answer: 0 for an implemented function, NOT_SUPPORTED
/// otherwise.
pub fn features(id: u32) -> i64 {
    match id {
        func::PSCI_VERSION
        | func::CPU_OFF
        | func::CPU_ON_32
        | func::CPU_ON_64
        | func::AFFINITY_INFO_32
        | func::AFFINITY_INFO_64
        | func::MIGRATE_INFO_TYPE
        | func::SYSTEM_OFF
        | func::SYSTEM_RESET
        | func::PSCI_FEATURES => ret::SUCCESS,
        _ => ret::NOT_SUPPORTED,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_cpu_on_both_widths() {
        let c = PsciCall::decode([u64::from(func::CPU_ON_64), 0x101, 0x4000_1000, 0xdead]);
        assert_eq!(
            c,
            PsciCall::CpuOn {
                target_mpidr: 0x101,
                entry: 0x4000_1000,
                context: 0xdead
            }
        );
        let c = PsciCall::decode([u64::from(func::CPU_ON_32), 1 << 40 | 3, 1 << 33 | 8, 0]);
        assert_eq!(
            c,
            PsciCall::CpuOn {
                target_mpidr: 3,
                entry: 8,
                context: 0
            }
        );
    }

    #[test]
    fn decodes_the_rest() {
        let d = |id: u32| PsciCall::decode([u64::from(id), 0, 0, 0]);
        assert_eq!(d(func::PSCI_VERSION), PsciCall::Version);
        assert_eq!(d(func::SYSTEM_OFF), PsciCall::SystemOff);
        assert_eq!(d(func::SYSTEM_RESET), PsciCall::SystemReset);
        assert_eq!(d(func::CPU_OFF), PsciCall::CpuOff);
        assert_eq!(
            d(func::CPU_SUSPEND_64),
            PsciCall::Unsupported(func::CPU_SUSPEND_64)
        );
        // The upper 32 bits of X0 are ignored (SMCCC: W0 holds the ID).
        assert_eq!(
            PsciCall::decode([0xffff_ffff_8400_0008, 0, 0, 0]),
            PsciCall::SystemOff
        );
    }

    #[test]
    fn features_cover_what_is_implemented() {
        assert_eq!(features(func::CPU_ON_64), ret::SUCCESS);
        assert_eq!(features(func::CPU_SUSPEND_32), ret::NOT_SUPPORTED);
        assert_eq!(features(func::SMCCC_VERSION), ret::NOT_SUPPORTED);
    }
}
