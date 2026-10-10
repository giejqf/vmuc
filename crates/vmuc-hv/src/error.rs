// SPDX-License-Identifier: Apache-2.0
//! `hv_return_t` to `HvError`.
//!
//! Derived from ArcBox `virt/arcbox-hv/src/error.rs` (MIT OR Apache-2.0).

use crate::ffi::hv_return_t;

/// Status codes returned by Hypervisor.framework (`<Hypervisor/hv_error.h>`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum HvError {
    #[error("generic hypervisor error")]
    Error,
    #[error("resource is busy")]
    Busy,
    #[error("bad argument")]
    BadArgument,
    #[error("illegal guest state")]
    IllegalGuestState,
    #[error("no resources available")]
    NoResources,
    #[error("no device found")]
    NoDevice,
    #[error("permission denied (missing com.apple.security.hypervisor entitlement?)")]
    Denied,
    #[error("operation not supported on this host or build")]
    Unsupported,
    #[error("unknown hypervisor error (0x{0:08x})")]
    Unknown(u32),
}

pub type HvResult<T> = Result<T, HvError>;

const HV_SUCCESS: hv_return_t = 0;
const HV_ERROR: hv_return_t = 0xfae9_4001_u32.cast_signed();
const HV_BUSY: hv_return_t = 0xfae9_4002_u32.cast_signed();
const HV_BAD_ARGUMENT: hv_return_t = 0xfae9_4003_u32.cast_signed();
const HV_ILLEGAL_GUEST_STATE: hv_return_t = 0xfae9_4004_u32.cast_signed();
const HV_NO_RESOURCES: hv_return_t = 0xfae9_4005_u32.cast_signed();
const HV_NO_DEVICE: hv_return_t = 0xfae9_4006_u32.cast_signed();
const HV_DENIED: hv_return_t = 0xfae9_4007_u32.cast_signed();
const HV_UNSUPPORTED: hv_return_t = 0xfae9_400f_u32.cast_signed();

/// Converts a raw `hv_return_t` into `HvResult<()>`.
pub fn check(status: hv_return_t) -> HvResult<()> {
    match status {
        HV_SUCCESS => Ok(()),
        HV_ERROR => Err(HvError::Error),
        HV_BUSY => Err(HvError::Busy),
        HV_BAD_ARGUMENT => Err(HvError::BadArgument),
        HV_ILLEGAL_GUEST_STATE => Err(HvError::IllegalGuestState),
        HV_NO_RESOURCES => Err(HvError::NoResources),
        HV_NO_DEVICE => Err(HvError::NoDevice),
        HV_DENIED => Err(HvError::Denied),
        HV_UNSUPPORTED => Err(HvError::Unsupported),
        other => Err(HvError::Unknown(other.cast_unsigned())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_codes_map() {
        assert!(check(HV_SUCCESS).is_ok());
        assert_eq!(check(HV_ERROR), Err(HvError::Error));
        assert_eq!(check(HV_BUSY), Err(HvError::Busy));
        assert_eq!(check(HV_BAD_ARGUMENT), Err(HvError::BadArgument));
        assert_eq!(
            check(HV_ILLEGAL_GUEST_STATE),
            Err(HvError::IllegalGuestState)
        );
        assert_eq!(check(HV_NO_RESOURCES), Err(HvError::NoResources));
        assert_eq!(check(HV_NO_DEVICE), Err(HvError::NoDevice));
        assert_eq!(check(HV_DENIED), Err(HvError::Denied));
        assert_eq!(check(HV_UNSUPPORTED), Err(HvError::Unsupported));
    }

    #[test]
    fn unknown_code_preserved() {
        assert_eq!(
            check(0xdead_beef_u32.cast_signed()),
            Err(HvError::Unknown(0xdead_beef))
        );
    }
}
