// SPDX-License-Identifier: Apache-2.0
//! The helper's relay through the shared rings (ADR 0001): vmnet's packet
//! callback fills the VMM-bound ring straight from `vmnet_read`; this
//! thread drains the helper-bound ring into `vmnet_write` and sleeps on
//! the socketpair between kicks. Everything read from the region was
//! written by the unprivileged VMM and is checked by `ring` first; a
//! corrupt ring ends the relay.

use std::ffi::c_void;
use std::os::fd::OwnedFd;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::handoff;
use crate::region::Region;
use crate::ring::{Consumer, Direction, MAX_FRAME, Producer, REGION_BYTES, Slot};
use crate::ringrelay::{self, Filled, In, Out};
use crate::vmnet::{
    VMNET_INTERFACE_PACKETS_AVAILABLE, VMNET_SUCCESS, dispatch_async_f, dispatch_queue_t,
    event_block, interface_ref, vmnet_interface_set_event_callback, vmnet_read, vmnet_write,
    vmpktdesc, xpc_object_t,
};

/// Frames vmnet hands over per fill: a batch of 64 KiB slots should
/// leave most of the 4 MiB ring to the VMM.
const RX_BATCH: usize = 16;

/// The VMM-bound side, owned by vmnet's serial dispatch queue.
struct Rx {
    iface: interface_ref,
    socket: i32,
    producer: Producer,
    capacity: usize,
    batch: usize,
    slots: Vec<Slot>,
    room: Vec<In>,
    /// Set while the ring is full and reading has stopped; the relay
    /// thread restarts reading once the VMM kicks.
    paused: Arc<AtomicBool>,
}

impl Rx {
    /// Reads from vmnet into the ring until vmnet is empty or the ring is
    /// full.
    fn fill(&mut self) {
        loop {
            let iface = self.iface;
            let filled = ringrelay::fill(
                &mut self.producer,
                self.batch,
                self.capacity,
                &mut self.slots,
                &mut self.room,
                |room| read(iface, room),
            );
            match filled {
                Filled::Frames { kick, .. } => {
                    if kick {
                        handoff::kick(self.socket);
                    }
                }
                Filled::Empty => return,
                Filled::Full => {
                    // Paused first, then the request to be kicked: a kick
                    // between the two would otherwise find nothing paused.
                    self.paused.store(true, Ordering::SeqCst);
                    if self.producer.prepare_wait(self.capacity) {
                        return;
                    }
                    self.paused.store(false, Ordering::SeqCst);
                }
            }
        }
    }
}

/// One `vmnet_read` into ring slots; returns how many frames came.
fn read(iface: interface_ref, room: &mut [In]) -> usize {
    let mut iovs: Vec<libc::iovec> = room
        .iter()
        .map(|&(data, cap)| libc::iovec {
            iov_base: data.cast(),
            iov_len: cap,
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
    let mut count = pkts.len() as i32;
    // SAFETY: every descriptor points at a reserved slot of its capacity.
    let st = unsafe { vmnet_read(iface, pkts.as_mut_ptr(), &raw mut count) };
    if st != VMNET_SUCCESS || count <= 0 {
        return 0;
    }
    let n = (count as usize).min(room.len());
    for (r, p) in room.iter_mut().zip(&pkts).take(n) {
        r.1 = p.vm_pkt_size;
    }
    n
}

unsafe extern "C" fn on_packets(ctx: *mut c_void, _mask: u32, _event: xpc_object_t) {
    // SAFETY: `ctx` is the leaked `Rx`, used only on vmnet's serial queue.
    unsafe { &mut *ctx.cast::<Rx>() }.fill();
}

unsafe extern "C" fn resume(ctx: *mut c_void) {
    // SAFETY: as above; dispatched onto the same serial queue.
    unsafe { &mut *ctx.cast::<Rx>() }.fill();
}

/// One `vmnet_write` of frames still in the ring.
fn write(iface: interface_ref, frames: &[Out]) {
    let mut iovs: Vec<libc::iovec> = frames
        .iter()
        .map(|&(data, len)| libc::iovec {
            iov_base: data.cast_mut().cast(),
            iov_len: len,
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
    // SAFETY: the descriptors point at validated frames in the mapped ring;
    // vmnet only reads them, and copies them before parsing.
    unsafe { vmnet_write(iface, pkts.as_mut_ptr(), &raw mut n) };
}

/// Relays through the rings in `region` until the VMM goes or corrupts
/// them. `capacity`: largest frame vmnet hands over (header included).
///
/// # Safety
/// `iface` is a started interface and `queue` its event queue.
pub(super) unsafe fn run(
    socket: i32,
    iface: interface_ref,
    queue: dispatch_queue_t,
    region: &OwnedFd,
    capacity: usize,
    read_max: usize,
    write_max: usize,
) -> Result<(), String> {
    let region =
        Arc::new(Region::from_fd(region, REGION_BYTES).map_err(|e| format!("shared region: {e}"))?);
    let paused = Arc::new(AtomicBool::new(false));
    let rx = Box::leak(Box::new(Rx {
        iface,
        socket,
        producer: Producer::new(Arc::clone(&region), Direction::ToVmm),
        capacity: capacity.min(MAX_FRAME),
        batch: read_max.min(RX_BATCH),
        slots: Vec::new(),
        room: Vec::new(),
        paused: Arc::clone(&paused),
    }));
    let ctx: *mut c_void = std::ptr::from_mut(rx).cast();
    // SAFETY: a started interface, its queue, and a context that lives as
    // long as the process.
    let registered = unsafe {
        let cb = event_block(on_packets, ctx);
        vmnet_interface_set_event_callback(iface, VMNET_INTERFACE_PACKETS_AVAILABLE, queue, cb)
    };
    if registered != VMNET_SUCCESS {
        return Err("vmnet_interface_set_event_callback failed".into());
    }

    let mut consumer = Consumer::new(region, Direction::ToHelper);
    let mut frames = Vec::new();
    let mut kicks = [0u8; 64];
    loop {
        loop {
            let (n, kick) =
                ringrelay::drain(&mut consumer, write_max, &mut frames, |f| write(iface, f))
                    .map_err(|why| format!("the VMM's ring is corrupt: {why}"))?;
            if kick {
                handoff::kick(socket);
            }
            if n == 0 {
                break;
            }
        }
        if !consumer.prepare_sleep() {
            continue;
        }
        // Until a kick, the receive timeout, or the VMM's departure.
        // SAFETY: receiving into our own buffer.
        let n = unsafe { libc::recv(socket, kicks.as_mut_ptr().cast(), kicks.len(), 0) };
        if n == 0 || (n < 0 && !super::relay::idle_and_connected(socket)) {
            return Ok(());
        }
        // SAFETY: as above; drains kicks that piled up.
        while unsafe {
            libc::recv(
                socket,
                kicks.as_mut_ptr().cast(),
                kicks.len(),
                libc::MSG_DONTWAIT,
            )
        } > 0
        {}
        if paused.swap(false, Ordering::SeqCst) {
            // SAFETY: `ctx` lives for the process; `resume` runs on the
            // interface's serial queue like the packet callback.
            unsafe { dispatch_async_f(queue, ctx, resume) };
        }
    }
}
