// SPDX-License-Identifier: Apache-2.0
use std::collections::{HashMap, VecDeque};
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use super::packet::{HDR_LEN, Hdr, SHUTDOWN_RCV, SHUTDOWN_SEND, TYPE_STREAM, op};
use super::*;
use crate::device::IrqStatus;
use crate::irq::TestIrq;
use crate::mem::Gpa;
use crate::queue::testing::Driver;

const RX_BUF: u32 = 4096;

/// A scripted guest driving the vsock queues.
struct Guest {
    rx: Driver,
    tx: Driver,
    ev: Driver,
    vsock: Vsock,
    bufs: HashMap<u32, Gpa>,
    seen: VecDeque<(u32, u32)>,
}

fn guest() -> Guest {
    let mut ds = Driver::many(3, 64, true);
    let ev = ds.pop().unwrap();
    let tx = ds.pop().unwrap();
    let rx = ds.pop().unwrap();
    let mut g = Guest {
        rx,
        tx,
        ev,
        vsock: Vsock::new(GUEST_CID).unwrap(),
        bufs: HashMap::new(),
        seen: VecDeque::new(),
    };
    for _ in 0..8 {
        g.post_rx();
    }
    g.vsock
        .activate(Activation {
            mem: g.rx.mem.clone(),
            queues: vec![g.rx.queue.clone(), g.tx.queue.clone(), g.ev.queue.clone()],
            irq: Arc::new(IrqStatus::new(Arc::new(TestIrq::default()))),
            features: features::RING_EVENT_IDX,
        })
        .unwrap();
    g
}

fn from_guest(op: u16, flags: u32, len: u32, buf_alloc: u32, fwd_cnt: u32) -> Hdr {
    Hdr {
        src_cid: GUEST_CID,
        dst_cid: 2,
        src_port: 7700,
        dst_port: 1024,
        len,
        kind: TYPE_STREAM,
        op,
        flags,
        buf_alloc,
        fwd_cnt,
    }
}

impl Guest {
    fn post_rx(&mut self) {
        let b = self.rx.buffer(&[], RX_BUF);
        let head = self.rx.post(&[(b, RX_BUF, true)]);
        self.bufs.insert(u32::from(head), b);
    }

    fn send(&mut self, h: Hdr, payload: &[u8]) {
        let hb = self.tx.buffer(&h.encode(), HDR_LEN as u32);
        let mut chain = vec![(hb, HDR_LEN as u32, false)];
        if !payload.is_empty() {
            let p = self.tx.buffer(payload, payload.len() as u32);
            chain.push((p, payload.len() as u32, false));
        }
        self.tx.post(&chain);
        self.vsock.doorbell().ring(1);
    }

    /// The next packet the device delivered, with its payload.
    fn recv(&mut self) -> (Hdr, Vec<u8>) {
        let t = Instant::now();
        while self.seen.is_empty() {
            self.seen.extend(self.rx.used());
            assert!(
                t.elapsed() < Duration::from_secs(5),
                "no packet from the device"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        let (head, len) = self.seen.pop_front().unwrap();
        let addr = self.bufs[&head];
        let mut hb = [0u8; HDR_LEN];
        self.rx.mem.read(addr, &mut hb).unwrap();
        let h = Hdr::parse(&hb);
        assert_eq!(len as usize, HDR_LEN + h.len as usize);
        let mut payload = vec![0u8; h.len as usize];
        self.rx
            .mem
            .read(addr + HDR_LEN as u64, &mut payload)
            .unwrap();
        (h, payload)
    }

    fn connect(&mut self) -> (UnixStream, mpsc::Receiver<Result<(), String>>) {
        let (host, theirs) = UnixStream::pair().unwrap();
        host.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let (tx, rx) = mpsc::channel();
        self.vsock
            .connector()
            .connect(
                theirs,
                7700,
                Box::new(move |_, r| {
                    tx.send(r).unwrap();
                }),
            )
            .unwrap();
        (host, rx)
    }
}

#[test]
fn connect_exchange_data_and_close() {
    let mut g = guest();
    let (mut host, result) = g.connect();
    let (req, _) = g.recv();
    assert_eq!(
        (req.op, req.src_cid, req.dst_cid),
        (op::REQUEST, 2, GUEST_CID)
    );
    assert_eq!((req.src_port, req.dst_port), (1024, 7700));
    g.send(from_guest(op::RESPONSE, 0, 0, 65536, 0), &[]);
    assert_eq!(result.recv_timeout(Duration::from_secs(5)).unwrap(), Ok(()));

    host.write_all(b"ping").unwrap();
    let (rw, data) = g.recv();
    assert_eq!((rw.op, data.as_slice()), (op::RW, &b"ping"[..]));

    g.send(from_guest(op::RW, 0, 4, 65536, 4), b"pong");
    let mut got = [0u8; 4];
    host.read_exact(&mut got).unwrap();
    assert_eq!(&got, b"pong");

    host.shutdown(std::net::Shutdown::Write).unwrap();
    let (sd, _) = g.recv();
    assert_eq!((sd.op, sd.flags), (op::SHUTDOWN, SHUTDOWN_SEND));
    assert_eq!(sd.fwd_cnt, 4, "our fwd_cnt counts the pong we delivered");

    g.send(
        from_guest(op::SHUTDOWN, SHUTDOWN_RCV | SHUTDOWN_SEND, 0, 65536, 4),
        &[],
    );
    let (rst, _) = g.recv();
    assert_eq!(rst.op, op::RST);
    assert_eq!(host.read(&mut got).unwrap(), 0, "host sees EOF");
}

#[test]
fn guest_refusal_reaches_the_host_client() {
    let mut g = guest();
    let (_host, result) = g.connect();
    let (req, _) = g.recv();
    assert_eq!(req.op, op::REQUEST);
    g.send(from_guest(op::RST, 0, 0, 0, 0), &[]);
    assert!(
        result
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .is_err()
    );
}

#[test]
fn stray_packets_get_a_reset() {
    let mut g = guest();
    let mut h = from_guest(op::RW, 0, 0, 0, 0);
    h.dst_port = 4242;
    g.send(h, &[]);
    let (rst, _) = g.recv();
    assert_eq!((rst.op, rst.dst_port, rst.src_port), (op::RST, 7700, 4242));
}

#[test]
fn sends_no_more_than_the_guest_window() {
    let mut g = guest();
    let (mut host, result) = g.connect();
    g.recv();
    g.send(from_guest(op::RESPONSE, 0, 0, 8, 0), &[]);
    result
        .recv_timeout(Duration::from_secs(5))
        .unwrap()
        .unwrap();
    host.write_all(b"0123456789abcdefghij").unwrap();
    let (rw, data) = g.recv();
    assert_eq!((rw.op, data.as_slice()), (op::RW, &b"01234567"[..]));
    std::thread::sleep(Duration::from_millis(50));
    assert!(g.rx.used().is_empty(), "window exhausted");
    g.send(from_guest(op::CREDIT_UPDATE, 0, 0, 8, 8), &[]);
    let (_, data) = g.recv();
    assert_eq!(data, b"89abcdef");
}

impl Guest {
    /// Activates the device again after a quiesce, its queues picking up
    /// at their used rings as a transport would.
    fn reactivate(&mut self) {
        let mut queues = vec![
            self.rx.queue.clone(),
            self.tx.queue.clone(),
            self.ev.queue.clone(),
        ];
        for q in &mut queues {
            q.resume(&self.rx.mem).unwrap();
        }
        self.vsock
            .activate(Activation {
                mem: self.rx.mem.clone(),
                queues,
                irq: Arc::new(IrqStatus::new(Arc::new(TestIrq::default()))),
                features: features::RING_EVENT_IDX,
            })
            .unwrap();
    }
}

#[test]
fn a_quiesce_keeps_the_hosts_connections() {
    let mut g = guest();
    let (mut host, result) = g.connect();
    g.recv();
    g.send(from_guest(op::RESPONSE, 0, 0, 65536, 0), &[]);
    assert_eq!(result.recv_timeout(Duration::from_secs(5)).unwrap(), Ok(()));
    g.vsock.quiesce(Duration::from_secs(1)).unwrap();
    host.write_all(b"held").unwrap();
    std::thread::sleep(Duration::from_millis(20));
    assert!(g.rx.used().is_empty(), "quiesced");
    g.reactivate();
    let (rw, data) = g.recv();
    assert_eq!((rw.op, data.as_slice()), (op::RW, &b"held"[..]));
}

#[test]
fn a_restored_device_tells_the_guest_its_transport_was_reset() {
    let mut g = guest();
    let mut fresh = Vsock::new(GUEST_CID).unwrap();
    fresh.restore_state(serde_json::Value::Null).unwrap();
    g.vsock = fresh;
    g.reactivate();
    std::thread::sleep(Duration::from_millis(20));
    assert!(g.ev.used().is_empty(), "no event buffer yet");
    let buf = g.ev.buffer(&[0xff; 4], 4);
    let head = g.ev.post(&[(buf, 4, true)]);
    g.vsock.doorbell().ring(2);
    let t = Instant::now();
    let used = loop {
        let used = g.ev.used();
        if !used.is_empty() {
            break used;
        }
        assert!(t.elapsed() < Duration::from_secs(5), "no event");
        std::thread::sleep(Duration::from_millis(1));
    };
    assert_eq!(used, vec![(u32::from(head), 4)]);
    let mut id = [0xffu8; 4];
    g.ev.mem.read(buf, &mut id).unwrap();
    assert_eq!(
        u32::from_le_bytes(id),
        0,
        "VIRTIO_VSOCK_EVENT_TRANSPORT_RESET"
    );

    // Activating again (a snapshot that did not go ahead) says nothing.
    g.vsock.quiesce(Duration::from_secs(1)).unwrap();
    g.ev.post(&[(buf, 4, true)]);
    g.reactivate();
    std::thread::sleep(Duration::from_millis(20));
    assert!(g.ev.used().is_empty(), "only once");
}
