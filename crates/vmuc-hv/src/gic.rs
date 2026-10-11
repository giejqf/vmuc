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

/// Guest physical placement of the GIC frames. Must match the FDT; the
/// VMM's guest physical map owns the values.
#[derive(Debug, Clone, Copy)]
pub struct GicConfig {
    pub distributor_base: u64,
    pub redistributor_base: u64,
}

/// Interrupts whose INTIDs the framework assigns (`hv_gic_intid_t`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum ReservedIntid {
    PerformanceMonitor = ffi::HV_GIC_INT_PERFORMANCE_MONITOR,
    Maintenance = ffi::HV_GIC_INT_MAINTENANCE,
    El2PhysicalTimer = ffi::HV_GIC_INT_EL2_PHYSICAL_TIMER,
    El1VirtualTimer = ffi::HV_GIC_INT_EL1_VIRTUAL_TIMER,
    El1PhysicalTimer = ffi::HV_GIC_INT_EL1_PHYSICAL_TIMER,
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
        let rc = error::check(unsafe {
            ffi::hv_gic_config_set_distributor_base(c, config.distributor_base)
        })
        .and_then(|()| {
            // SAFETY: as above.
            error::check(unsafe {
                ffi::hv_gic_config_set_redistributor_base(c, config.redistributor_base)
            })
        })
        // SAFETY: as above; the framework copies the configuration.
        .and_then(|()| error::check(unsafe { ffi::hv_gic_create(c) }));
        // SAFETY: `c` was returned retained and is not used again.
        unsafe { ffi::os_release(c) };
        rc?;
        debug!(
            gicd = format_args!("{:#x}", config.distributor_base),
            gicr = format_args!("{:#x}", config.redistributor_base),
            "GICv3 created"
        );
        Ok(Self { config })
    }

    /// Size of the distributor frame and the alignment its base needs.
    /// Valid before the GIC exists (after the VM does).
    pub fn distributor_geometry() -> HvResult<(usize, usize)> {
        let (mut size, mut align) = (0usize, 0usize);
        // SAFETY: valid out-pointers.
        error::check(unsafe { ffi::hv_gic_get_distributor_size(&raw mut size) })?;
        // SAFETY: valid out-pointer.
        error::check(unsafe { ffi::hv_gic_get_distributor_base_alignment(&raw mut align) })?;
        Ok((size, align))
    }

    /// Size of the whole redistributor region (every vCPU the framework
    /// supports, not just the ones created) and its base alignment.
    pub fn redistributor_geometry() -> HvResult<(usize, usize)> {
        let (mut size, mut align) = (0usize, 0usize);
        // SAFETY: valid out-pointers.
        error::check(unsafe { ffi::hv_gic_get_redistributor_region_size(&raw mut size) })?;
        // SAFETY: valid out-pointer.
        error::check(unsafe { ffi::hv_gic_get_redistributor_base_alignment(&raw mut align) })?;
        Ok((size, align))
    }

    /// The INTID the framework uses for one of its reserved interrupts,
    /// e.g. the EL1 virtual timer PPI the FDT must advertise.
    pub fn reserved_intid(which: ReservedIntid) -> HvResult<u32> {
        let mut intid = 0u32;
        // SAFETY: valid out-pointer.
        error::check(unsafe { ffi::hv_gic_get_intid(which as u16, &raw mut intid) })?;
        Ok(intid)
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
    /// Callable from any device thread once the GIC exists.
    #[inline]
    pub fn set_spi(intid: u32, level: bool) -> HvResult<()> {
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
            let config = super::GicConfig {
                distributor_base: 0x0800_0000,
                redistributor_base: 0x0c00_0000,
            };
            assert!(super::Gic::new(config).is_err());
            assert!(super::Gic::redistributor_geometry().is_err());
        }
    }
}
