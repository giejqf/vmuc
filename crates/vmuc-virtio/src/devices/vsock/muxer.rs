// SPDX-License-Identifier: Apache-2.0
//! The vsock event loop: one thread owns the queues and every host-side
//! socket, waiting on both with one `mio::Poll` (queue kicks arrive
//! through a `mio::Waker`).
//!
//! Host data is read straight into guest receive buffers and guest data
//! written straight from transmit buffers (`readv`/`writev`); only what
//! a slow host socket cannot take is copied, into the connection's
//! buffer. Readiness is edge-triggered, so a connection stays marked
//! readable until a read returns WouldBlock, and a receive chain popped
//! for a read that then would block is kept for the next packet.

use std::collections::{HashMap, VecDeque};
use std::io;
use std::os::fd::AsRawFd;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender};

use mio::net::UnixStream;
use mio::{Events, Interest, Poll, Token};

use super::conn::{BUF_ALLOC, Conn, OnConnect, State};
use super::packet::{HDR_LEN, HOST_CID, Hdr, SHUTDOWN_RCV, SHUTDOWN_SEND, TYPE_STREAM, op};
use crate::device::{WakerDoorbell, WorkerCtx};
use crate::devices::Segments;
use crate::mem::GuestMemory;
use crate::queue::DescChain;

pub const WAKER: Token = Token(0);
pub const RX: usize = 0;
pub const TX: usize = 1;
pub const CONTROL: u32 = 1 << 31;
const FIRST_HOST_PORT: u32 = 1024;

pub enum Ctrl {
    Activate(WorkerCtx),
    Reset(Sender<()>),
    Connect(std::os::unix::net::UnixStream, u32, OnConnect),
    Shutdown,
}

pub struct Muxer {
    pub poll: Poll,
    pub guest_cid: u64,
    pub ctrl: Receiver<Ctrl>,
    pub doorbell: Arc<WakerDoorbell>,
    ctx: Option<WorkerCtx>,
    conns: HashMap<u32, Conn>,
    /// Packets without payload waiting for a receive buffer.
    replies: VecDeque<Hdr>,
    spare: Option<DescChain>,
    next_port: u32,
}

fn rst_for(h: &Hdr) -> Hdr {
    Hdr {
        src_cid: h.dst_cid,
        dst_cid: h.src_cid,
        src_port: h.dst_port,
        dst_port: h.src_port,
        kind: TYPE_STREAM,
        op: op::RST,
        ..Hdr::default()
    }
}

impl Muxer {
    pub fn new(
        poll: Poll,
        guest_cid: u64,
        ctrl: Receiver<Ctrl>,
        doorbell: Arc<WakerDoorbell>,
    ) -> Self {
        Self {
            poll,
            guest_cid,
            ctrl,
            doorbell,
            ctx: None,
            conns: HashMap::new(),
            replies: VecDeque::new(),
            spare: None,
            next_port: FIRST_HOST_PORT,
        }
    }

    pub fn run(mut self) {
        let mut events = Events::with_capacity(128);
        loop {
            if let Err(e) = self.poll.poll(&mut events, None) {
                if e.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                tracing::error!("vsock poll: {e}");
                return;
            }
            let mut kicked = 0;
            for ev in &events {
                if ev.token() == WAKER {
                    kicked |= self.doorbell.take();
                    continue;
                }
                let port = ev.token().0 as u32;
                let Some(c) = self.conns.get_mut(&port) else {
                    continue;
                };
                if ev.is_readable() || ev.is_read_closed() || ev.is_error() {
                    c.readable = true;
                }
                if ev.is_writable() && !c.to_host.is_empty() {
                    self.flush(port);
                }
            }
            if kicked & CONTROL != 0 && !self.control() {
                return;
            }
            if self.ctx.is_some() {
                if kicked & (1 << TX) != 0 {
                    self.process_tx();
                }
                self.process_rx();
            }
        }
    }

    /// Handles control messages; `false` on shutdown.
    fn control(&mut self) -> bool {
        while let Ok(msg) = self.ctrl.try_recv() {
            match msg {
                Ctrl::Activate(ctx) => self.ctx = Some(ctx),
                Ctrl::Reset(ack) => {
                    self.drop_all();
                    let _ = ack.send(());
                }
                Ctrl::Connect(stream, guest_port, on_connect) => {
                    self.connect(stream, guest_port, on_connect);
                }
                Ctrl::Shutdown => {
                    self.drop_all();
                    return false;
                }
            }
        }
        true
    }

    fn drop_all(&mut self) {
        for (_, mut c) in self.conns.drain() {
            if let Some(f) = c.on_connect.take() {
                f(&mut c.stream, Err("vsock device reset".into()));
            }
        }
        self.ctx = None;
        self.replies.clear();
        self.spare = None;
    }

    fn connect(
        &mut self,
        stream: std::os::unix::net::UnixStream,
        guest_port: u32,
        on_connect: OnConnect,
    ) {
        let mut stream = UnixStream::from_std(stream);
        if self.ctx.is_none() {
            on_connect(
                &mut stream,
                Err("the guest has not set up vsock yet".into()),
            );
            return;
        }
        let mut port = self.next_port;
        while self.conns.contains_key(&port) {
            port = port.checked_add(1).unwrap_or(FIRST_HOST_PORT);
        }
        self.next_port = port.checked_add(1).unwrap_or(FIRST_HOST_PORT);
        if let Err(e) = self.poll.registry().register(
            &mut stream,
            Token(port as usize),
            Interest::READABLE | Interest::WRITABLE,
        ) {
            on_connect(&mut stream, Err(format!("register: {e}")));
            return;
        }
        let mut c = Conn::new(stream, port, guest_port, on_connect);
        self.replies
            .push_back(c.header(self.guest_cid, op::REQUEST, 0, 0));
        self.conns.insert(port, c);
    }

    /// Resets and forgets a connection, telling the host client if it
    /// never got connected.
    fn reset_conn(&mut self, port: u32, notify_guest: bool) {
        if let Some(mut c) = self.conns.remove(&port) {
            if let Some(f) = c.on_connect.take() {
                f(&mut c.stream, Err("connection refused by the guest".into()));
            }
            if notify_guest {
                self.replies
                    .push_back(c.header(self.guest_cid, op::RST, 0, 0));
            }
        }
    }

    fn flush(&mut self, port: u32) {
        let Some(c) = self.conns.get_mut(&port) else {
            return;
        };
        if c.flush().is_err() {
            return self.reset_conn(port, true);
        }
        self.after_progress(port);
    }

    /// After data moved: half-close the host side, finish the connection,
    /// or tell the guest about freed buffer space, as needed.
    fn after_progress(&mut self, port: u32) {
        let guest_cid = self.guest_cid;
        let Some(c) = self.conns.get_mut(&port) else {
            return;
        };
        if c.guest_send_done && c.to_host.is_empty() && !c.host_write_shut {
            let _ = c.stream.shutdown(std::net::Shutdown::Write);
            c.host_write_shut = true;
        }
        if c.finished() {
            return self.reset_conn(port, true);
        }
        if c.owes_credit() {
            let h = c.header(guest_cid, op::CREDIT_UPDATE, 0, 0);
            self.replies.push_back(h);
        }
    }

    fn process_tx(&mut self) {
        let Some(mut ctx) = self.ctx.take() else {
            return;
        };
        let mem = ctx.mem.clone();
        loop {
            while let Ok(Some(chain)) = ctx.queues[TX].pop(&mem) {
                let (hseg, payload) = Segments::readable(&chain).split_at(HDR_LEN as u64);
                let mut hb = [0u8; HDR_LEN];
                if hseg.read_into(&mem, &mut hb) == Ok(HDR_LEN) {
                    self.guest_packet(&Hdr::parse(&hb), &payload, &mem);
                }
                let _ = ctx.queues[TX].add_used(&mem, chain.head, 0);
            }
            let _ = ctx.queues[TX].publish_used(&mem);
            if !ctx.queues[TX].enable_notification(&mem).unwrap_or(false) {
                break;
            }
        }
        if ctx.queues[TX].needs_interrupt(&mem).unwrap_or(true) {
            ctx.irq.signal_used();
        }
        self.ctx = Some(ctx);
    }

    fn guest_packet(&mut self, h: &Hdr, payload: &Segments, mem: &GuestMemory) {
        let valid = h.kind == TYPE_STREAM && h.dst_cid == HOST_CID && h.src_cid == self.guest_cid;
        let known = valid
            && self
                .conns
                .get(&h.dst_port)
                .is_some_and(|c| c.guest_port == h.src_port);
        if !known {
            // Guest-initiated connections are not offered yet.
            if h.op != op::RST {
                self.replies.push_back(rst_for(h));
            }
            return;
        }
        let port = h.dst_port;
        let guest_cid = self.guest_cid;
        let Some(c) = self.conns.get_mut(&port) else {
            return;
        };
        c.update_peer(h);
        match h.op {
            op::RESPONSE if c.state == State::Connecting => {
                c.state = State::Established;
                if let Some(f) = c.on_connect.take() {
                    f(&mut c.stream, Ok(()));
                }
                c.readable = true;
            }
            op::RW if c.state == State::Established => {
                let (data, _) = payload.split_at(u64::from(h.len));
                if Self::deliver(c, &data, mem).is_err() || c.to_host.len() > BUF_ALLOC as usize {
                    return self.reset_conn(port, true);
                }
                self.after_progress(port);
            }
            op::SHUTDOWN => {
                c.guest_send_done |= h.flags & SHUTDOWN_SEND != 0;
                c.guest_rcv_done |= h.flags & SHUTDOWN_RCV != 0;
                self.after_progress(port);
            }
            op::CREDIT_REQUEST => {
                let reply = c.header(guest_cid, op::CREDIT_UPDATE, 0, 0);
                self.replies.push_back(reply);
            }
            op::CREDIT_UPDATE => {}
            op::RST => self.reset_conn(port, false),
            _ => self.reset_conn(port, true),
        }
    }

    /// Writes guest data to the host socket straight from guest memory;
    /// whatever the socket cannot take now is copied into the buffer.
    fn deliver(c: &mut Conn, data: &Segments, mem: &GuestMemory) -> io::Result<()> {
        let mut sent = 0u64;
        if c.to_host.is_empty() {
            let iov = data.iovecs(mem).map_err(io::Error::other)?;
            // SAFETY: the iovecs point into guest RAM validated above.
            let n = unsafe {
                libc::writev(
                    c.stream.as_raw_fd(),
                    iov.as_ptr(),
                    iov.len().min(1024) as i32,
                )
            };
            if n >= 0 {
                sent = n as u64;
                c.fwd_cnt = c.fwd_cnt.wrapping_add(n as u32);
            } else {
                let e = io::Error::last_os_error();
                if e.kind() != io::ErrorKind::WouldBlock {
                    return Err(e);
                }
            }
        }
        let (_, rest) = data.split_at(sent);
        if !rest.is_empty() {
            let at = c.to_host.len();
            c.to_host.resize(at + rest.len() as usize, 0);
            rest.read_into(mem, &mut c.to_host[at..])
                .map_err(io::Error::other)?;
        }
        Ok(())
    }

    fn next_sender(&self) -> Option<u32> {
        self.conns
            .iter()
            .filter(|(_, c)| c.wants_to_send())
            .map(|(p, _)| *p)
            .min()
    }

    fn process_rx(&mut self) {
        let Some(mut ctx) = self.ctx.take() else {
            return;
        };
        let mem = ctx.mem.clone();
        loop {
            let mut used = false;
            let mut starved = false;
            loop {
                let ctrl = !self.replies.is_empty();
                let data = if ctrl { None } else { self.next_sender() };
                if !ctrl && data.is_none() {
                    break;
                }
                let chain = if let Some(c) = self.spare.take() {
                    c
                } else {
                    let Ok(Some(c)) = ctx.queues[RX].pop(&mem) else {
                        starved = true;
                        break;
                    };
                    c
                };
                let out = Segments::writable(&chain);
                let len = if let Some(port) = data {
                    let Some(n) = self.read_host(port, &out, &mem) else {
                        self.spare = Some(chain);
                        continue;
                    };
                    n
                } else {
                    let h = self.replies.pop_front().unwrap_or_default();
                    out.write_from(&mem, &h.encode()).map_or(0, |n| n as u32)
                };
                let _ = ctx.queues[RX].add_used(&mem, chain.head, len);
                used = true;
            }
            if used {
                let _ = ctx.queues[RX].publish_used(&mem);
                if ctx.queues[RX].needs_interrupt(&mem).unwrap_or(true) {
                    ctx.irq.signal_used();
                }
            }
            // Out of buffers with work left: ask for a kick, and go again if
            // buffers slipped in before the request was visible.
            if !(starved && ctx.queues[RX].enable_notification(&mem).unwrap_or(false)) {
                break;
            }
        }
        self.ctx = Some(ctx);
    }

    /// Fills one receive chain from a host socket: an RW packet, or the
    /// SHUTDOWN/RST its EOF or error turns into. `None` when the socket
    /// would block (the chain is kept for later).
    fn read_host(&mut self, port: u32, out: &Segments, mem: &GuestMemory) -> Option<u32> {
        let guest_cid = self.guest_cid;
        let c = self.conns.get_mut(&port)?;
        let (hseg, pseg) = out.split_at(HDR_LEN as u64);
        let (pseg, _) = pseg.split_at(u64::from(c.credit()));
        if pseg.is_empty() {
            // A buffer with no room past the header: spend it on a credit
            // update rather than mistake an empty read for EOF.
            let h = c.header(guest_cid, op::CREDIT_UPDATE, 0, 0);
            hseg.write_from(mem, &h.encode()).ok()?;
            return Some(HDR_LEN as u32);
        }
        let iov = pseg.iovecs(mem).ok()?;
        // SAFETY: the iovecs point into guest receive buffers validated above.
        let n = unsafe {
            libc::readv(
                c.stream.as_raw_fd(),
                iov.as_ptr(),
                iov.len().min(1024) as i32,
            )
        };
        let h = match n {
            0 => {
                c.readable = false;
                c.host_eof = true;
                c.header(guest_cid, op::SHUTDOWN, 0, SHUTDOWN_SEND)
            }
            n if n > 0 => {
                c.tx_cnt = c.tx_cnt.wrapping_add(n as u32);
                c.header(guest_cid, op::RW, n as u32, 0)
            }
            _ if io::Error::last_os_error().kind() == io::ErrorKind::WouldBlock => {
                c.readable = false;
                return None;
            }
            _ => {
                let h = c.header(guest_cid, op::RST, 0, 0);
                self.conns.remove(&port);
                hseg.write_from(mem, &h.encode()).ok()?;
                return Some(HDR_LEN as u32);
            }
        };
        hseg.write_from(mem, &h.encode()).ok()?;
        if h.op == op::SHUTDOWN {
            self.after_progress(port);
        }
        Some(HDR_LEN as u32 + h.len)
    }
}
