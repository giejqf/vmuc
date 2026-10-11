// SPDX-License-Identifier: Apache-2.0
use std::time::{Duration, Instant};

use super::*;
use crate::device::IrqStatus;
use crate::irq::TestIrq;
use crate::mem::Gpa;
use crate::queue::testing::Driver;

const MAC: [u8; 6] = [0x0a, 0x56, 0x4d, 0x01, 0x02, 0x03];

struct Rig {
    rx: Driver,
    tx: Driver,
    net: Net,
    peer: UnixDatagram,
}

fn rig(mergeable: bool) -> Rig {
    let (ours, peer) = UnixDatagram::pair().unwrap();
    peer.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let mut ds = Driver::many(2, 16, true);
    let tx = ds.pop().unwrap();
    let rx = ds.pop().unwrap();
    let mut net = Net::new(MAC, ours).unwrap();
    let features = features::RING_EVENT_IDX | if mergeable { F_MRG_RXBUF } else { 0 };
    net.activate(Activation {
        mem: rx.mem.clone(),
        queues: vec![rx.queue.clone(), tx.queue.clone()],
        irq: Arc::new(IrqStatus::new(Arc::new(TestIrq::default()))),
        features,
    })
    .unwrap();
    Rig { rx, tx, net, peer }
}

fn wait_used(d: &Driver, n: u16) {
    let t = Instant::now();
    while d.mem.load_u16_acquire(d.queue.used + 2).unwrap() < n {
        assert!(t.elapsed() < Duration::from_secs(5), "no completion");
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn frame(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i % 251) as u8).collect()
}

#[test]
fn config_space_has_mac_link_and_mtu() {
    let r = rig(true);
    let mut b = [0u8; 12];
    r.net.read_config(0, &mut b);
    assert_eq!(&b[0..6], &MAC);
    assert_eq!(u16::from_le_bytes([b[6], b[7]]), 1);
    assert_eq!(u16::from_le_bytes([b[10], b[11]]), 1500);
    assert_eq!(parse_mac("0a:56:4d:01:02:03"), Some(MAC));
    assert_eq!(parse_mac("0a:56:4d:01:02"), None);
    assert_eq!(parse_mac("0a:56:4d:01:02:03:04"), None);
}

#[test]
fn transmit_strips_the_header_and_sends_one_datagram() {
    let mut r = rig(true);
    let sent = frame(1514);
    let hdr = r.tx.buffer(&[0u8; 12], 12);
    let head = r.tx.buffer(&sent[..600], 600);
    let tail = r.tx.buffer(&sent[600..], 914);
    r.tx.post(&[(hdr, 12, false), (head, 600, false), (tail, 914, false)]);
    r.net.doorbell().ring(1);
    let mut got = vec![0u8; 2048];
    let len = r.peer.recv(&mut got).unwrap();
    assert_eq!(&got[..len], sent.as_slice());
    wait_used(&r.tx, 1);
}

fn post_rx(d: &mut Driver, size: u32) -> Gpa {
    let b = d.buffer(&[], size);
    d.post(&[(b, size, true)]);
    b
}

#[test]
fn receive_waits_for_buffers_then_delivers_header_and_frame() {
    let mut r = rig(true);
    let f = frame(1000);
    r.peer.send(&f).unwrap();
    std::thread::sleep(Duration::from_millis(30));
    assert_eq!(
        r.rx.mem.load_u16_acquire(r.rx.queue.used + 2).unwrap(),
        0,
        "no buffers yet"
    );
    let buf = post_rx(&mut r.rx, 2048);
    r.net.doorbell().ring(0);
    wait_used(&r.rx, 1);
    let (_, len) = r.rx.used()[0];
    assert_eq!(len as usize, 12 + f.len());
    let mut got = vec![0u8; len as usize];
    r.rx.mem.read(buf, &mut got).unwrap();
    assert_eq!(u16::from_le_bytes([got[10], got[11]]), 1, "num_buffers");
    assert_eq!(&got[12..], f.as_slice());
}

#[test]
fn large_frames_merge_across_buffers() {
    let mut r = rig(true);
    let a = post_rx(&mut r.rx, 1024);
    let b = post_rx(&mut r.rx, 1024);
    let f = frame(1800);
    r.peer.send(&f).unwrap();
    r.net.doorbell().ring(0);
    wait_used(&r.rx, 2);
    let used = r.rx.used();
    assert_eq!(used.iter().map(|u| u.1).sum::<u32>() as usize, 12 + f.len());
    let mut got = vec![0u8; 2048];
    r.rx.mem.read(a, &mut got[..1024]).unwrap();
    r.rx.mem.read(b, &mut got[1024..]).unwrap();
    assert_eq!(u16::from_le_bytes([got[10], got[11]]), 2, "num_buffers");
    assert_eq!(&got[12..12 + f.len()], f.as_slice());
}
