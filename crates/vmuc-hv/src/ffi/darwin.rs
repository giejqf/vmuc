// SPDX-License-Identifier: Apache-2.0
//! The real `extern "C"` declarations, matching `<Hypervisor/hv.h>`,
//! `<Hypervisor/hv_vcpu.h>`, `<Hypervisor/hv_vm_config.h>` and
//! `<Hypervisor/hv_gic.h>` from the macOS 15+ SDK.
//!
//! Derived from ArcBox `virt/arcbox-hv/src/ffi.rs` (MIT OR Apache-2.0).

use std::ffi::c_void;

use super::{hv_ipa_t, hv_memory_flags_t, hv_return_t, hv_vcpu_exit_t, hv_vcpu_t};

#[link(name = "Hypervisor", kind = "framework")]
unsafe extern "C" {
    // VM lifecycle.
    pub fn hv_vm_get_max_vcpu_count(count: *mut u32) -> hv_return_t;
    pub fn hv_vm_create(config: *mut c_void) -> hv_return_t;
    pub fn hv_vm_destroy() -> hv_return_t;

    // Guest physical memory.
    pub fn hv_vm_map(
        addr: *mut u8,
        ipa: hv_ipa_t,
        size: usize,
        flags: hv_memory_flags_t,
    ) -> hv_return_t;
    pub fn hv_vm_unmap(ipa: hv_ipa_t, size: usize) -> hv_return_t;
    pub fn hv_vm_protect(ipa: hv_ipa_t, size: usize, flags: hv_memory_flags_t) -> hv_return_t;

    // VM configuration (macOS 13+).
    pub fn hv_vm_config_create() -> *mut c_void;
    pub fn hv_vm_config_get_ipa_size(config: *mut c_void, ipa_size: *mut u32) -> hv_return_t;
    pub fn hv_vm_config_set_ipa_size(config: *mut c_void, ipa_size: u32) -> hv_return_t;
    pub fn hv_vm_config_get_default_ipa_size(ipa_size: *mut u32) -> hv_return_t;
    pub fn hv_vm_config_get_max_ipa_size(ipa_size: *mut u32) -> hv_return_t;

    // vCPU lifecycle and execution.
    pub fn hv_vcpu_create(
        vcpu: *mut hv_vcpu_t,
        exit: *mut hv_vcpu_exit_t,
        config: *mut c_void,
    ) -> hv_return_t;
    pub fn hv_vcpu_destroy(vcpu: hv_vcpu_t) -> hv_return_t;
    pub fn hv_vcpu_run(vcpu: hv_vcpu_t) -> hv_return_t;
    pub fn hv_vcpus_exit(vcpus: *const hv_vcpu_t, count: u32) -> hv_return_t;

    // Registers.
    pub fn hv_vcpu_get_reg(vcpu: hv_vcpu_t, reg: u32, value: *mut u64) -> hv_return_t;
    pub fn hv_vcpu_set_reg(vcpu: hv_vcpu_t, reg: u32, value: u64) -> hv_return_t;
    pub fn hv_vcpu_get_sys_reg(vcpu: hv_vcpu_t, reg: u16, value: *mut u64) -> hv_return_t;
    pub fn hv_vcpu_set_sys_reg(vcpu: hv_vcpu_t, reg: u16, value: u64) -> hv_return_t;

    // Interrupts and the virtual timer.
    pub fn hv_vcpu_set_pending_interrupt(
        vcpu: hv_vcpu_t,
        int_type: u32,
        pending: bool,
    ) -> hv_return_t;
    pub fn hv_vcpu_get_vtimer_mask(vcpu: hv_vcpu_t, masked: *mut bool) -> hv_return_t;
    pub fn hv_vcpu_set_vtimer_mask(vcpu: hv_vcpu_t, masked: bool) -> hv_return_t;
    pub fn hv_vcpu_get_vtimer_offset(vcpu: hv_vcpu_t, offset: *mut u64) -> hv_return_t;
    pub fn hv_vcpu_set_vtimer_offset(vcpu: hv_vcpu_t, offset: u64) -> hv_return_t;
    pub fn hv_vcpu_get_exec_time(vcpu: hv_vcpu_t, time: *mut u64) -> hv_return_t;

    // Debug traps.
    pub fn hv_vcpu_set_trap_debug_exceptions(vcpu: hv_vcpu_t, enable: bool) -> hv_return_t;
    pub fn hv_vcpu_set_trap_debug_reg_accesses(vcpu: hv_vcpu_t, enable: bool) -> hv_return_t;

    // In-kernel GICv3 (macOS 15+). The config object form is the one the
    // Xcode 26 SDK ships; the VMM chooses the distributor and
    // redistributor bases and tells the framework.
    pub fn hv_gic_config_create() -> *mut c_void;
    pub fn hv_gic_config_set_distributor_base(config: *mut c_void, base: u64) -> hv_return_t;
    pub fn hv_gic_config_set_redistributor_base(config: *mut c_void, base: u64) -> hv_return_t;
    pub fn hv_gic_config_set_msi_region_base(config: *mut c_void, base: u64) -> hv_return_t;
    pub fn hv_gic_config_set_msi_interrupt_range(
        config: *mut c_void,
        base: u32,
        count: u32,
    ) -> hv_return_t;
    pub fn hv_gic_create(config: *mut c_void) -> hv_return_t;
    pub fn hv_gic_reset() -> hv_return_t;
    pub fn hv_gic_set_spi(intid: u32, level: bool) -> hv_return_t;
    pub fn hv_gic_send_msi(address: u64, intid: u32) -> hv_return_t;
    pub fn hv_gic_get_distributor_size(size: *mut usize) -> hv_return_t;
    pub fn hv_gic_get_distributor_base_alignment(alignment: *mut usize) -> hv_return_t;
    pub fn hv_gic_get_redistributor_base(vcpu: hv_vcpu_t, addr: *mut u64) -> hv_return_t;
    pub fn hv_gic_get_redistributor_region_size(size: *mut usize) -> hv_return_t;
    pub fn hv_gic_get_redistributor_size(size: *mut usize) -> hv_return_t;
    pub fn hv_gic_get_redistributor_base_alignment(alignment: *mut usize) -> hv_return_t;
    pub fn hv_gic_get_msi_region_size(size: *mut usize) -> hv_return_t;
    pub fn hv_gic_get_msi_region_base_alignment(alignment: *mut usize) -> hv_return_t;
    pub fn hv_gic_get_spi_interrupt_range(base: *mut u32, count: *mut u32) -> hv_return_t;
    pub fn hv_gic_get_intid(interrupt: u16, intid: *mut u32) -> hv_return_t;
    pub fn hv_gic_set_state(data: *const c_void, size: usize) -> hv_return_t;

    // State for snapshots (macOS 15+): the SIMD/FP registers, the GIC's
    // device state (an os_object) and its CPU interface registers.
    pub fn hv_vcpu_get_simd_fp_reg(
        vcpu: hv_vcpu_t,
        reg: u32,
        value: *mut hv_simd_fp_uchar16_t,
    ) -> hv_return_t;
    /// Takes the value by value in Q0, which stable Rust cannot declare;
    /// called through [`hv_vcpu_set_simd_fp_reg`]'s assembly.
    #[link_name = "hv_vcpu_set_simd_fp_reg"]
    fn hv_vcpu_set_simd_fp_reg_sym();
    pub fn hv_gic_state_create() -> *mut c_void;
    pub fn hv_gic_state_get_size(state: *mut c_void, size: *mut usize) -> hv_return_t;
    pub fn hv_gic_state_get_data(state: *mut c_void, data: *mut c_void) -> hv_return_t;
    pub fn hv_gic_get_icc_reg(vcpu: hv_vcpu_t, reg: u16, value: *mut u64) -> hv_return_t;
    pub fn hv_gic_set_icc_reg(vcpu: hv_vcpu_t, reg: u16, value: u64) -> hv_return_t;
}

/// A Q register's 16 bytes (`hv_simd_fp_uchar16_t`, read by pointer).
pub type hv_simd_fp_uchar16_t = [u8; 16];

pub fn simd_from_bytes(b: [u8; 16]) -> hv_simd_fp_uchar16_t {
    b
}

pub fn simd_to_bytes(v: hv_simd_fp_uchar16_t) -> [u8; 16] {
    v
}

/// `hv_vcpu_set_simd_fp_reg(vcpu, reg, value)`: AAPCS64 passes the vector
/// `value` in Q0, which needs the unstable `simd_ffi` in an `extern`
/// declaration, so the call is made here with the C ABI's clobbers.
///
/// # Safety
/// As for any framework call: `vcpu` belongs to the calling thread.
pub unsafe fn hv_vcpu_set_simd_fp_reg(
    vcpu: hv_vcpu_t,
    reg: u32,
    value: hv_simd_fp_uchar16_t,
) -> hv_return_t {
    let rc: u64;
    // SAFETY: loads 16 readable bytes into Q0 and calls the function with
    // x0 = vcpu, w1 = reg as the C ABI wants; `clobber_abi("C")` covers
    // every register the callee may change, and the stack is untouched.
    unsafe {
        std::arch::asm!(
            "ldr q0, [{value}]",
            "blr {f}",
            value = in(reg) value.as_ptr(),
            f = in(reg) hv_vcpu_set_simd_fp_reg_sym as *const () as usize,
            inlateout("x0") vcpu => rc,
            in("x1") u64::from(reg),
            clobber_abi("C"),
        );
    }
    rc as hv_return_t
}

// Config objects (`hv_vm_config_t`, `hv_gic_config_t`) are os_objects
// returned retained; libSystem's `os_release` drops them.
unsafe extern "C" {
    pub fn os_release(object: *mut c_void);
}
