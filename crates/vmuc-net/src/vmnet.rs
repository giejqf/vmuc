// SPDX-License-Identifier: Apache-2.0
//! Hand-written bindings for vmnet.framework, plus the bits of xpc,
//! libdispatch and the blocks ABI it needs. macOS only.
//!
//! vmnet reports through Objective-C blocks. The blocks here are global
//! block literals leaked for the process's lifetime: `Block_copy` returns
//! a global block as-is, so the captured context pointer stays valid with
//! no copy/dispose helpers. The helper creates three per run.

#![allow(non_camel_case_types, non_upper_case_globals)]

use std::ffi::{CStr, CString, c_char, c_void};

pub type xpc_object_t = *mut c_void;
pub type dispatch_queue_t = *mut c_void;
pub type dispatch_semaphore_t = *mut c_void;
pub type interface_ref = *mut c_void;
pub type vmnet_return_t = u32;

pub const VMNET_SUCCESS: vmnet_return_t = 1000;
pub const VMNET_SHARED_MODE: u64 = 1001;
pub const VMNET_BRIDGED_MODE: u64 = 1002;
pub const VMNET_INTERFACE_PACKETS_AVAILABLE: u32 = 1;
pub const DISPATCH_TIME_FOREVER: u64 = u64::MAX;

#[repr(C)]
pub struct vmpktdesc {
    pub vm_pkt_size: usize,
    pub vm_pkt_iov: *mut libc::iovec,
    pub vm_pkt_iovcnt: u32,
    pub vm_flags: u32,
}

#[link(name = "vmnet", kind = "framework")]
unsafe extern "C" {
    pub static vmnet_operation_mode_key: *const c_char;
    pub static vmnet_shared_interface_name_key: *const c_char;
    pub static vmnet_allocate_mac_address_key: *const c_char;
    pub static vmnet_mtu_key: *const c_char;
    pub static vmnet_max_packet_size_key: *const c_char;
    pub static vmnet_start_address_key: *const c_char;
    pub static vmnet_end_address_key: *const c_char;
    pub static vmnet_subnet_mask_key: *const c_char;
    pub static vmnet_enable_tso_key: *const c_char;
    pub static vmnet_read_max_packets_key: *const c_char;
    pub static vmnet_write_max_packets_key: *const c_char;

    pub fn vmnet_start_interface(
        desc: xpc_object_t,
        queue: dispatch_queue_t,
        handler: *const c_void,
    ) -> interface_ref;
    pub fn vmnet_interface_set_event_callback(
        iface: interface_ref,
        mask: u32,
        queue: dispatch_queue_t,
        callback: *const c_void,
    ) -> vmnet_return_t;
    pub fn vmnet_read(
        iface: interface_ref,
        packets: *mut vmpktdesc,
        count: *mut i32,
    ) -> vmnet_return_t;
    pub fn vmnet_write(
        iface: interface_ref,
        packets: *mut vmpktdesc,
        count: *mut i32,
    ) -> vmnet_return_t;
    pub fn vmnet_stop_interface(
        iface: interface_ref,
        queue: dispatch_queue_t,
        handler: *const c_void,
    ) -> vmnet_return_t;
}

/// `vmnet_enable_virtio_header_key`, from macOS 15.4: looked up at run
/// time so the helper still loads on 15.0-15.3, where it is absent.
pub fn virtio_header_key() -> Option<*const c_char> {
    // SAFETY: dlsym with a NUL-terminated name; a non-null result is the
    // address of the framework's `const char * const` key.
    unsafe {
        let p = libc::dlsym(
            libc::RTLD_DEFAULT,
            c"vmnet_enable_virtio_header_key".as_ptr(),
        );
        (!p.is_null()).then(|| *p.cast::<*const c_char>())
    }
}

unsafe extern "C" {
    static _NSConcreteGlobalBlock: c_void;

    pub fn xpc_dictionary_create(
        keys: *const *const c_char,
        values: *const xpc_object_t,
        count: usize,
    ) -> xpc_object_t;
    pub fn xpc_dictionary_set_uint64(dict: xpc_object_t, key: *const c_char, value: u64);
    pub fn xpc_dictionary_set_bool(dict: xpc_object_t, key: *const c_char, value: bool);
    pub fn xpc_dictionary_set_string(dict: xpc_object_t, key: *const c_char, value: *const c_char);
    pub fn xpc_dictionary_get_uint64(dict: xpc_object_t, key: *const c_char) -> u64;
    pub fn xpc_dictionary_get_string(dict: xpc_object_t, key: *const c_char) -> *const c_char;
    pub fn xpc_release(object: xpc_object_t);

    pub fn dispatch_queue_create(label: *const c_char, attr: *const c_void) -> dispatch_queue_t;
    pub fn dispatch_semaphore_create(value: isize) -> dispatch_semaphore_t;
    pub fn dispatch_semaphore_wait(sem: dispatch_semaphore_t, timeout: u64) -> isize;
    pub fn dispatch_semaphore_signal(sem: dispatch_semaphore_t) -> isize;
    pub fn dispatch_time(when: u64, delta: i64) -> u64;
    pub fn dispatch_async_f(
        queue: dispatch_queue_t,
        context: *mut c_void,
        work: unsafe extern "C" fn(*mut c_void),
    );
}

/// `BLOCK_IS_GLOBAL`: `Block_copy` returns the block unchanged.
const BLOCK_IS_GLOBAL: i32 = 1 << 28;

#[repr(C)]
struct BlockDescriptor {
    reserved: usize,
    size: usize,
}

/// A block literal capturing one context pointer.
#[repr(C)]
struct BlockLiteral<F> {
    isa: *const c_void,
    flags: i32,
    reserved: i32,
    invoke: F,
    descriptor: *const BlockDescriptor,
    context: *mut c_void,
}

fn leak_block<F>(invoke: F, context: *mut c_void) -> *const c_void {
    let descriptor = Box::leak(Box::new(BlockDescriptor {
        reserved: 0,
        size: size_of::<BlockLiteral<F>>(),
    }));
    let block = Box::leak(Box::new(BlockLiteral {
        // Only the address of the runtime's class object is taken.
        isa: &raw const _NSConcreteGlobalBlock,
        flags: BLOCK_IS_GLOBAL,
        reserved: 0,
        invoke,
        descriptor,
        context,
    }));
    std::ptr::from_ref(block).cast()
}

/// `void (^)(vmnet_return_t, xpc_object_t)`, calling `f(context, status, params)`.
pub fn status_params_block(
    f: unsafe extern "C" fn(*mut c_void, vmnet_return_t, xpc_object_t),
    context: *mut c_void,
) -> *const c_void {
    unsafe extern "C" fn trampoline(
        block: *const BlockLiteral<usize>,
        status: vmnet_return_t,
        params: xpc_object_t,
    ) {
        // SAFETY: called by the blocks runtime with the literal built in
        // `leak_block`, whose `invoke` slot holds the trampoline and whose
        // `context` holds the Rust callback and its data (see below).
        unsafe {
            let ctx = (*block).context.cast::<(
                unsafe extern "C" fn(*mut c_void, vmnet_return_t, xpc_object_t),
                *mut c_void,
            )>();
            ((*ctx).0)((*ctx).1, status, params);
        }
    }
    let ctx = Box::leak(Box::new((f, context)));
    leak_block(
        trampoline
            as unsafe extern "C" fn(*const BlockLiteral<usize>, vmnet_return_t, xpc_object_t),
        std::ptr::from_mut(ctx).cast(),
    )
}

/// `void (^)(uint32_t event_mask, xpc_object_t event)`.
pub fn event_block(
    f: unsafe extern "C" fn(*mut c_void, u32, xpc_object_t),
    context: *mut c_void,
) -> *const c_void {
    // Same shape as the status block: (u32, object).
    status_params_block(f, context)
}

/// `void (^)(vmnet_return_t)`.
pub fn status_block(
    f: unsafe extern "C" fn(*mut c_void, vmnet_return_t),
    context: *mut c_void,
) -> *const c_void {
    unsafe extern "C" fn trampoline(block: *const BlockLiteral<usize>, status: vmnet_return_t) {
        // SAFETY: as in `status_params_block`.
        unsafe {
            let ctx = (*block).context.cast::<(
                unsafe extern "C" fn(*mut c_void, vmnet_return_t),
                *mut c_void,
            )>();
            ((*ctx).0)((*ctx).1, status);
        }
    }
    let ctx = Box::leak(Box::new((f, context)));
    leak_block(
        trampoline as unsafe extern "C" fn(*const BlockLiteral<usize>, vmnet_return_t),
        std::ptr::from_mut(ctx).cast(),
    )
}

/// A string value from an xpc dictionary.
///
/// # Safety
/// `dict` must be a live xpc dictionary (or null) and `key` a vmnet key.
pub unsafe fn dict_string(dict: xpc_object_t, key: *const c_char) -> Option<String> {
    if dict.is_null() {
        return None;
    }
    // SAFETY: caller contract; the returned pointer is owned by `dict`.
    let p = unsafe { xpc_dictionary_get_string(dict, key) };
    // SAFETY: a non-null result is a NUL-terminated C string.
    (!p.is_null()).then(|| unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned())
}

pub fn cstring(s: &str) -> CString {
    CString::new(s.replace('\0', "")).unwrap_or_default()
}
