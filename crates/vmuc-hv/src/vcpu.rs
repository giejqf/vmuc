// SPDX-License-Identifier: Apache-2.0
//! One vCPU, pinned to the thread that created it.
//!
//! Derived from ArcBox `virt/arcbox-hv/src/vcpu.rs` (MIT OR Apache-2.0).

use std::marker::PhantomData;
use std::ptr;
use std::rc::Rc;

use tracing::{trace, warn};

use crate::error::{self, HvResult};
use crate::exit::{self, VcpuExit};
use crate::ffi;

/// A vCPU handle. `!Send` on purpose: Hypervisor.framework requires every
/// operation on a vCPU to happen on its creating thread. Create each vCPU
/// once and keep it across PSCI CPU_OFF/CPU_ON; recreating deadlocks.
pub struct HvVcpu {
    id: u64,
    exit_info: *const ffi::HvVcpuExitInfo,
    _not_send: PhantomData<Rc<()>>,
}

impl HvVcpu {
    /// Creates a vCPU on the current thread.
    pub fn new() -> HvResult<Self> {
        let mut id: u64 = 0;
        let mut exit: ffi::hv_vcpu_exit_t = ptr::null();
        // SAFETY: valid out-pointers; the framework owns the exit struct.
        error::check(unsafe { ffi::hv_vcpu_create(&raw mut id, &raw mut exit, ptr::null_mut()) })?;
        trace!(vcpu = id, "vCPU created");
        Ok(Self {
            id,
            exit_info: exit,
            _not_send: PhantomData,
        })
    }

    /// The framework handle, shareable as a plain `u64` for
    /// [`HvVcpu::force_exit`].
    #[inline]
    pub fn id(&self) -> u64 {
        self.id
    }

    /// Forces the listed vCPUs out of `hv_vcpu_run` (or makes their next
    /// run return at once). Callable from any thread; this is the stop and
    /// pause path only, never an interrupt kick. An empty list is a no-op,
    /// not "all vCPUs".
    pub fn force_exit(ids: &[u64]) -> HvResult<()> {
        if ids.is_empty() {
            return Ok(());
        }
        // SAFETY: `ids` is a live slice; the framework does not retain it.
        error::check(unsafe { ffi::hv_vcpus_exit(ids.as_ptr(), ids.len() as u32) })
    }

    /// Runs the guest until the next exit.
    pub fn run(&self) -> HvResult<VcpuExit> {
        // SAFETY: created on this thread, `id` is live.
        error::check(unsafe { ffi::hv_vcpu_run(self.id) })?;
        if self.exit_info.is_null() {
            return Err(crate::HvError::Error);
        }
        // SAFETY: the framework keeps `exit_info` valid for the vCPU's life
        // and only writes it inside `hv_vcpu_run`.
        Ok(exit::parse_exit(unsafe { &*self.exit_info }))
    }

    pub fn get_reg(&self, reg: u32) -> HvResult<u64> {
        let mut v = 0u64;
        // SAFETY: live vCPU, valid out-pointer.
        error::check(unsafe { ffi::hv_vcpu_get_reg(self.id, reg, &raw mut v) })?;
        Ok(v)
    }

    pub fn set_reg(&self, reg: u32, value: u64) -> HvResult<()> {
        // SAFETY: live vCPU.
        error::check(unsafe { ffi::hv_vcpu_set_reg(self.id, reg, value) })
    }

    pub fn get_sys_reg(&self, reg: u16) -> HvResult<u64> {
        let mut v = 0u64;
        // SAFETY: live vCPU, valid out-pointer.
        error::check(unsafe { ffi::hv_vcpu_get_sys_reg(self.id, reg, &raw mut v) })?;
        Ok(v)
    }

    pub fn set_sys_reg(&self, reg: u16, value: u64) -> HvResult<()> {
        // SAFETY: live vCPU.
        error::check(unsafe { ffi::hv_vcpu_set_sys_reg(self.id, reg, value) })
    }

    /// Advances PC by one instruction. Needed after a data abort or a
    /// system-register trap; *not* after HVC/SMC.
    pub fn skip_instruction(&self) -> HvResult<()> {
        let pc = self.get_reg(ffi::HV_REG_PC)?;
        self.set_reg(ffi::HV_REG_PC, pc.wrapping_add(4))
    }

    /// Raises or clears a pending IRQ/FIQ directly on this vCPU. Not used
    /// when the in-kernel GIC is active; kept for bring-up experiments.
    pub fn set_pending_interrupt(&self, irq: bool, fiq: bool) -> HvResult<()> {
        // SAFETY: live vCPU.
        error::check(unsafe {
            ffi::hv_vcpu_set_pending_interrupt(self.id, ffi::HV_INTERRUPT_TYPE_IRQ, irq)
        })?;
        // SAFETY: live vCPU.
        error::check(unsafe {
            ffi::hv_vcpu_set_pending_interrupt(self.id, ffi::HV_INTERRUPT_TYPE_FIQ, fiq)
        })
    }

    pub fn set_vtimer_mask(&self, masked: bool) -> HvResult<()> {
        // SAFETY: live vCPU.
        error::check(unsafe { ffi::hv_vcpu_set_vtimer_mask(self.id, masked) })
    }

    pub fn vtimer_mask(&self) -> HvResult<bool> {
        let mut m = false;
        // SAFETY: live vCPU, valid out-pointer.
        error::check(unsafe { ffi::hv_vcpu_get_vtimer_mask(self.id, &raw mut m) })?;
        Ok(m)
    }

    /// Offset subtracted from the host counter to form the guest's
    /// CNTVCT. Used to keep guest time continuous across a pause.
    pub fn vtimer_offset(&self) -> HvResult<u64> {
        let mut o = 0u64;
        // SAFETY: live vCPU, valid out-pointer.
        error::check(unsafe { ffi::hv_vcpu_get_vtimer_offset(self.id, &raw mut o) })?;
        Ok(o)
    }

    pub fn set_vtimer_offset(&self, offset: u64) -> HvResult<()> {
        // SAFETY: live vCPU.
        error::check(unsafe { ffi::hv_vcpu_set_vtimer_offset(self.id, offset) })
    }

    /// Cumulative guest execution time in nanoseconds.
    pub fn exec_time(&self) -> HvResult<u64> {
        let mut t = 0u64;
        // SAFETY: live vCPU, valid out-pointer.
        error::check(unsafe { ffi::hv_vcpu_get_exec_time(self.id, &raw mut t) })?;
        Ok(t)
    }
}

impl Drop for HvVcpu {
    fn drop(&mut self) {
        // SAFETY: live vCPU on its creating thread (enforced by !Send).
        if let Err(e) = error::check(unsafe { ffi::hv_vcpu_destroy(self.id) }) {
            warn!(vcpu = self.id, "hv_vcpu_destroy failed: {e}");
        } else {
            trace!(vcpu = self.id, "vCPU destroyed");
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn stub_returns_unsupported() {
        if !crate::NATIVE {
            assert_eq!(
                super::HvVcpu::new().err(),
                Some(crate::HvError::Unsupported)
            );
        }
    }
}
