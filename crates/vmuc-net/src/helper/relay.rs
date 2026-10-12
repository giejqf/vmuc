// SPDX-License-Identifier: Apache-2.0
//! The helper's vmnet side: starting the interface, the hello, and the
//! relay over the socketpair itself (a VMM that predates the rings).

use std::ffi::c_void;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::net::UnixDatagram;

use super::{Hello, Mode};
use crate::handoff::{self, First};
use crate::vmnet::{
    VMNET_BRIDGED_MODE, VMNET_INTERFACE_PACKETS_AVAILABLE, VMNET_SHARED_MODE, VMNET_SUCCESS,
    cstring, dict_string, dispatch_queue_create, dispatch_semaphore_create,
    dispatch_semaphore_signal, dispatch_semaphore_t, dispatch_semaphore_wait, dispatch_time,
    event_block, interface_ref, status_block, status_params_block, virtio_header_key,
    vmnet_allocate_mac_address_key, vmnet_enable_tso_key, vmnet_end_address_key,
    vmnet_interface_set_event_callback, vmnet_max_packet_size_key, vmnet_mtu_key,
    vmnet_operation_mode_key, vmnet_read, vmnet_read_max_packets_key, vmnet_return_t,
    vmnet_shared_interface_name_key, vmnet_start_address_key, vmnet_start_interface,
    vmnet_stop_interface, vmnet_subnet_mask_key, vmnet_write, vmnet_write_max_packets_key,
    vmpktdesc, xpc_dictionary_create, xpc_dictionary_get_uint64, xpc_dictionary_set_bool,
    xpc_dictionary_set_string, xpc_dictionary_set_uint64, xpc_object_t, xpc_release,
};

/// Most packets per vmnet_read / vmnet_write call, whatever vmnet allows.
const BATCH: usize = 64;
/// The virtio-net header vmnet adds to every packet when asked to.
const VIRTIO_HDR: usize = 12;
/// Largest TSO segment: a 64 KiB IP packet in an Ethernet frame.
const TSO_FRAME: usize = 65_535 + 14;

struct Start {
    sem: dispatch_semaphore_t,
    status: vmnet_return_t,
    virtio_header: bool,
    hello: Option<Hello>,
    read_max: usize,
    write_max: usize,
}

unsafe extern "C" fn on_start(ctx: *mut c_void, status: vmnet_return_t, params: xpc_object_t) {
    // SAFETY: `ctx` is the `Start` that `run` keeps alive while it waits.
    let s = unsafe { &mut *ctx.cast::<Start>() };
    s.status = status;
    if status == VMNET_SUCCESS && !params.is_null() {
        // SAFETY: `params` is valid for the duration of this callback.
        s.hello = Some(unsafe {
            Hello {
                mtu: xpc_dictionary_get_uint64(params, vmnet_mtu_key),
                max_packet_size: xpc_dictionary_get_uint64(params, vmnet_max_packet_size_key),
                start: dict_string(params, vmnet_start_address_key),
                end: dict_string(params, vmnet_end_address_key),
                mask: dict_string(params, vmnet_subnet_mask_key),
                virtio_header: s.virtio_header,
                rings: true,
            }
        });
        // SAFETY: as above. Zero (unknown) means one packet per call.
        let max = |key| unsafe { xpc_dictionary_get_uint64(params, key) };
        // SAFETY: reading the framework's key pointers.
        let (r, w) = unsafe { (vmnet_read_max_packets_key, vmnet_write_max_packets_key) };
        s.read_max = usize::try_from(max(r)).unwrap_or(1).clamp(1, BATCH);
        s.write_max = usize::try_from(max(w)).unwrap_or(1).clamp(1, BATCH);
    }
    // SAFETY: the semaphore outlives the wait in `run`.
    unsafe { dispatch_semaphore_signal(s.sem) };
}

/// Frames from vmnet to the VMM, on vmnet's dispatch queue.
struct ToVmm {
    iface: interface_ref,
    fd: i32,
    bufs: Vec<Vec<u8>>,
}

unsafe extern "C" fn on_packets(ctx: *mut c_void, _mask: u32, _event: xpc_object_t) {
    // SAFETY: `ctx` is the leaked `ToVmm`, used only on this serial queue.
    let t = unsafe { &mut *ctx.cast::<ToVmm>() };
    loop {
        let mut iov: Vec<libc::iovec> = t
            .bufs
            .iter_mut()
            .map(|b| libc::iovec {
                iov_base: b.as_mut_ptr().cast(),
                iov_len: b.len(),
            })
            .collect();
        let mut pkts: Vec<vmpktdesc> = iov
            .iter_mut()
            .map(|v| vmpktdesc {
                vm_pkt_size: v.iov_len,
                vm_pkt_iov: v,
                vm_pkt_iovcnt: 1,
                vm_flags: 0,
            })
            .collect();
        let mut count = pkts.len() as i32;
        // SAFETY: every descriptor points at one of our buffers.
        let st = unsafe { vmnet_read(t.iface, pkts.as_mut_ptr(), &raw mut count) };
        if st != VMNET_SUCCESS || count <= 0 {
            return;
        }
        for p in &pkts[..count as usize] {
            // SAFETY: vmnet filled `vm_pkt_size` bytes of the buffer.
            let frame = unsafe {
                std::slice::from_raw_parts((*p.vm_pkt_iov).iov_base.cast::<u8>(), p.vm_pkt_size)
            };
            send_frame(t.fd, frame);
        }
    }
}

unsafe extern "C" fn on_stop(ctx: *mut c_void, _status: vmnet_return_t) {
    // SAFETY: `ctx` is the semaphore `run` waits on.
    unsafe { dispatch_semaphore_signal(ctx) };
}

/// Starts a vmnet interface, with TSO and virtio-net headers when
/// `header_key` is given. `Err` carries vmnet's status.
///
/// # Safety
/// `queue` must be a live serial dispatch queue.
unsafe fn start(
    mode: &Mode,
    header_key: Option<*const std::ffi::c_char>,
    queue: *mut c_void,
) -> Result<(interface_ref, Hello, Start), vmnet_return_t> {
    // SAFETY (this block): plain xpc/dispatch/vmnet calls on objects we
    // create; the C strings and `start` outlive every use.
    unsafe {
        let desc = xpc_dictionary_create(std::ptr::null(), std::ptr::null(), 0);
        let iface_name;
        match mode {
            Mode::Shared => {
                xpc_dictionary_set_uint64(desc, vmnet_operation_mode_key, VMNET_SHARED_MODE);
            }
            Mode::Bridged(name) => {
                xpc_dictionary_set_uint64(desc, vmnet_operation_mode_key, VMNET_BRIDGED_MODE);
                iface_name = cstring(name);
                xpc_dictionary_set_string(
                    desc,
                    vmnet_shared_interface_name_key,
                    iface_name.as_ptr(),
                );
            }
        }
        xpc_dictionary_set_bool(desc, vmnet_allocate_mac_address_key, false);
        if let Some(key) = header_key {
            xpc_dictionary_set_bool(desc, vmnet_enable_tso_key, true);
            xpc_dictionary_set_bool(desc, key, true);
        }
        let mut start = Start {
            sem: dispatch_semaphore_create(0),
            status: 0,
            virtio_header: header_key.is_some(),
            hello: None,
            read_max: 1,
            write_max: 1,
        };
        let handler = status_params_block(on_start, (&raw mut start).cast());
        let iface = vmnet_start_interface(desc, queue, handler);
        dispatch_semaphore_wait(start.sem, dispatch_time(0, 30_000_000_000));
        xpc_release(desc);
        match start.hello.take() {
            Some(hello) if !iface.is_null() => Ok((iface, hello, start)),
            _ => Err(start.status),
        }
    }
}

/// Starts the interface, sends the hello, relays until the VMM goes.
pub fn run(mode: &Mode) -> Result<(), String> {
    // SAFETY: fd 0 is the socket the VMM handed us; we own it from here.
    let socket = unsafe { UnixDatagram::from_raw_fd(0) };
    let mut ty: libc::c_int = 0;
    let mut len = size_of::<libc::c_int>() as libc::socklen_t;
    // SAFETY: valid out-pointers for SO_TYPE.
    let rc = unsafe {
        libc::getsockopt(
            0,
            libc::SOL_SOCKET,
            libc::SO_TYPE,
            (&raw mut ty).cast(),
            &raw mut len,
        )
    };
    if rc != 0 || ty != libc::SOCK_DGRAM {
        return Err("stdin must be a SOCK_DGRAM unix socket".into());
    }
    // SAFETY (this block): plain dispatch/vmnet calls on objects we
    // create; the queue lives as long as the process.
    unsafe {
        let label = cstring("vmuc.vmnet");
        let queue = dispatch_queue_create(label.as_ptr(), std::ptr::null());
        // Offloads when this macOS has them; plain frames otherwise or
        // if vmnet refuses them.
        let started = match virtio_header_key() {
            Some(key) => start(mode, Some(key), queue).or_else(|_| start(mode, None, queue)),
            None => start(mode, None, queue),
        };
        let (iface, hello, st) = started
            .map_err(|status| format!("vmnet_start_interface failed with status {status}"))?;
        let line = serde_json::to_vec(&hello).map_err(|e| e.to_string())?;
        socket.send(&line).map_err(|e| format!("hello: {e}"))?;

        // With TSO vmnet hands over and accepts 64 KiB segments whatever
        // max_packet_size says, each behind its virtio-net header.
        let reported = usize::try_from(hello.max_packet_size)
            .unwrap_or(1514)
            .max(1514);
        let size = if hello.virtio_header {
            reported.max(TSO_FRAME) + VIRTIO_HDR
        } else {
            reported
        };
        set_recv_wait(socket.as_raw_fd());
        // The VMM answers the hello with the shared region (rings), or an
        // older VMM simply starts sending frames.
        let mut first = vec![0u8; size];
        let mode = loop {
            match handoff::recv_first(socket.as_raw_fd(), &mut first) {
                First::Idle if idle_and_connected(socket.as_raw_fd()) => {}
                other => break other,
            }
        };
        match mode {
            First::Region(region) => super::rings::run(
                socket.as_raw_fd(),
                iface,
                queue,
                &region,
                size,
                st.read_max,
                st.write_max,
            )?,
            First::Frame(n) => {
                let to_vmm = Box::leak(Box::new(ToVmm {
                    iface,
                    fd: socket.as_raw_fd(),
                    bufs: (0..st.read_max).map(|_| vec![0u8; size]).collect(),
                }));
                let cb = event_block(on_packets, std::ptr::from_mut(to_vmm).cast());
                if vmnet_interface_set_event_callback(
                    iface,
                    VMNET_INTERFACE_PACKETS_AVAILABLE,
                    queue,
                    cb,
                ) != VMNET_SUCCESS
                {
                    return Err("vmnet_interface_set_event_callback failed".into());
                }
                write_frames(iface, &mut [(&mut first, n)]);
                relay_to_vmnet(socket.as_raw_fd(), iface, size, st.write_max);
            }
            First::Idle | First::Closed => {}
        }
        let stopped = dispatch_semaphore_create(0);
        let done = status_block(on_stop, stopped);
        if vmnet_stop_interface(iface, queue, done) == VMNET_SUCCESS {
            dispatch_semaphore_wait(stopped, dispatch_time(0, 5_000_000_000));
        }
    }
    Ok(())
}

/// How long one wait for the VMM's next frame lasts. A datagram socket
/// gives a blocked recv no end-of-file when the VMM goes away, but a
/// recv started after that fails at once, so the helper notices within
/// this time and exits, taking its vmnet interface with it.
const RECV_WAIT: libc::timeval = libc::timeval {
    tv_sec: 1,
    tv_usec: 0,
};

/// Bounds every blocking recv on the VMM's socket by [`RECV_WAIT`].
fn set_recv_wait(fd: i32) {
    let wait = RECV_WAIT;
    // SAFETY: a valid socket and a timeval option value.
    unsafe {
        libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_RCVTIMEO,
            (&raw const wait).cast(),
            size_of::<libc::timeval>() as libc::socklen_t,
        );
    }
}

/// After a recv returned an error: it only timed out (or was
/// interrupted) and the VMM is still there.
pub(super) fn idle_and_connected(fd: i32) -> bool {
    matches!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::EAGAIN | libc::EINTR)
    ) && peer_connected(fd)
}

/// What one wait on the VMM's socket brought.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Batch {
    /// This many frames are in the buffers.
    Frames(usize),
    /// Nothing within the receive timeout.
    Idle,
    /// The VMM is gone.
    Closed,
}

/// Waits (up to the socket's receive timeout) for one datagram, then
/// takes whatever else is already queued, up to `bufs.len()`.
pub(super) fn recv_batch(fd: i32, bufs: &mut [Vec<u8>], lens: &mut [usize]) -> Batch {
    let mut count = 0;
    while count < bufs.len() {
        let flags = if count == 0 { 0 } else { libc::MSG_DONTWAIT };
        let b = &mut bufs[count];
        // SAFETY: receiving into our own buffer of its length.
        let n = unsafe { libc::recv(fd, b.as_mut_ptr().cast(), b.len(), flags) };
        if n > 0 {
            lens[count] = n as usize;
            count += 1;
            continue;
        }
        if count > 0 {
            break;
        }
        let err = std::io::Error::last_os_error().raw_os_error();
        return match (n, err) {
            (0, _) => Batch::Closed,
            (_, Some(libc::EAGAIN | libc::EINTR)) if peer_connected(fd) => Batch::Idle,
            _ => Batch::Closed,
        };
    }
    Batch::Frames(count)
}

/// The VMM's end is still there. The ECONNRESET a vanished peer leaves
/// is reported to whichever call comes first, often a send from the
/// vmnet thread, after which recv only times out; getpeername keeps
/// saying ENOTCONN.
fn peer_connected(fd: i32) -> bool {
    // SAFETY: zeroed sockaddr storage and its length are valid out-params.
    let mut addr: libc::sockaddr_storage = unsafe { std::mem::zeroed() };
    let mut len = size_of::<libc::sockaddr_storage>() as libc::socklen_t;
    // SAFETY: valid fd and out-params.
    unsafe { libc::getpeername(fd, (&raw mut addr).cast(), &raw mut len) == 0 }
}

/// Sends one frame to the VMM. A datagram send into a full socket
/// fails with ENOBUFS instead of waiting, so wait here (about 20 ms at
/// most) rather than drop the frame and make TCP retransmit it.
pub(super) fn send_frame(fd: i32, frame: &[u8]) {
    for _ in 0..1000 {
        // SAFETY: sending from a valid slice.
        if unsafe { libc::send(fd, frame.as_ptr().cast(), frame.len(), 0) } >= 0 {
            return;
        }
        match std::io::Error::last_os_error().raw_os_error() {
            Some(libc::ENOBUFS | libc::EAGAIN | libc::EINTR) => {
                std::thread::sleep(std::time::Duration::from_micros(20));
            }
            _ => return,
        }
    }
}

/// Frames from the VMM to vmnet, on this thread, until the VMM goes:
/// each batch is handed to vmnet in one call.
fn relay_to_vmnet(fd: i32, iface: interface_ref, size: usize, batch: usize) {
    let mut bufs: Vec<Vec<u8>> = (0..batch).map(|_| vec![0u8; size]).collect();
    let mut lens = vec![0usize; batch];
    loop {
        let count = match recv_batch(fd, &mut bufs, &mut lens) {
            Batch::Frames(n) => n,
            Batch::Idle => continue,
            Batch::Closed => return,
        };
        let mut batch: Vec<(&mut Vec<u8>, usize)> = bufs
            .iter_mut()
            .zip(lens.iter().copied())
            .take(count)
            .collect();
        write_frames(iface, &mut batch);
    }
}

/// Hands frames in our own buffers to vmnet in one call.
fn write_frames(iface: interface_ref, frames: &mut [(&mut Vec<u8>, usize)]) {
    let mut iovs: Vec<libc::iovec> = frames
        .iter_mut()
        .map(|(b, n)| libc::iovec {
            iov_base: b.as_mut_ptr().cast(),
            iov_len: *n,
        })
        .collect();
    let mut pkts: Vec<vmpktdesc> = iovs
        .iter_mut()
        .map(|v| vmpktdesc {
            vm_pkt_size: v.iov_len,
            vm_pkt_iov: v,
            vm_pkt_iovcnt: 1,
            vm_flags: 0,
        })
        .collect();
    let mut n = pkts.len() as i32;
    // SAFETY: every descriptor points at one of our filled buffers.
    unsafe { vmnet_write(iface, pkts.as_mut_ptr(), &raw mut n) };
}
