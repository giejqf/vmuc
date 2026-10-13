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
    rig_with(false, if mergeable { F_MRG_RXBUF } else { 0 })
}

/// `offload`: the peer speaks virtio-net headers; `negotiated`: the
/// driver's feature bits besides EVENT_IDX.
fn rig_with(offload: bool, negotiated: u64) -> Rig {
    let (ours, peer) = UnixDatagram::pair().unwrap();
    peer.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    size_buffers(&peer).unwrap();
    let mut ds = Driver::many(2, 16, true);
    let tx = ds.pop().unwrap();
    let rx = ds.pop().unwrap();
    let mut net = Net::new(MAC, ours, offload, None).unwrap();
    net.activate(Activation {
        mem: rx.mem.clone(),
        queues: vec![rx.queue.clone(), tx.queue.clone()],
        irq: Arc::new(IrqStatus::new(Arc::new(TestIrq::default()))),
        features: features::RING_EVENT_IDX | negotiated,
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
    let c = r.net.counters();
    assert_eq!(c.tx_frames.load(Ordering::Relaxed), 1);
    assert_eq!(c.tx_bytes.load(Ordering::Relaxed), sent.len() as u64);
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
    let c = r.net.counters();
    assert_eq!(c.rx_frames.load(Ordering::Relaxed), 1);
    assert_eq!(c.rx_bytes.load(Ordering::Relaxed), f.len() as u64);
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

/// A virtio-net header: flags, gso_type, then gso_size 1448.
fn hdr(flags: u8, gso: u8) -> [u8; 12] {
    let mut h = [0u8; 12];
    h[0] = flags;
    h[1] = gso;
    h[4..6].copy_from_slice(&1448u16.to_le_bytes());
    h
}

#[test]
fn offloads_are_offered_only_when_the_peer_speaks_headers() {
    assert_eq!(rig(true).net.features() & offload::OFFERED, 0);
    let r = rig_with(true, F_MRG_RXBUF);
    assert_eq!(r.net.features() & offload::OFFERED, offload::OFFERED);
}

#[test]
fn with_offloads_transmit_keeps_the_header() {
    let mut r = rig_with(true, F_MRG_RXBUF | offload::OFFERED);
    let h = hdr(1, 1); // NEEDS_CSUM, TCPv4 GSO
    let sent = frame(9000);
    let hb = r.tx.buffer(&h, 12);
    let fb = r.tx.buffer(&sent, 9000);
    r.tx.post(&[(hb, 12, false), (fb, 9000, false)]);
    r.net.doorbell().ring(1);
    let mut got = vec![0u8; 16384];
    let len = r.peer.recv(&mut got).unwrap();
    assert_eq!(len, 12 + sent.len());
    assert_eq!(&got[..12], &h);
    assert_eq!(&got[12..len], sent.as_slice());
}

#[test]
fn with_offloads_receive_fits_frames_to_the_driver() {
    // A driver without TSO: the GSO frame is dropped, the plain one kept.
    let mut r = rig_with(true, F_MRG_RXBUF | offload::F_GUEST_CSUM);
    let buf = post_rx(&mut r.rx, 4096);
    let mut gso = hdr(0, 1).to_vec();
    gso.extend(frame(3000));
    r.peer.send(&gso).unwrap();
    let mut plain = hdr(0, 0).to_vec();
    plain.extend(frame(100));
    r.peer.send(&plain).unwrap();
    r.net.doorbell().ring(0);
    wait_used(&r.rx, 1);
    let (_, len) = r.rx.used()[0];
    assert_eq!(len as usize, plain.len());
    let mut got = vec![0u8; len as usize];
    r.rx.mem.read(buf, &mut got).unwrap();
    assert_eq!(u16::from_le_bytes([got[10], got[11]]), 1, "num_buffers set");
    assert_eq!(&got[12..], &plain[12..]);

    // With TSO the 3000-byte segment arrives whole, header intact.
    let mut r = rig_with(true, F_MRG_RXBUF | offload::OFFERED);
    let buf = post_rx(&mut r.rx, 4096);
    r.peer.send(&gso).unwrap();
    r.net.doorbell().ring(0);
    wait_used(&r.rx, 1);
    let mut got = vec![0u8; gso.len()];
    r.rx.mem.read(buf, &mut got).unwrap();
    assert_eq!(&got[..10], &gso[..10]);
    assert_eq!(&got[12..], &gso[12..]);
}

#[test]
fn a_frame_waits_for_enough_buffers_instead_of_dropping() {
    let mut r = rig(true);
    let a = post_rx(&mut r.rx, 1024);
    let f = frame(1800);
    r.peer.send(&f).unwrap();
    r.net.doorbell().ring(0);
    std::thread::sleep(Duration::from_millis(30));
    assert_eq!(
        r.rx.mem.load_u16_acquire(r.rx.queue.used + 2).unwrap(),
        0,
        "one buffer is not enough: held, not delivered or dropped"
    );
    let b = post_rx(&mut r.rx, 1024);
    r.net.doorbell().ring(0);
    wait_used(&r.rx, 2);
    let mut got = vec![0u8; 2048];
    r.rx.mem.read(a, &mut got[..1024]).unwrap();
    r.rx.mem.read(b, &mut got[1024..]).unwrap();
    assert_eq!(u16::from_le_bytes([got[10], got[11]]), 2, "num_buffers");
    assert_eq!(&got[12..12 + f.len()], f.as_slice());
}

#[test]
fn without_mergeable_buffers_an_oversized_frame_is_dropped() {
    let mut r = rig(false);
    let buf = post_rx(&mut r.rx, 1024);
    r.peer.send(&frame(1800)).unwrap();
    let small = frame(500);
    r.peer.send(&small).unwrap();
    r.net.doorbell().ring(0);
    wait_used(&r.rx, 1);
    let (_, len) = r.rx.used()[0];
    assert_eq!(len as usize, 12 + small.len());
    let mut got = vec![0u8; len as usize];
    r.rx.mem.read(buf, &mut got).unwrap();
    assert_eq!(&got[12..], small.as_slice());
}

mod with_rings {
    use std::sync::Arc;

    use vmuc_net::region::Region;
    use vmuc_net::ring::{Consumer, Direction, Next, Producer, REGION_BYTES};

    use super::*;

    /// The device in ring mode, and the test as the helper.
    struct RingRig {
        rx: Driver,
        tx: Driver,
        net: Net,
        peer: UnixDatagram,
        to_vmm: Producer,
        from_vmm: Consumer,
        region: Arc<Region>,
    }

    fn ring_rig() -> RingRig {
        let region = Arc::new(Region::anonymous(REGION_BYTES).unwrap());
        let (ours, peer) = UnixDatagram::pair().unwrap();
        peer.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let mut ds = Driver::many(2, 16, true);
        let tx = ds.pop().unwrap();
        let rx = ds.pop().unwrap();
        let mut net = Net::new(MAC, ours, true, Some(&region)).unwrap();
        net.activate(Activation {
            mem: rx.mem.clone(),
            queues: vec![rx.queue.clone(), tx.queue.clone()],
            irq: Arc::new(IrqStatus::new(Arc::new(TestIrq::default()))),
            features: features::RING_EVENT_IDX | F_MRG_RXBUF | offload::OFFERED,
        })
        .unwrap();
        RingRig {
            rx,
            tx,
            net,
            peer,
            to_vmm: Producer::new(Arc::clone(&region), Direction::ToVmm),
            from_vmm: Consumer::new(Arc::clone(&region), Direction::ToHelper),
            region,
        }
    }

    fn take(c: &mut Consumer) -> Vec<u8> {
        let t = Instant::now();
        loop {
            match c.peek() {
                Next::Frame { data, len } => {
                    let mut v = vec![0u8; len];
                    // SAFETY: `peek` validated `len` bytes at `data`.
                    unsafe { std::ptr::copy_nonoverlapping(data, v.as_mut_ptr(), len) };
                    c.advance();
                    c.release();
                    return v;
                }
                Next::Empty => {
                    assert!(t.elapsed() < Duration::from_secs(5), "no frame");
                    std::thread::sleep(Duration::from_millis(1));
                }
                Next::Corrupt(why) => panic!("{why}"),
            }
        }
    }

    #[test]
    fn transmit_lands_in_the_helper_ring_and_kicks_a_sleeping_helper() {
        let mut r = ring_rig();
        assert!(r.from_vmm.prepare_sleep());
        let mut sent = hdr(1, 1).to_vec();
        sent.extend(frame(5000));
        let b = r.tx.buffer(&sent, sent.len() as u32);
        r.tx.post(&[(b, sent.len() as u32, false)]);
        r.net.doorbell().ring(1);
        let mut kick = [0u8; 8];
        assert_eq!(r.peer.recv(&mut kick).unwrap(), 1, "kicked");
        assert_eq!(take(&mut r.from_vmm), sent, "header and frame, untouched");
        wait_used(&r.tx, 1);
    }

    #[test]
    fn a_stream_is_taken_without_kicks_until_it_pauses() {
        let mut r = ring_rig();
        let post = |r: &mut RingRig| {
            let mut f = hdr(0, 0).to_vec();
            f.extend(frame(1000));
            let b = r.tx.buffer(&f, f.len() as u32);
            r.tx.post(&[(b, f.len() as u32, false)]);
        };
        // A stray frame: kicks are asked for again at once.
        post(&mut r);
        r.net.doorbell().ring(1);
        take(&mut r.from_vmm);
        wait_used(&r.tx, 1);
        let t = Instant::now();
        while r.tx.avail_event() != 1 {
            assert!(
                t.elapsed() < Duration::from_millis(20),
                "kicks not re-armed"
            );
            std::thread::yield_now();
        }
        // Another right after: a stream, and the device watches the queue.
        post(&mut r);
        r.net.doorbell().ring(1);
        take(&mut r.from_vmm);
        for _ in 0..8 {
            post(&mut r);
        }
        for _ in 0..8 {
            take(&mut r.from_vmm);
        }
        wait_used(&r.tx, 10);
        assert_eq!(r.tx.avail_event(), 1, "no kick asked for meanwhile");
        // Once the stream pauses, kicks are asked for again.
        let t = Instant::now();
        while r.tx.avail_event() != 10 {
            assert!(t.elapsed() < Duration::from_secs(5), "kicks not re-armed");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn receive_waits_in_the_ring_for_buffers_then_delivers() {
        let mut r = ring_rig();
        let mut f = hdr(0, 0).to_vec();
        f.extend(frame(1800));
        let slot = r.to_vmm.reserve(f.len()).unwrap();
        // SAFETY: the slot has room for the frame.
        unsafe { std::ptr::copy_nonoverlapping(f.as_ptr(), slot.data(), f.len()) };
        r.to_vmm.commit(&slot, f.len());
        r.to_vmm.publish();
        let a = post_rx(&mut r.rx, 1024);
        r.peer.send(b"k").unwrap();
        std::thread::sleep(Duration::from_millis(30));
        assert_eq!(
            r.rx.mem.load_u16_acquire(r.rx.queue.used + 2).unwrap(),
            0,
            "one buffer is not enough: the frame stays in the ring"
        );
        let b = post_rx(&mut r.rx, 1024);
        r.net.doorbell().ring(0);
        wait_used(&r.rx, 2);
        let mut got = vec![0u8; 2048];
        r.rx.mem.read(a, &mut got[..1024]).unwrap();
        r.rx.mem.read(b, &mut got[1024..]).unwrap();
        assert_eq!(u16::from_le_bytes([got[10], got[11]]), 2, "num_buffers");
        assert_eq!(&got[12..f.len()], &f[12..]);
    }

    #[allow(clippy::cast_ptr_alignment)] // the region is page-aligned
    #[test]
    fn a_corrupt_ring_from_the_helper_stops_receive_not_the_device() {
        let mut r = ring_rig();
        // Head far beyond anything published.
        // SAFETY: the VMM-bound ring's head is at its start, 8-aligned.
        unsafe {
            r.region
                .base()
                .add(vmuc_net::ring::RING_BYTES)
                .cast::<u64>()
                .write_volatile(u64::MAX / 2);
        }
        post_rx(&mut r.rx, 2048);
        r.peer.send(b"k").unwrap();
        std::thread::sleep(Duration::from_millis(30));
        assert_eq!(r.rx.mem.load_u16_acquire(r.rx.queue.used + 2).unwrap(), 0);
        // Transmit still works.
        let mut sent = hdr(0, 0).to_vec();
        sent.extend(frame(100));
        let b = r.tx.buffer(&sent, sent.len() as u32);
        r.tx.post(&[(b, sent.len() as u32, false)]);
        r.net.doorbell().ring(1);
        assert_eq!(take(&mut r.from_vmm), sent);
    }
}
