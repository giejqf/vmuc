// SPDX-License-Identifier: Apache-2.0
//! The ring half of the helper's relay (ADR 0001), kept apart from vmnet
//! so it can be tested anywhere: draining the VMM's ring into a writer
//! and filling the VMM's ring from a reader, a batch at a time.

use crate::ring::{Consumer, Next, Producer, Slot};

/// A frame handed to a writer: `len` bytes at `data`, inside the ring
/// and valid until the call returns.
pub type Out = (*const u8, usize);
/// Room handed to a reader: it writes a frame at `data` and sets `len`.
pub type In = (*mut u8, usize);

/// Takes up to `batch` frames off `consumer`, hands them to `write` in one
/// call, then frees their space. `Ok((frames, kick))`, `kick` when the
/// producer asked to be woken; `Err` when the ring is corrupt.
pub fn drain(
    consumer: &mut Consumer,
    batch: usize,
    frames: &mut Vec<Out>,
    write: impl FnOnce(&[Out]),
) -> Result<(usize, bool), &'static str> {
    frames.clear();
    while frames.len() < batch {
        match consumer.peek() {
            Next::Frame { data, len } => {
                frames.push((data, len));
                consumer.advance();
            }
            Next::Empty => break,
            Next::Corrupt(why) => return Err(why),
        }
    }
    if frames.is_empty() {
        return Ok((0, false));
    }
    write(frames);
    Ok((frames.len(), consumer.release()))
}

/// What one fill brought.
#[derive(Debug, PartialEq, Eq)]
pub enum Filled {
    /// `n` frames, published; `kick` when the consumer asked to be woken.
    Frames { n: usize, kick: bool },
    /// The reader had nothing.
    Empty,
    /// No room for even one frame of `capacity`.
    Full,
}

/// Reserves up to `batch` slots of `capacity` bytes, lets `read` fill the
/// first `n` of them (setting each length, as `vmnet_read` does) and
/// return `n`, then commits and publishes those.
pub fn fill(
    producer: &mut Producer,
    batch: usize,
    capacity: usize,
    slots: &mut Vec<Slot>,
    room: &mut Vec<In>,
    read: impl FnOnce(&mut [In]) -> usize,
) -> Filled {
    slots.clear();
    room.clear();
    while slots.len() < batch {
        let Some(slot) = producer.reserve(capacity) else {
            break;
        };
        room.push((slot.data(), slot.capacity()));
        slots.push(slot);
    }
    if slots.is_empty() {
        return Filled::Full;
    }
    let n = read(room).min(slots.len());
    if n == 0 {
        producer.abandon();
        return Filled::Empty;
    }
    for (slot, &(_, len)) in slots.iter().zip(room.iter()).take(n) {
        producer.commit(slot, len.min(slot.capacity()));
    }
    Filled::Frames {
        n,
        kick: producer.publish(),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::region::Region;
    use crate::ring::{Direction, REGION_BYTES};

    #[test]
    fn frames_go_round_through_fill_and_drain() {
        let region = Arc::new(Region::anonymous(REGION_BYTES).unwrap());
        let mut p = Producer::new(Arc::clone(&region), Direction::ToVmm);
        let mut c = Consumer::new(Arc::clone(&region), Direction::ToVmm);
        let (mut slots, mut room, mut out) = (Vec::new(), Vec::new(), Vec::new());

        // A reader with three frames for eight slots.
        let filled = fill(&mut p, 8, 2048, &mut slots, &mut room, |room| {
            for (i, r) in room.iter_mut().take(3).enumerate() {
                let f = vec![i as u8 + 1; 100 * (i + 1)];
                // SAFETY: each slot has 2048 writable bytes.
                unsafe { std::ptr::copy_nonoverlapping(f.as_ptr(), r.0, f.len()) };
                r.1 = f.len();
            }
            3
        });
        assert_eq!(filled, Filled::Frames { n: 3, kick: false });
        assert_eq!(
            fill(&mut p, 8, 2048, &mut slots, &mut room, |_| 0),
            Filled::Empty
        );

        let mut seen = Vec::new();
        let drained = drain(&mut c, 2, &mut out, |frames| {
            for &(data, len) in frames {
                // SAFETY: the ring holds `len` bytes at `data` during the call.
                seen.push(unsafe { std::slice::from_raw_parts(data, len) }.to_vec());
            }
        });
        assert_eq!(drained, Ok((2, false)));
        assert_eq!(seen, vec![vec![1; 100], vec![2; 200]]);
        assert_eq!(
            drain(&mut c, 8, &mut out, |f| assert_eq!(f.len(), 1)),
            Ok((1, false))
        );
        assert_eq!(
            drain(&mut c, 8, &mut out, |_| panic!("nothing left")),
            Ok((0, false))
        );
    }

    #[test]
    fn a_full_ring_reports_full_without_reading() {
        let region = Arc::new(Region::anonymous(REGION_BYTES).unwrap());
        let mut p = Producer::new(Arc::clone(&region), Direction::ToVmm);
        let (mut slots, mut room) = (Vec::new(), Vec::new());
        let big = crate::ring::MAX_FRAME;
        while let Filled::Frames { .. } = fill(&mut p, 64, big, &mut slots, &mut room, |r| {
            for x in r.iter_mut() {
                x.1 = big;
            }
            r.len()
        }) {}
        assert_eq!(
            fill(&mut p, 64, big, &mut slots, &mut room, |_| panic!(
                "no room to read into"
            )),
            Filled::Full
        );
    }
}
