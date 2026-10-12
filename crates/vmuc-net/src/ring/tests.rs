// SPDX-License-Identifier: Apache-2.0
use std::sync::Arc;

use super::*;

fn pair() -> (Producer, Consumer, Arc<Region>) {
    let region = Arc::new(Region::anonymous(REGION_BYTES).unwrap());
    let p = Producer::new(Arc::clone(&region), Direction::ToHelper);
    let c = Consumer::new(Arc::clone(&region), Direction::ToHelper);
    (p, c, region)
}

/// A frame whose bytes encode `seq` and `len`.
fn frame(seq: u32, len: usize) -> Vec<u8> {
    (0..len).map(|i| (seq as usize * 31 + i) as u8).collect()
}

fn push(p: &mut Producer, f: &[u8]) -> bool {
    let Some(slot) = p.reserve(f.len()) else {
        return false;
    };
    // SAFETY: the slot has `f.len()` writable bytes.
    unsafe { std::ptr::copy_nonoverlapping(f.as_ptr(), slot.data(), f.len()) };
    p.commit(&slot, f.len());
    true
}

fn pop(c: &mut Consumer) -> Option<Vec<u8>> {
    match c.peek() {
        Next::Frame { data, len } => {
            let mut v = vec![0u8; len];
            // SAFETY: `peek` validated `len` readable bytes at `data`.
            unsafe { std::ptr::copy_nonoverlapping(data, v.as_mut_ptr(), len) };
            c.advance();
            Some(v)
        }
        Next::Empty => None,
        Next::Corrupt(why) => panic!("corrupt: {why}"),
    }
}

#[test]
fn frames_round_trip_across_many_wraps() {
    let (mut p, mut c, _r) = pair();
    let sizes = [60, 1514, 65_536 + 12, 1, 9000, 4096 - 8];
    let mut seq = 0u32;
    // ~60 MiB through a 4 MiB ring: it wraps over a dozen times.
    for round in 0..200 {
        let mut sent = Vec::new();
        for &len in &sizes {
            let f = frame(seq, len + round % 7);
            assert!(push(&mut p, &f), "ring full at round {round}");
            sent.push(f);
            seq += 1;
        }
        assert!(!p.publish(), "nobody asleep");
        for f in sent {
            assert_eq!(pop(&mut c).as_deref(), Some(f.as_slice()));
        }
        assert_eq!(pop(&mut c), None);
        assert!(!c.release(), "nobody waiting");
    }
}

#[test]
fn reservations_are_invisible_until_committed_and_published() {
    let (mut p, mut c, _r) = pair();
    let a = p.reserve(MAX_FRAME).unwrap();
    let _b = p.reserve(MAX_FRAME).unwrap();
    // SAFETY: the slot has room for 3 bytes.
    unsafe { std::ptr::copy_nonoverlapping(b"abc".as_ptr(), a.data(), 3) };
    assert_eq!(c.peek(), Next::Empty, "nothing published");
    p.commit(&a, 3);
    p.publish(); // `_b` is abandoned
    assert_eq!(pop(&mut c).as_deref(), Some(&b"abc"[..]));
    assert_eq!(c.peek(), Next::Empty);
    // The abandoned room is reused.
    assert!(push(&mut p, b"next"));
    p.publish();
    assert_eq!(pop(&mut c).as_deref(), Some(&b"next"[..]));
}

#[test]
fn a_full_ring_waits_and_both_sides_ask_for_kicks() {
    let (mut p, mut c, _r) = pair();
    let big = frame(0, MAX_FRAME);
    let mut n = 0;
    while push(&mut p, &big) {
        n += 1;
    }
    assert!(n >= DATA / (MAX_FRAME + 64) - 1, "only {n} fit");
    assert!(p.prepare_wait(MAX_FRAME), "still full: wait");
    p.publish();
    assert!(pop(&mut c).is_some());
    assert!(c.release(), "the producer asked to be kicked");
    assert!(!c.release(), "and only once");
    assert!(!p.prepare_wait(MAX_FRAME), "room now: no wait");

    while pop(&mut c).is_some() {}
    c.release();
    assert!(c.prepare_sleep(), "empty: sleep");
    assert!(push(&mut p, b"x"));
    assert!(p.publish(), "the consumer asked to be kicked");
    assert!(!c.prepare_sleep(), "a frame arrived: do not sleep");
}

#[test]
fn two_threads_move_forty_thousand_frames_in_order() {
    // ~460 MiB: the 4 MiB ring wraps over a hundred times.
    const N: u32 = 40_000;
    let (mut p, mut c, _r) = pair();
    let len = |seq: u32| [64, 1514, 9000, 35_000][seq as usize % 4] + seq as usize % 13;
    let producer = std::thread::spawn(move || {
        for seq in 0..N {
            let f = frame(seq, len(seq));
            while !push(&mut p, &f) {
                p.publish();
                std::hint::spin_loop();
            }
            if seq % 8 == 0 {
                p.publish();
            }
        }
        p.publish();
    });
    let mut seq = 0;
    while seq < N {
        if let Some(f) = pop(&mut c) {
            assert_eq!(f, frame(seq, len(seq)), "frame {seq}");
            seq += 1;
            if seq % 16 == 0 {
                c.release();
            }
        } else {
            c.release();
            std::hint::spin_loop();
        }
    }
    producer.join().unwrap();
}

/// Writes a u32 or u64 into the ring's shared memory, as a hostile
/// producer could.
#[allow(clippy::cast_ptr_alignment)] // offsets are aligned in these tests
fn poke32(r: &Region, off: usize, v: u32) {
    // SAFETY: `off` is within the region and 4-aligned in these tests.
    unsafe { r.base().add(off).cast::<u32>().write_volatile(v) };
}

#[allow(clippy::cast_ptr_alignment)]
fn poke64(r: &Region, off: usize, v: u64) {
    // SAFETY: as above, 8-aligned.
    unsafe { r.base().add(off).cast::<u64>().write_volatile(v) };
}

#[test]
fn a_hostile_producer_cannot_make_the_consumer_read_out_of_bounds() {
    type Case = (&'static str, fn(&Region));
    let cases: [Case; 6] = [
        ("head far ahead", |r| poke64(r, HEAD, DATA as u64 + 64)),
        ("head behind", |r| poke64(r, HEAD, u64::MAX)),
        ("huge length", |r| {
            poke64(r, HEAD, 128);
            poke32(r, CONTROL, u32::MAX - 1);
            poke32(r, CONTROL + 4, 128);
        }),
        ("misaligned next", |r| {
            poke64(r, HEAD, 128);
            poke32(r, CONTROL, 10);
            poke32(r, CONTROL + 4, 72);
        }),
        ("next past head", |r| {
            poke64(r, HEAD, 64);
            poke32(r, CONTROL, 10);
            poke32(r, CONTROL + 4, 128);
        }),
        ("wrap marker mid-ring", |r| {
            poke64(r, HEAD, 128);
            poke32(r, CONTROL, WRAP);
            poke32(r, CONTROL + 4, 64);
        }),
    ];
    for (what, corrupt) in cases {
        let region = Arc::new(Region::anonymous(REGION_BYTES).unwrap());
        let mut c = Consumer::new(Arc::clone(&region), Direction::ToHelper);
        corrupt(&region);
        assert!(matches!(c.peek(), Next::Corrupt(_)), "{what}");
        c.advance(); // without a valid peek: no movement
        assert!(matches!(c.peek(), Next::Corrupt(_)), "{what}, again");
    }
}

#[test]
fn a_hostile_consumer_only_makes_the_producer_wait() {
    let region = Arc::new(Region::anonymous(REGION_BYTES).unwrap());
    let mut p = Producer::new(Arc::clone(&region), Direction::ToVmm);
    poke64(&region, RING_BYTES + TAIL, 1 << 40);
    assert!(p.reserve(100).is_none(), "tail ahead of the producer");
}
