// SPDX-License-Identifier: Apache-2.0
//! One host-initiated vsock stream: its host socket, its state, and the
//! credit accounting of virtio 1.2 §5.10.6.3.
//!
//! We may send the guest at most `peer_buf_alloc - (tx_cnt - peer_fwd_cnt)`
//! bytes; we advertise [`BUF_ALLOC`] and count in `fwd_cnt` what we have
//! delivered to the host socket, and buffer what the host is slow to take.

use std::io::{self, Write};

use mio::net::UnixStream;

use super::packet::{HOST_CID, Hdr, TYPE_STREAM};

/// Bytes of guest data we are willing to hold per connection.
pub const BUF_ALLOC: u32 = 256 * 1024;

/// What to tell the host client once the guest answers the REQUEST.
pub type OnConnect = Box<dyn FnOnce(&mut UnixStream, Result<(), String>) + Send>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Connecting,
    Established,
}

pub struct Conn {
    pub stream: UnixStream,
    pub host_port: u32,
    pub guest_port: u32,
    pub state: State,
    pub on_connect: Option<OnConnect>,
    /// The host socket may have data (edge-triggered readiness remembered
    /// until a read returns WouldBlock).
    pub readable: bool,
    /// Guest data not yet written to the host socket.
    pub to_host: Vec<u8>,
    pub peer_buf_alloc: u32,
    pub peer_fwd_cnt: u32,
    /// Bytes sent to the guest (wrapping).
    pub tx_cnt: u32,
    /// Guest bytes delivered to the host socket (wrapping).
    pub fwd_cnt: u32,
    /// `fwd_cnt` as of the last packet we sent the guest.
    pub fwd_cnt_sent: u32,
    pub host_eof: bool,
    pub host_write_shut: bool,
    pub guest_send_done: bool,
    pub guest_rcv_done: bool,
}

impl Conn {
    pub fn new(stream: UnixStream, host_port: u32, guest_port: u32, on_connect: OnConnect) -> Self {
        Self {
            stream,
            host_port,
            guest_port,
            state: State::Connecting,
            on_connect: Some(on_connect),
            readable: false,
            to_host: Vec::new(),
            peer_buf_alloc: 0,
            peer_fwd_cnt: 0,
            tx_cnt: 0,
            fwd_cnt: 0,
            fwd_cnt_sent: 0,
            host_eof: false,
            host_write_shut: false,
            guest_send_done: false,
            guest_rcv_done: false,
        }
    }

    /// Bytes the guest can still accept.
    pub fn credit(&self) -> u32 {
        self.peer_buf_alloc
            .saturating_sub(self.tx_cnt.wrapping_sub(self.peer_fwd_cnt))
    }

    pub fn update_peer(&mut self, h: &Hdr) {
        self.peer_buf_alloc = h.buf_alloc;
        self.peer_fwd_cnt = h.fwd_cnt;
    }

    /// Whether this connection has host data to send the guest now.
    pub fn wants_to_send(&self) -> bool {
        self.state == State::Established
            && self.readable
            && !self.host_eof
            && !self.guest_rcv_done
            && self.credit() > 0
    }

    /// The guest has not heard about enough delivered bytes to keep its
    /// sender going.
    pub fn owes_credit(&self) -> bool {
        self.fwd_cnt.wrapping_sub(self.fwd_cnt_sent) >= BUF_ALLOC / 2
    }

    /// A header from host to guest for this connection, carrying our
    /// current credit.
    pub fn header(&mut self, guest_cid: u64, op: u16, len: u32, flags: u32) -> Hdr {
        self.fwd_cnt_sent = self.fwd_cnt;
        Hdr {
            src_cid: HOST_CID,
            dst_cid: guest_cid,
            src_port: self.host_port,
            dst_port: self.guest_port,
            len,
            kind: TYPE_STREAM,
            op,
            flags,
            buf_alloc: BUF_ALLOC,
            fwd_cnt: self.fwd_cnt,
        }
    }

    /// Writes buffered guest data to the host socket until it would block.
    pub fn flush(&mut self) -> io::Result<()> {
        let mut done = 0;
        while done < self.to_host.len() {
            match self.stream.write(&self.to_host[done..]) {
                Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
                Ok(n) => done += n,
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
        self.to_host.drain(..done);
        self.fwd_cnt = self.fwd_cnt.wrapping_add(done as u32);
        Ok(())
    }

    /// Both directions are finished and everything is delivered: the
    /// connection can be reset and forgotten.
    pub fn finished(&self) -> bool {
        let guest_done = self.guest_send_done && self.to_host.is_empty();
        (guest_done && (self.guest_rcv_done || self.host_eof))
            || (self.guest_rcv_done && self.host_eof)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conn() -> Conn {
        let (a, _b) = std::os::unix::net::UnixStream::pair().unwrap();
        a.set_nonblocking(true).unwrap();
        Conn::new(UnixStream::from_std(a), 1024, 7700, Box::new(|_, _| {}))
    }

    #[test]
    fn credit_follows_the_peer_window() {
        let mut c = conn();
        assert_eq!(c.credit(), 0, "no window before the guest speaks");
        c.update_peer(&Hdr {
            buf_alloc: 1000,
            fwd_cnt: 0,
            ..Hdr::default()
        });
        c.tx_cnt = 600;
        assert_eq!(c.credit(), 400);
        c.update_peer(&Hdr {
            buf_alloc: 1000,
            fwd_cnt: 500,
            ..Hdr::default()
        });
        assert_eq!(c.credit(), 900);
        // Wrapping counters.
        c.tx_cnt = 5;
        c.peer_fwd_cnt = u32::MAX - 4;
        assert_eq!(c.credit(), 990);
    }

    #[test]
    fn headers_carry_our_credit_and_reset_the_debt() {
        let mut c = conn();
        c.fwd_cnt = BUF_ALLOC / 2;
        assert!(c.owes_credit());
        let h = c.header(3, super::super::packet::op::CREDIT_UPDATE, 0, 0);
        assert_eq!(
            (h.src_cid, h.dst_cid, h.src_port, h.dst_port),
            (2, 3, 1024, 7700)
        );
        assert_eq!((h.buf_alloc, h.fwd_cnt), (BUF_ALLOC, BUF_ALLOC / 2));
        assert!(!c.owes_credit());
    }

    #[test]
    fn finished_needs_both_directions_and_an_empty_buffer() {
        let mut c = conn();
        c.guest_send_done = true;
        assert!(!c.finished());
        c.host_eof = true;
        assert!(c.finished());
        c.to_host.push(1);
        assert!(!c.finished(), "undelivered guest data");
        c.to_host.clear();
        c.host_eof = false;
        c.guest_rcv_done = true;
        assert!(c.finished());
    }
}
