// SPDX-License-Identifier: Apache-2.0
//! The in-kernel GICv3 (`hv_gic_*`, macOS 15+).
//!
//! Devices assert SPIs with [`Gic::set_spi`]. With the in-kernel GIC an
//! asserted SPI wakes a WFI-idle vCPU inside `hv_vcpu_run`; never follow
//! it with `hv_vcpus_exit`.
//!
//! Derived from ArcBox `virt/arcbox-hv/src/gic.rs` (MIT OR Apache-2.0).

use tracing::debug;

use crate::error::{self, HvResult};
use crate::ffi;

/// Guest physical placement of the GIC frames. Must match the FDT.
#[derive(Debug, Clone, Copy)]
pub struct GicConfig {
    pub distributor_base: u64,
    pub redistributor_base: u64,
}

impl Default for GicConfig {
    fn default() -> Self {
        Self {
            distributor_base: 0x0800_0000,
            redistributor_base: 0x080A_0000,
        }
    }
}

/// Handle to the VM's one GIC. Created after the VM and before any vCPU
/// runs; destroyed with the VM.
#[derive(Debug)]
pub struct Gic {
    config: GicConfig,
}

impl Gic {
    pub fn new(config: GicConfig) -> HvResult<Self> {
        // SAFETY: returns an os_object or NULL.
        let c = unsafe { ffi::hv_gic_config_create() };
        if c.is_null() {
            return Err(crate::HvError::Unsupported);
        }
        // SAFETY: `c` is a live config object for each call below.
        error::check(unsafe {
            ffi::hv_gic_config_set_distributor_base(c, config.distributor_base)
        })?;
        error::check(unsafe {
            ffi::hv_gic_config_set_redistributor_base(c, config.redistributor_base)
        })?;
        error::check(unsafe { ffi::hv_gic_create(c) })?;
        debug!(
            gicd = format_args!("{:#x}", config.distributor_base),
            gicr = format_args!("{:#x}", config.redistributor_base),
            "GICv3 created"
        );
        Ok(Self { config })
    }

    pub fn config(&self) -> GicConfig {
        self.config
    }

    pub fn reset(&self) -> HvResult<()> {
        // SAFETY: GIC exists.
        error::check(unsafe { ffi::hv_gic_reset() })
    }

    /// Asserts (`true`) or deasserts a shared peripheral interrupt.
    /// `intid` is the GIC INTID (32 and up), not the FDT SPI number.
    #[inline]
    pub fn set_spi(&self, intid: u32, level: bool) -> HvResult<()> {
        // SAFETY: GIC exists.
        error::check(unsafe { ffi::hv_gic_set_spi(intid, level) })
    }

    pub fn distributor_size(&self) -> HvResult<usize> {
        let mut s = 0usize;
        // SAFETY: valid out-pointer.
        error::check(unsafe { ffi::hv_gic_get_distributor_size(&raw mut s) })?;
        Ok(s)
    }

    /// Size of one redistributor frame (one per vCPU).
    pub fn redistributor_size(&self) -> HvResult<usize> {
        let mut s = 0usize;
        // SAFETY: valid out-pointer.
        error::check(unsafe { ffi::hv_gic_get_redistributor_size(&raw mut s) })?;
        Ok(s)
    }

    /// Size of the whole redistributor region.
    pub fn redistributor_region_size(&self) -> HvResult<usize> {
        let mut s = 0usize;
        // SAFETY: valid out-pointer.
        error::check(unsafe { ffi::hv_gic_get_redistributor_region_size(&raw mut s) })?;
        Ok(s)
    }

    pub fn redistributor_base(&self, vcpu: u64) -> HvResult<u64> {
        let mut a = 0u64;
        // SAFETY: valid out-pointer.
        error::check(unsafe { ffi::hv_gic_get_redistributor_base(vcpu, &raw mut a) })?;
        Ok(a)
    }

    /// `(first_intid, count)` of the SPIs the framework supports.
    pub fn spi_range(&self) -> HvResult<(u32, u32)> {
        let (mut base, mut count) = (0u32, 0u32);
        // SAFETY: valid out-pointers.
        error::check(unsafe {
            ffi::hv_gic_get_spi_interrupt_range(&raw mut base, &raw mut count)
        })?;
        Ok((base, count))
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn stub_returns_unsupported() {
        if !crate::NATIVE {
            assert!(super::Gic::new(super::GicConfig::default()).is_err());
        }
    }
}
