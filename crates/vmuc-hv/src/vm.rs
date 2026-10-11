// SPDX-License-Identifier: Apache-2.0
//! VM lifecycle and guest physical memory.
//!
//! Derived from ArcBox `virt/arcbox-hv/src/vm.rs` (MIT OR Apache-2.0).

use std::ptr;

use tracing::{debug, warn};

use crate::error::{self, HvResult};
use crate::ffi;
use crate::memory::MemoryPermission;

/// The process's one and only Hypervisor.framework VM.
///
/// Hypervisor.framework allows a single VM per process; a second
/// [`HvVm::new`] returns [`HvError::Busy`](crate::HvError::Busy).
/// Dropping destroys the VM; every vCPU and every mapping must be gone
/// by then (see the teardown contract in `AGENTS.md`).
pub struct HvVm {
    _private: (),
}

impl HvVm {
    /// Creates a VM with the framework's default IPA size.
    pub fn new() -> HvResult<Self> {
        // SAFETY: `hv_vm_create(NULL)` is the documented default-config call.
        error::check(unsafe { ffi::hv_vm_create(ptr::null_mut()) })?;
        debug!("hypervisor VM created");
        Ok(Self { _private: () })
    }

    /// Creates a VM with an explicit IPA width in bits (36 is the default
    /// on most hosts, 40 gives 1 TiB of guest physical space).
    pub fn with_ipa_size(ipa_bits: u32) -> HvResult<Self> {
        // SAFETY: returns an os_object or NULL; the framework owns it.
        let config = unsafe { ffi::hv_vm_config_create() };
        if config.is_null() {
            return Err(crate::HvError::Unsupported);
        }
        // SAFETY: `config` is a live config object.
        let rc = error::check(unsafe { ffi::hv_vm_config_set_ipa_size(config, ipa_bits) })
            // SAFETY: as above; the framework copies what it needs.
            .and_then(|()| error::check(unsafe { ffi::hv_vm_create(config) }));
        // SAFETY: `config` was returned retained and is not used again.
        unsafe { ffi::os_release(config) };
        rc?;
        debug!(ipa_bits, "hypervisor VM created");
        Ok(Self { _private: () })
    }

    /// The default and maximum IPA widths this host supports.
    pub fn ipa_size_limits() -> HvResult<(u32, u32)> {
        let (mut default, mut max) = (0u32, 0u32);
        // SAFETY: valid out-pointers.
        error::check(unsafe { ffi::hv_vm_config_get_default_ipa_size(&raw mut default) })?;
        // SAFETY: valid out-pointer.
        error::check(unsafe { ffi::hv_vm_config_get_max_ipa_size(&raw mut max) })?;
        Ok((default, max))
    }

    /// Most vCPUs one VM can have on this host.
    pub fn max_vcpu_count() -> HvResult<u32> {
        let mut n = 0u32;
        // SAFETY: valid out-pointer.
        error::check(unsafe { ffi::hv_vm_get_max_vcpu_count(&raw mut n) })?;
        Ok(n)
    }

    /// Maps host memory into the guest at `ipa`.
    ///
    /// `host_addr`, `ipa` and `size` must be multiples of the 16 KiB host
    /// page. The framework rejects overlapping remaps of a *different*
    /// host range into a live IPA range.
    ///
    /// # Safety
    /// `host_addr` must point to at least `size` bytes that stay mapped in
    /// this process for as long as the guest mapping exists.
    pub unsafe fn map_memory(
        &self,
        host_addr: *mut u8,
        ipa: u64,
        size: usize,
        perm: MemoryPermission,
    ) -> HvResult<()> {
        // SAFETY: caller contract above; the framework validates the IPA range.
        error::check(unsafe { ffi::hv_vm_map(host_addr, ipa, size, perm.bits()) })
    }

    /// Unmaps `[ipa, ipa + size)`. Sub-ranges of a live mapping are allowed
    /// as long as the same host memory is mapped back afterwards.
    pub fn unmap_memory(&self, ipa: u64, size: usize) -> HvResult<()> {
        // SAFETY: the framework validates the range.
        error::check(unsafe { ffi::hv_vm_unmap(ipa, size) })
    }

    /// Changes permissions on a mapped range.
    pub fn protect_memory(&self, ipa: u64, size: usize, perm: MemoryPermission) -> HvResult<()> {
        // SAFETY: the framework validates the range.
        error::check(unsafe { ffi::hv_vm_protect(ipa, size, perm.bits()) })
    }

    /// Forces the listed vCPUs out of `hv_vcpu_run`. On Apple silicon the
    /// list must be explicit; an empty list is a no-op, not "all vCPUs".
    pub fn exit_vcpus(&self, vcpus: &[u64]) -> HvResult<()> {
        if vcpus.is_empty() {
            return Ok(());
        }
        // SAFETY: `vcpus` is a live slice; the framework does not retain it.
        error::check(unsafe { ffi::hv_vcpus_exit(vcpus.as_ptr(), vcpus.len() as u32) })
    }
}

impl Drop for HvVm {
    fn drop(&mut self) {
        // SAFETY: we hold the only VM handle.
        if let Err(e) = error::check(unsafe { ffi::hv_vm_destroy() }) {
            warn!("hv_vm_destroy failed: {e}");
        } else {
            debug!("hypervisor VM destroyed");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stub_or_native_behaves() {
        if crate::NATIVE {
            return; // Native tests need the entitlement; they live in tests/e2e.
        }
        assert_eq!(HvVm::new().err(), Some(crate::HvError::Unsupported));
        assert_eq!(
            HvVm::with_ipa_size(40).err(),
            Some(crate::HvError::Unsupported)
        );
    }
}
