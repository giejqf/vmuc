// SPDX-License-Identifier: Apache-2.0
//! virtio-net over a datagram socket: one Ethernet frame per datagram.
//!
//! The other end of the socket is `vmuc-vmnet-helper`, which relays
//! frames to a vmnet interface (or a test peer). One thread runs a mio
//! loop over the queue doorbell and the socket. Transmit is zero-copy:
//! the frame goes from the guest's buffers to `sendmsg` with the 12-byte
//! virtio-net header skipped; a full socket keeps the chain for the next
//! writable event. Receive copies each datagram once into the guest's
//! buffers (several with mergeable receive buffers); while the guest has
//! posted none, frames wait in the kernel socket.
//!
//! When the peer speaks virtio-net headers too (vmnet on macOS 15.4+),
//! each datagram carries the 12-byte header and the device offers
//! checksum and TCP segmentation offload both ways (see `offload`).
//! Otherwise datagrams are bare frames and checksums are the guest's job.

use std::collections::VecDeque;
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixDatagram;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};

use mio::{Events, Interest, Poll, Token, Waker};

use crate::bus::put_le;
use crate::device::{
    Activation, Doorbell, VirtioDevice, WakerDoorbell, WorkerCtx, device_type, features,
};
use crate::devices::Segments;
use crate::queue::DescChain;
use vmuc_net::region::Region;

mod offload;
mod rings;

const F_MTU: u64 = 1 << 3;
const F_MAC: u64 = 1 << 5;
const F_MRG_RXBUF: u64 = 1 << 15;
const F_STATUS: u64 = 1 << 16;

const RX: usize = 0;
const TX: usize = 1;
/// `struct virtio_net_hdr_v1`, always 12 bytes with VERSION_1.
const HDR_LEN: usize = 12;
const MTU: u16 = 1500;
/// Largest frame accepted from the socket (jumbo-safe).
const MAX_FRAME: usize = 65_550;
/// Socket buffer bytes per direction. A unix datagram socket holds 4 KiB
/// on macOS by default, two frames, and caps a datagram at 2 KiB, too
/// small for bursts or for 64 KiB offload segments.
const SOCKET_BUFFER: libc::c_int = 4 << 20;
const WAKER: Token = Token(0);
const SOCKET: Token = Token(1);
const CONTROL: u32 = 1 << 31;

/// Traffic through the NIC since it was created, for `vmuc stat`.
/// Bytes are Ethernet frames, without the virtio-net header.
#[derive(Debug, Default)]
pub struct NetCounters {
    pub rx_bytes: AtomicU64,
    pub rx_frames: AtomicU64,
    pub tx_bytes: AtomicU64,
    pub tx_frames: AtomicU64,
    /// Frames dropped in either direction.
    pub dropped: AtomicU64,
}

impl NetCounters {
    fn sent(&self, frame_bytes: usize) {
        self.tx_frames.fetch_add(1, Ordering::Relaxed);
        self.tx_bytes
            .fetch_add(frame_bytes as u64, Ordering::Relaxed);
    }

    fn received(&self, frame_bytes: usize) {
        self.rx_frames.fetch_add(1, Ordering::Relaxed);
        self.rx_bytes
            .fetch_add(frame_bytes as u64, Ordering::Relaxed);
    }

    fn drop_one(&self) {
        self.dropped.fetch_add(1, Ordering::Relaxed);
    }
}

enum Ctrl {
    Activate(WorkerCtx),
    Reset(Sender<()>),
    Shutdown,
}

struct Loop {
    /// Datagrams carry the virtio-net header (offloads on).
    offload: bool,
    counters: Arc<NetCounters>,
    /// Frames travel through shared rings; the socket only kicks.
    rings: Option<rings::Rings>,
    poll: Poll,
    socket: mio::net::UnixDatagram,
    doorbell: Arc<WakerDoorbell>,
    ctrl: Receiver<Ctrl>,
    ctx: Option<WorkerCtx>,
    /// A transmit chain the socket could not take yet.
    tx_pending: Option<DescChain>,
    /// Receive chains popped but not filled (the socket ran dry, or a
    /// frame needed more buffers than were posted): used first next time.
    rx_spare: VecDeque<DescChain>,
    /// The socket may hold frames (edge-triggered readiness remembered).
    readable: bool,
    /// Length of a frame in `frame` waiting for the guest to post enough
    /// receive buffers (a 64 KiB TSO segment needs sixteen 4 KiB ones).
    rx_pending: Option<usize>,
    frame: Vec<u8>,
}

impl Loop {
    fn run(mut self) {
        let mut events = Events::with_capacity(16);
        loop {
            let timeout = (!self.may_sleep()).then_some(std::time::Duration::ZERO);
            if let Err(e) = self.poll.poll(&mut events, timeout) {
                if e.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                tracing::error!("virtio-net poll: {e}");
                return;
            }
            let mut bits = 0;
            let mut writable = false;
            for ev in &events {
                if ev.token() == WAKER {
                    bits |= self.doorbell.take();
                } else {
                    self.readable |= ev.is_readable();
                    writable |= ev.is_writable();
                }
            }
            if bits & CONTROL != 0 && !self.control() {
                return;
            }
            if self.ctx.is_none() {
                continue;
            }
            if self.rings.is_some() {
                if self.readable {
                    self.drain_kicks();
                }
                self.transmit_rings();
                self.receive_rings();
                continue;
            }
            if bits & (1 << TX) != 0 || writable {
                self.transmit();
            }
            self.receive();
        }
    }

    fn control(&mut self) -> bool {
        while let Ok(msg) = self.ctrl.try_recv() {
            match msg {
                Ctrl::Activate(ctx) => {
                    self.ctx = Some(ctx);
                    self.readable = true;
                }
                Ctrl::Reset(ack) => {
                    self.ctx = None;
                    self.tx_pending = None;
                    self.rx_spare.clear();
                    self.rx_pending = None;
                    let _ = ack.send(());
                }
                Ctrl::Shutdown => return false,
            }
        }
        true
    }

    /// Sends a chain's frame (header skipped) straight from guest memory.
    /// `Ok(false)` when the socket is full.
    fn send_chain(&self, ctx: &WorkerCtx, chain: &DescChain) -> io::Result<bool> {
        let readable = Segments::readable(chain);
        let frame = if self.offload {
            readable
        } else {
            readable.split_at(HDR_LEN as u64).1
        };
        let mut iov = frame.iovecs(&ctx.mem).map_err(io::Error::other)?;
        // SAFETY: a zeroed msghdr is valid; we fill the iovec fields.
        let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
        msg.msg_iov = iov.as_mut_ptr();
        msg.msg_iovlen = iov.len().min(1024) as _;
        // SAFETY: the iovecs point into guest buffers validated above.
        let n = unsafe { libc::sendmsg(self.socket.as_raw_fd(), &raw const msg, 0) };
        if n >= 0 {
            let hdr = if self.offload { HDR_LEN } else { 0 };
            self.counters.sent((n as usize).saturating_sub(hdr));
            return Ok(true);
        }
        let e = io::Error::last_os_error();
        match e.kind() {
            io::ErrorKind::WouldBlock => Ok(false),
            // A frame larger than the socket allows: drop it, as a NIC would.
            _ if e.raw_os_error() == Some(libc::EMSGSIZE) => {
                self.counters.drop_one();
                Ok(true)
            }
            _ => Err(e),
        }
    }

    fn transmit(&mut self) {
        let Some(mut ctx) = self.ctx.take() else {
            return;
        };
        let mem = ctx.mem.clone();
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
            match self.send_chain(&ctx, &chain) {
                Ok(true) => {
                    let _ = ctx.queues[TX].add_used(&mem, chain.head, 0);
                }
                Ok(false) => {
                    self.tx_pending = Some(chain);
                    let _ = ctx.queues[TX].publish_used(&mem);
                    break;
                }
                Err(e) => {
                    tracing::warn!("virtio-net send: {e}");
                    let _ = ctx.queues[TX].add_used(&mem, chain.head, 0);
                }
            }
        }
        if ctx.queues[TX].needs_interrupt(&mem).unwrap_or(true) {
            ctx.irq.signal_used();
        }
        self.ctx = Some(ctx);
    }

    /// Takes the next datagram into `frame`, header first; its length,
    /// or `None` when the socket is empty (`readable` cleared) or the frame
    /// was dropped (`readable` left set).
    fn next_frame(&mut self, features: u64) -> Option<usize> {
        self.frame.resize(HDR_LEN + MAX_FRAME, 0);
        // With offloads the datagram brings its own header.
        let skip = if self.offload { 0 } else { HDR_LEN };
        let n = match self.socket.recv(&mut self.frame[skip..]) {
            Ok(n) => n,
            Err(e) => {
                if e.kind() == io::ErrorKind::WouldBlock {
                    self.readable = false;
                } else {
                    tracing::warn!("virtio-net recv: {e}");
                    self.readable = false;
                }
                return None;
            }
        };
        let total = skip + n;
        if self.offload {
            if let offload::Fit::Drop(why) =
                offload::fit_for_guest(&mut self.frame[..total], features)
            {
                tracing::debug!(len = n, "virtio-net: dropped a frame with {why}");
                self.counters.drop_one();
                return None;
            }
        } else {
            self.frame[..HDR_LEN].fill(0);
        }
        Some(total)
    }

    fn receive(&mut self) {
        if !self.readable && self.rx_pending.is_none() {
            return;
        }
        let Some(mut ctx) = self.ctx.take() else {
            return;
        };
        let mem = ctx.mem.clone();
        let mut used_any = false;
        loop {
            // A buffer first: a frame taken off the socket cannot go back.
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
            let features = ctx.features;
            let Some(total) = self.rx_pending.take().or_else(|| self.next_frame(features)) else {
                self.rx_spare.push_front(first);
                if self.readable {
                    continue; // a frame was dropped; try the next
                }
                break;
            };
            match deliver(
                &mut ctx,
                &mut self.rx_spare,
                first,
                &mut self.frame[..total],
            ) {
                Delivery::Done => self.counters.received(total - HDR_LEN),
                Delivery::Dropped => {
                    self.counters.drop_one();
                    continue;
                }
                Delivery::Wait => {
                    // Hold the frame until the guest posts more buffers:
                    // the socket fills meanwhile and the helper waits,
                    // rather than TCP retransmitting a dropped segment.
                    self.rx_pending = Some(total);
                    if ctx.queues[RX].enable_notification(&mem).unwrap_or(false) {
                        continue;
                    }
                    break;
                }
            }
            used_any = true;
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

/// What became of a frame offered to the guest.
#[derive(Debug, PartialEq, Eq)]
enum Delivery {
    Done,
    /// Not enough buffers posted yet: try again when the guest adds some.
    Wait,
    /// More than the guest's buffers could ever hold.
    Dropped,
}

/// Spreads `frame` (virtio-net header first) over the guest's receive
/// buffers: `first`, then as many more as it needs (mergeable buffers).
/// On `Wait` and `Dropped` the buffers go back to `spare`, in order.
fn deliver(
    ctx: &mut WorkerCtx,
    spare: &mut VecDeque<DescChain>,
    first: DescChain,
    frame: &mut [u8],
) -> Delivery {
    let mem = ctx.mem.clone();
    let mergeable = ctx.features & F_MRG_RXBUF != 0;
    let total = frame.len();
    let mut room = Segments::writable(&first).len();
    let mut chains = vec![first];
    while room < total as u64 && mergeable {
        let next = spare
            .pop_front()
            .or_else(|| ctx.queues[RX].pop(&mem).ok().flatten());
        let Some(next) = next else { break };
        room += Segments::writable(&next).len();
        chains.push(next);
    }
    if room < total as u64 {
        let never = !mergeable || chains.len() >= usize::from(ctx.queues[RX].size);
        for c in chains.into_iter().rev() {
            spare.push_front(c);
        }
        if never {
            tracing::debug!(
                len = total,
                "virtio-net: frame dropped, larger than the ring"
            );
            return Delivery::Dropped;
        }
        return Delivery::Wait;
    }
    let count = chains.len() as u16;
    frame[10..12].copy_from_slice(&count.to_le_bytes());
    let mut at = 0;
    for c in &chains {
        let out = Segments::writable(c);
        let take = (out.len() as usize).min(total - at);
        let wrote = out.write_from(&mem, &frame[at..at + take]).unwrap_or(0);
        at += wrote;
        let _ = ctx.queues[RX].add_used(&mem, c.head, wrote as u32);
    }
    Delivery::Done
}

pub struct Net {
    offload: bool,
    counters: Arc<NetCounters>,
    config: [u8; 12],
    doorbell: Arc<WakerDoorbell>,
    ctrl: Sender<Ctrl>,
    thread: Option<JoinHandle<()>>,
}

impl Net {
    /// A NIC with `mac` whose frames travel over `socket` (a connected
    /// `SOCK_DGRAM` unix socket, one frame per datagram), or through the
    /// shared rings in `rings` with the socket carrying only kicks.
    /// `offload`: the peer reads and writes the virtio-net header with
    /// every frame.
    pub fn new(
        mac: [u8; 6],
        socket: UnixDatagram,
        offload: bool,
        rings: Option<&Arc<Region>>,
    ) -> io::Result<Self> {
        size_buffers(&socket)?;
        socket.set_nonblocking(true)?;
        let poll = Poll::new()?;
        let doorbell = Arc::new(WakerDoorbell::new(Waker::new(poll.registry(), WAKER)?, 2));
        let mut socket = mio::net::UnixDatagram::from_std(socket);
        poll.registry()
            .register(&mut socket, SOCKET, Interest::READABLE | Interest::WRITABLE)?;
        let (ctrl, rx) = mpsc::channel();
        let counters = Arc::new(NetCounters::default());
        let event_loop = Loop {
            offload,
            counters: Arc::clone(&counters),
            rings: rings.map(rings::Rings::new),
            poll,
            socket,
            doorbell: Arc::clone(&doorbell),
            ctrl: rx,
            ctx: None,
            tx_pending: None,
            rx_spare: VecDeque::new(),
            readable: false,
            rx_pending: None,
            frame: Vec::new(),
        };
        let thread = thread::Builder::new()
            .name("virtio-net".into())
            .spawn(move || event_loop.run())?;
        let mut config = [0u8; 12];
        config[0..6].copy_from_slice(&mac);
        config[6..8].copy_from_slice(&1u16.to_le_bytes()); // VIRTIO_NET_S_LINK_UP
        config[8..10].copy_from_slice(&1u16.to_le_bytes()); // one queue pair
        config[10..12].copy_from_slice(&MTU.to_le_bytes());
        Ok(Self {
            offload,
            counters,
            config,
            doorbell,
            ctrl,
            thread: Some(thread),
        })
    }

    /// Traffic counters, live.
    pub fn counters(&self) -> Arc<NetCounters> {
        Arc::clone(&self.counters)
    }

    fn send(&self, msg: Ctrl) -> bool {
        let ok = self.ctrl.send(msg).is_ok();
        self.doorbell.ring_bits(CONTROL);
        ok
    }
}

impl Drop for Net {
    fn drop(&mut self) {
        self.send(Ctrl::Shutdown);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl VirtioDevice for Net {
    fn device_type(&self) -> u32 {
        device_type::NET
    }
    fn features(&self) -> u64 {
        features::RING_EVENT_IDX
            | features::RING_INDIRECT_DESC
            | F_MAC
            | F_STATUS
            | F_MRG_RXBUF
            | F_MTU
            | if self.offload { offload::OFFERED } else { 0 }
    }
    fn queue_max_sizes(&self) -> Vec<u16> {
        // Receive gets room for many 64 KiB segments in 4 KiB buffers.
        vec![1024, 256]
    }
    fn read_config(&self, offset: u64, data: &mut [u8]) {
        let off = offset as usize;
        if off + data.len() <= self.config.len() {
            data.copy_from_slice(&self.config[off..off + data.len()]);
        } else {
            put_le(data, 0);
        }
    }
    fn activate(&mut self, act: Activation) -> Result<(), String> {
        let ctx = WorkerCtx {
            mem: act.mem,
            queues: act.queues,
            irq: act.irq,
            features: act.features,
        };
        if !self.send(Ctrl::Activate(ctx)) {
            return Err("virtio-net loop is gone".into());
        }
        self.doorbell.ring_bits(0b11);
        Ok(())
    }
    fn reset(&mut self) {
        let (tx, rx) = mpsc::channel();
        if self.send(Ctrl::Reset(tx)) {
            let _ = rx.recv();
        }
    }
    fn doorbell(&self) -> Arc<dyn Doorbell> {
        self.doorbell.clone()
    }
}

/// Gives the device's end of the socket room for bursts and large frames.
fn size_buffers(socket: &UnixDatagram) -> io::Result<()> {
    let bytes = SOCKET_BUFFER;
    for opt in [libc::SO_SNDBUF, libc::SO_RCVBUF] {
        // SAFETY: a valid socket and a c_int option value.
        let rc = unsafe {
            libc::setsockopt(
                socket.as_raw_fd(),
                libc::SOL_SOCKET,
                opt,
                (&raw const bytes).cast(),
                size_of::<libc::c_int>() as libc::socklen_t,
            )
        };
        if rc != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

/// `0a:56:4d:d2:a4:d4` → bytes.
pub fn parse_mac(s: &str) -> Option<[u8; 6]> {
    let mut mac = [0u8; 6];
    let mut parts = s.split(':');
    for b in &mut mac {
        *b = u8::from_str_radix(parts.next()?, 16).ok()?;
    }
    parts.next().is_none().then_some(mac)
}

#[cfg(test)]
mod tests;
