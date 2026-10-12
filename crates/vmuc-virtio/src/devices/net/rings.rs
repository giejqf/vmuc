// SPDX-License-Identifier: Apache-2.0
//! virtio-net over the helper's shared rings (ADR 0001): transmit copies
//! each frame from guest memory into the helper-bound ring, receive copies
//! frames from the VMM-bound ring into the guest's buffers, and the socket
//! only carries kicks. A frame waiting for receive buffers stays in the
//! ring; the helper stops reading from vmnet when the ring fills.

use std::os::fd::AsRawFd;
use std::sync::Arc;

use vmuc_net::handoff;
use vmuc_net::region::Region;
use vmuc_net::ring::{Consumer, Direction, MAX_FRAME, Next, Producer};

use super::{Delivery, HDR_LEN, Loop, RX, TX, deliver, offload};
use crate::devices::Segments;

pub(super) struct Rings {
    tx: Producer,
    rx: Consumer,
    /// The VMM-bound ring was found empty: only then may the loop ask to
    /// be kicked and sleep (otherwise it is waiting for guest buffers).
    rx_drained: bool,
    /// The helper wrote something impossible: receive stops.
    broken: bool,
}

impl Rings {
    pub(super) fn new(region: &Arc<Region>) -> Self {
        Self {
            tx: Producer::new(Arc::clone(region), Direction::ToHelper),
            rx: Consumer::new(Arc::clone(region), Direction::ToVmm),
            rx_drained: false,
            broken: false,
        }
    }
}

impl Loop {
    /// Kicks pile up as datagrams; reading them is all there is to it.
    pub(super) fn drain_kicks(&mut self) {
        let mut b = [0u8; 64];
        while self.socket.recv(&mut b).is_ok() {}
        self.readable = false;
    }

    /// Before the loop sleeps: `false` when frames arrived meanwhile.
    pub(super) fn may_sleep(&mut self) -> bool {
        match &mut self.rings {
            Some(r) if self.ctx.is_some() && r.rx_drained && !r.broken => r.rx.prepare_sleep(),
            _ => true,
        }
    }

    pub(super) fn transmit_rings(&mut self) {
        let (Some(mut ctx), Some(rings)) = (self.ctx.take(), self.rings.as_mut()) else {
            return;
        };
        let mem = ctx.mem.clone();
        let socket = self.socket.as_raw_fd();
        loop {
            let popped = self
                .tx_pending
                .take()
                .or_else(|| ctx.queues[TX].pop(&mem).ok().flatten());
            let Some(chain) = popped else {
                let _ = ctx.queues[TX].publish_used(&mem);
                if ctx.queues[TX].enable_notification(&mem).unwrap_or(false) {
                    continue;
                }
                break;
            };
            let readable = Segments::readable(&chain);
            let frame = if self.offload {
                readable
            } else {
                readable.split_at(HDR_LEN as u64).1
            };
            let len = frame.len() as usize;
            if len > MAX_FRAME {
                tracing::debug!(len, "virtio-net: transmit frame too large, dropped");
                let _ = ctx.queues[TX].add_used(&mem, chain.head, 0);
                continue;
            }
            let Some(slot) = rings.tx.reserve(len) else {
                // Let the helper drain what is there, then wait for room.
                if rings.tx.publish() {
                    handoff::kick(socket);
                }
                self.tx_pending = Some(chain);
                if rings.tx.prepare_wait(len) {
                    break;
                }
                continue;
            };
            // SAFETY: the slot has `len` writable bytes, which the helper
            // does not read until they are published.
            let dst = unsafe { std::slice::from_raw_parts_mut(slot.data(), len) };
            let n = frame.read_into(&mem, dst).unwrap_or(0);
            rings.tx.commit(&slot, n);
            let _ = ctx.queues[TX].add_used(&mem, chain.head, 0);
        }
        if rings.tx.publish() {
            handoff::kick(socket);
        }
        let _ = ctx.queues[TX].publish_used(&mem);
        if ctx.queues[TX].needs_interrupt(&mem).unwrap_or(true) {
            ctx.irq.signal_used();
        }
        self.ctx = Some(ctx);
    }

    pub(super) fn receive_rings(&mut self) {
        let (Some(mut ctx), Some(rings)) = (self.ctx.take(), self.rings.as_mut()) else {
            return;
        };
        if rings.broken {
            self.ctx = Some(ctx);
            return;
        }
        let mem = ctx.mem.clone();
        let features = ctx.features;
        let mut used_any = false;
        rings.rx_drained = false;
        loop {
            let popped = self
                .rx_spare
                .pop_front()
                .or_else(|| ctx.queues[RX].pop(&mem).ok().flatten());
            let Some(first) = popped else {
                if ctx.queues[RX].enable_notification(&mem).unwrap_or(false) {
                    continue;
                }
                break;
            };
            let (data, len) = match rings.rx.peek() {
                Next::Frame { data, len } => (data, len),
                Next::Empty => {
                    self.rx_spare.push_front(first);
                    rings.rx_drained = true;
                    break;
                }
                Next::Corrupt(why) => {
                    tracing::error!(
                        "virtio-net: the helper's ring is corrupt ({why}); receive stops"
                    );
                    rings.broken = true;
                    self.rx_spare.push_front(first);
                    break;
                }
            };
            let frame: &mut [u8] = if self.offload {
                // SAFETY: the frame is ours until released; the helper only
                // writes past the published head.
                let f = unsafe { std::slice::from_raw_parts_mut(data.cast_mut(), len) };
                if let offload::Fit::Drop(why) = offload::fit_for_guest(f, features) {
                    tracing::debug!(len, "virtio-net: dropped a frame with {why}");
                    rings.rx.advance();
                    self.rx_spare.push_front(first);
                    continue;
                }
                f
            } else {
                let total = HDR_LEN + len;
                if self.frame.len() < total {
                    self.frame.resize(total, 0);
                }
                self.frame[..HDR_LEN].fill(0);
                // SAFETY: `len` readable bytes at `data` until released.
                unsafe {
                    std::ptr::copy_nonoverlapping(data, self.frame[HDR_LEN..].as_mut_ptr(), len);
                }
                &mut self.frame[..total]
            };
            match deliver(&mut ctx, &mut self.rx_spare, first, frame) {
                Delivery::Done => {
                    rings.rx.advance();
                    used_any = true;
                }
                Delivery::Dropped => rings.rx.advance(),
                Delivery::Wait => {
                    if ctx.queues[RX].enable_notification(&mem).unwrap_or(false) {
                        continue;
                    }
                    break;
                }
            }
        }
        if rings.rx.release() {
            handoff::kick(self.socket.as_raw_fd());
        }
        if used_any {
            let _ = ctx.queues[RX].publish_used(&mem);
            if ctx.queues[RX].needs_interrupt(&mem).unwrap_or(true) {
                ctx.irq.signal_used();
            }
        }
        self.ctx = Some(ctx);
    }
}
