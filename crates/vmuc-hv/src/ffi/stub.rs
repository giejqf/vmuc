// SPDX-License-Identifier: Apache-2.0
//! Stand-ins for the framework functions on targets that cannot link
//! Hypervisor.framework. Every function returns `HV_UNSUPPORTED`
//! (`0xfae9400f`) so callers see a clean error, never UB. Signatures must
//! match `darwin.rs` exactly; the wrappers are compiled against both.

use std::ffi::c_void;

use super::{hv_ipa_t, hv_memory_flags_t, hv_return_t, hv_vcpu_exit_t, hv_vcpu_t};

const UNSUPPORTED: hv_return_t = 0xfae9_400f_u32.cast_signed();

macro_rules! stubs {
    ($( fn $name:ident ( $( $arg:ident : $ty:ty ),* $(,)? ) -> $ret:ty = $val:expr ; )*) => {
        $(
            #[allow(unused_variables, clippy::missing_safety_doc)]
            pub unsafe fn $name($( $arg: $ty ),*) -> $ret { $val }
        )*
    };
}

stubs! {
    fn hv_vm_get_max_vcpu_count(count: *mut u32) -> hv_return_t = UNSUPPORTED;
    fn hv_vm_create(config: *mut c_void) -> hv_return_t = UNSUPPORTED;
    fn hv_vm_destroy() -> hv_return_t = UNSUPPORTED;
    fn hv_vm_map(addr: *mut u8, ipa: hv_ipa_t, size: usize, flags: hv_memory_flags_t) -> hv_return_t = UNSUPPORTED;
    fn hv_vm_unmap(ipa: hv_ipa_t, size: usize) -> hv_return_t = UNSUPPORTED;
    fn hv_vm_protect(ipa: hv_ipa_t, size: usize, flags: hv_memory_flags_t) -> hv_return_t = UNSUPPORTED;
    fn hv_vm_config_create() -> *mut c_void = std::ptr::null_mut();
    fn hv_vm_config_get_ipa_size(config: *mut c_void, ipa_size: *mut u32) -> hv_return_t = UNSUPPORTED;
    fn hv_vm_config_set_ipa_size(config: *mut c_void, ipa_size: u32) -> hv_return_t = UNSUPPORTED;
    fn hv_vm_config_get_default_ipa_size(ipa_size: *mut u32) -> hv_return_t = UNSUPPORTED;
    fn hv_vm_config_get_max_ipa_size(ipa_size: *mut u32) -> hv_return_t = UNSUPPORTED;
    fn hv_vcpu_create(vcpu: *mut hv_vcpu_t, exit: *mut hv_vcpu_exit_t, config: *mut c_void) -> hv_return_t = UNSUPPORTED;
    fn hv_vcpu_destroy(vcpu: hv_vcpu_t) -> hv_return_t = UNSUPPORTED;
    fn hv_vcpu_run(vcpu: hv_vcpu_t) -> hv_return_t = UNSUPPORTED;
    fn hv_vcpus_exit(vcpus: *const hv_vcpu_t, count: u32) -> hv_return_t = UNSUPPORTED;
    fn hv_vcpu_get_reg(vcpu: hv_vcpu_t, reg: u32, value: *mut u64) -> hv_return_t = UNSUPPORTED;
    fn hv_vcpu_set_reg(vcpu: hv_vcpu_t, reg: u32, value: u64) -> hv_return_t = UNSUPPORTED;
    fn hv_vcpu_get_sys_reg(vcpu: hv_vcpu_t, reg: u16, value: *mut u64) -> hv_return_t = UNSUPPORTED;
    fn hv_vcpu_set_sys_reg(vcpu: hv_vcpu_t, reg: u16, value: u64) -> hv_return_t = UNSUPPORTED;
    fn hv_vcpu_set_pending_interrupt(vcpu: hv_vcpu_t, int_type: u32, pending: bool) -> hv_return_t = UNSUPPORTED;
    fn hv_vcpu_get_vtimer_mask(vcpu: hv_vcpu_t, masked: *mut bool) -> hv_return_t = UNSUPPORTED;
    fn hv_vcpu_set_vtimer_mask(vcpu: hv_vcpu_t, masked: bool) -> hv_return_t = UNSUPPORTED;
    fn hv_vcpu_get_vtimer_offset(vcpu: hv_vcpu_t, offset: *mut u64) -> hv_return_t = UNSUPPORTED;
    fn hv_vcpu_set_vtimer_offset(vcpu: hv_vcpu_t, offset: u64) -> hv_return_t = UNSUPPORTED;
    fn hv_vcpu_get_exec_time(vcpu: hv_vcpu_t, time: *mut u64) -> hv_return_t = UNSUPPORTED;
    fn hv_vcpu_set_trap_debug_exceptions(vcpu: hv_vcpu_t, enable: bool) -> hv_return_t = UNSUPPORTED;
    fn hv_vcpu_set_trap_debug_reg_accesses(vcpu: hv_vcpu_t, enable: bool) -> hv_return_t = UNSUPPORTED;
    fn hv_gic_config_create() -> *mut c_void = std::ptr::null_mut();
    fn hv_gic_config_set_distributor_base(config: *mut c_void, base: u64) -> hv_return_t = UNSUPPORTED;
    fn hv_gic_config_set_redistributor_base(config: *mut c_void, base: u64) -> hv_return_t = UNSUPPORTED;
    fn hv_gic_config_set_msi_region_base(config: *mut c_void, base: u64) -> hv_return_t = UNSUPPORTED;
    fn hv_gic_config_set_msi_interrupt_range(config: *mut c_void, base: u32, count: u32) -> hv_return_t = UNSUPPORTED;
    fn hv_gic_create(config: *mut c_void) -> hv_return_t = UNSUPPORTED;
    fn hv_gic_reset() -> hv_return_t = UNSUPPORTED;
    fn hv_gic_set_spi(intid: u32, level: bool) -> hv_return_t = UNSUPPORTED;
    fn hv_gic_send_msi(address: u64, intid: u32) -> hv_return_t = UNSUPPORTED;
    fn hv_gic_get_distributor_size(size: *mut usize) -> hv_return_t = UNSUPPORTED;
    fn hv_gic_get_distributor_base_alignment(alignment: *mut usize) -> hv_return_t = UNSUPPORTED;
    fn hv_gic_get_redistributor_base(vcpu: hv_vcpu_t, addr: *mut u64) -> hv_return_t = UNSUPPORTED;
    fn hv_gic_get_redistributor_region_size(size: *mut usize) -> hv_return_t = UNSUPPORTED;
    fn hv_gic_get_redistributor_size(size: *mut usize) -> hv_return_t = UNSUPPORTED;
    fn hv_gic_get_redistributor_base_alignment(alignment: *mut usize) -> hv_return_t = UNSUPPORTED;
    fn hv_gic_get_msi_region_size(size: *mut usize) -> hv_return_t = UNSUPPORTED;
    fn hv_gic_get_msi_region_base_alignment(alignment: *mut usize) -> hv_return_t = UNSUPPORTED;
    fn hv_gic_get_spi_interrupt_range(base: *mut u32, count: *mut u32) -> hv_return_t = UNSUPPORTED;
    fn hv_gic_get_intid(interrupt: u16, intid: *mut u32) -> hv_return_t = UNSUPPORTED;
    fn hv_gic_set_state(data: *const c_void, size: usize) -> hv_return_t = UNSUPPORTED;
    fn hv_vcpu_get_simd_fp_reg(vcpu: hv_vcpu_t, reg: u32, value: *mut hv_simd_fp_uchar16_t) -> hv_return_t = UNSUPPORTED;
    fn hv_vcpu_set_simd_fp_reg(vcpu: hv_vcpu_t, reg: u32, value: hv_simd_fp_uchar16_t) -> hv_return_t = UNSUPPORTED;
    fn hv_gic_state_create() -> *mut c_void = std::ptr::null_mut();
    fn hv_gic_state_get_size(state: *mut c_void, size: *mut usize) -> hv_return_t = UNSUPPORTED;
    fn hv_gic_state_get_data(state: *mut c_void, data: *mut c_void) -> hv_return_t = UNSUPPORTED;
    fn hv_gic_get_icc_reg(vcpu: hv_vcpu_t, reg: u16, value: *mut u64) -> hv_return_t = UNSUPPORTED;
    fn hv_gic_set_icc_reg(vcpu: hv_vcpu_t, reg: u16, value: u64) -> hv_return_t = UNSUPPORTED;
    fn os_release(object: *mut c_void) -> () = ();
}

/// Off Apple silicon there are no vector registers to pass.
pub type hv_simd_fp_uchar16_t = [u8; 16];

pub fn simd_from_bytes(b: [u8; 16]) -> hv_simd_fp_uchar16_t {
    b
}

pub fn simd_to_bytes(v: hv_simd_fp_uchar16_t) -> [u8; 16] {
    v
}
