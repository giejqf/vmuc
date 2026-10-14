// SPDX-License-Identifier: Apache-2.0
//! Single-producer, single-consumer byte rings for frames between the VMM
//! and the vmnet helper, in a shared [`Region`] (ADR 0001).
//!
//! Each ring is a control page and a power-of-two data area. The control
//! page holds the producer's `head` and the consumer's `tail` (byte
//! positions that only grow) and two wake-up flags, each on its own cache
//! line. A record is a 64-byte-aligned 8-byte header, `len` (frame bytes)
//! and `next` (bytes to the following record), then the frame; records
//! never wrap, a `len == WRAP` marker sends the consumer to the start.
//!
//! The other side of the region may be another process with different
//! privileges, so its contents are untrusted: the region is touched only
//! through atomics and raw-pointer copies, never references, and every
//! index and header is read once and checked before use. A consumer that
//! finds something malformed reports [`Next::Corrupt`] and never reads
//! outside its ring.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use crate::region::Region;

/// Version of this layout. The helper's hello names the one it speaks and
/// the VMM uses rings only when they match: a helper is installed
/// separately and can lag behind. Bump it with any change below.
pub const LAYOUT: u32 = 2;
/// Control page bytes; data starts after it. A 16 KiB page, so the region
/// is whole pages on Apple silicon.
const CONTROL: usize = 16 << 10;
/// Data bytes per ring.
pub const DATA: usize = 4 << 20;
/// One ring in the region.
pub const RING_BYTES: usize = CONTROL + DATA;
/// The region: the VMM-to-helper ring, then the helper-to-VMM ring.
pub const REGION_BYTES: usize = 2 * RING_BYTES;
/// Largest frame a record may carry (a 64 KiB TSO segment, its Ethernet
/// and virtio-net headers, with room to spare).
pub const MAX_FRAME: usize = 65_536 + 128;

const HEAD: usize = 0;
const TAIL: usize = 64;
const CONSUMER_SLEEPING: usize = 128;
const PRODUCER_WAITING: usize = 192;
const HDR: usize = 8;
const ALIGN: usize = 64;
const WRAP: u32 = u32::MAX;

/// Which ring of the region.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    ToHelper,
    ToVmm,
}

fn ring_base(region: &Region, dir: Direction) -> *mut u8 {
    assert!(
        region.len() >= REGION_BYTES,
        "region too small for two rings"
    );
    let off = match dir {
        Direction::ToHelper => 0,
        Direction::ToVmm => RING_BYTES,
    };
    // SAFETY: `off + RING_BYTES <= region.len()`, checked above.
    unsafe { region.base().add(off) }
}

/// An atomic in the control page.
///
/// # Safety
/// `base` is a ring base from `ring_base` and `off` one of the control
/// offsets: in bounds and suitably aligned (the region is page-aligned).
#[allow(clippy::cast_ptr_alignment)] // aligned by the layout, see above
unsafe fn word64<'a>(base: *mut u8, off: usize) -> &'a AtomicU64 {
    // SAFETY: per the contract; atomics may be shared across processes.
    unsafe { &*base.add(off).cast::<AtomicU64>() }
}

/// # Safety
/// As for [`word64`].
#[allow(clippy::cast_ptr_alignment)] // aligned by the layout
unsafe fn word32<'a>(base: *mut u8, off: usize) -> &'a AtomicU32 {
    // SAFETY: per the contract.
    unsafe { &*base.add(off).cast::<AtomicU32>() }
}

/// A record header field.
///
/// # Safety
/// `p` is a header field inside a ring's data area: records start 64-byte
/// aligned in a page-aligned region, so fields are 4-byte aligned.
#[allow(clippy::cast_ptr_alignment)] // aligned by the layout, see above
unsafe fn load_field(p: *const u8) -> u32 {
    // SAFETY: per the contract.
    unsafe { p.cast::<u32>().read_volatile() }
}

/// # Safety
/// As for [`load_field`].
#[allow(clippy::cast_ptr_alignment)] // aligned by the layout
unsafe fn store_field(p: *mut u8, v: u32) {
    // SAFETY: per the contract.
    unsafe { p.cast::<u32>().write_volatile(v) }
}

fn record_len(frame: usize) -> usize {
    (HDR + frame).next_multiple_of(ALIGN)
}

/// Room reserved for one frame.
#[derive(Debug, Clone, Copy)]
pub struct Slot {
    at: u64,
    stride: usize,
    data: *mut u8,
    capacity: usize,
}

impl Slot {
    /// Where the frame goes: `capacity()` writable bytes.
    pub fn data(&self) -> *mut u8 {
        self.data
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }
}

/// The writing end of one ring.
#[derive(Debug)]
pub struct Producer {
    _region: Arc<Region>,
    base: *mut u8,
    /// Next free byte (reservations move it).
    cursor: u64,
    /// End of the last committed record (publish moves head to it).
    committed: u64,
    /// What `head` holds.
    published: u64,
}

// SAFETY: the producer is used by one thread at a time; the region stays
// mapped while it holds the Arc.
unsafe impl Send for Producer {}

impl Producer {
    pub fn new(region: Arc<Region>, dir: Direction) -> Self {
        let base = ring_base(&region, dir);
        // SAFETY: control offsets of a ring base.
        let head = unsafe { word64(base, HEAD) }.load(Ordering::Acquire);
        Self {
            _region: region,
            base,
            cursor: head,
            committed: head,
            published: head,
        }
    }

    fn data(&self, pos: usize) -> *mut u8 {
        // SAFETY: callers keep `pos < DATA`.
        unsafe { self.base.add(CONTROL + pos) }
    }

    /// Room for a frame of up to `capacity` bytes after any already
    /// reserved, or `None` while the ring is too full.
    /// `capacity` must be at most [`MAX_FRAME`]; callers drop larger frames.
    pub fn reserve(&mut self, capacity: usize) -> Option<Slot> {
        assert!(capacity <= MAX_FRAME, "frame larger than a record may be");
        let need = record_len(capacity);
        // SAFETY: control offsets of a ring base.
        let tail = unsafe { word64(self.base, TAIL) }.load(Ordering::Acquire);
        // The consumer owns `tail`; one out of range just means no room.
        let used = self
            .cursor
            .checked_sub(tail)
            .filter(|&u| u <= DATA as u64)?;
        let free = DATA - used as usize;
        let pos = (self.cursor % DATA as u64) as usize;
        let till_end = DATA - pos;
        let (skip, at) = if need > till_end {
            (till_end, 0)
        } else {
            (0, pos)
        };
        if skip + need > free {
            return None;
        }
        if skip > 0 {
            // Not visible until published; harmless if abandoned.
            self.write_header(pos, WRAP, till_end);
            self.cursor += skip as u64;
        }
        let slot = Slot {
            at: self.cursor,
            stride: need,
            data: self.data(at + HDR),
            capacity,
        };
        self.cursor += need as u64;
        Some(slot)
    }

    fn write_header(&self, pos: usize, len: u32, next: usize) {
        let p = self.data(pos);
        // SAFETY: a header fits: `pos` is aligned and `pos + HDR <= DATA`.
        unsafe {
            store_field(p, len);
            store_field(p.add(4), next as u32);
        }
    }

    /// Records `len` bytes (at most the slot's capacity) in `slot`. Slots
    /// are committed in the order they were reserved.
    pub fn commit(&mut self, slot: &Slot, len: usize) {
        assert!(len <= slot.capacity);
        let pos = (slot.at % DATA as u64) as usize;
        self.write_header(pos, len as u32, slot.stride);
        self.committed = slot.at + slot.stride as u64;
    }

    /// Makes committed records visible and forgets uncommitted
    /// reservations. `true` when the consumer is asleep and must be kicked.
    /// With nothing new committed the consumer's request to be kicked is
    /// left for the publish that has something.
    pub fn publish(&mut self) -> bool {
        self.cursor = self.committed;
        if self.committed == self.published {
            return false;
        }
        self.published = self.committed;
        // SAFETY: control offsets of a ring base.
        unsafe { word64(self.base, HEAD) }.store(self.committed, Ordering::SeqCst);
        // SAFETY: as above.
        unsafe { word32(self.base, CONSUMER_SLEEPING) }.swap(0, Ordering::SeqCst) != 0
    }

    /// Published bytes the consumer has not released yet.
    pub fn unreleased(&self) -> u64 {
        // SAFETY: control offsets of a ring base.
        let tail = unsafe { word64(self.base, TAIL) }.load(Ordering::Acquire);
        // The consumer owns `tail`; one out of range counts as nothing.
        self.published.saturating_sub(tail)
    }

    /// Whether records are committed but not yet published.
    pub fn holding(&self) -> bool {
        self.committed != self.published
    }

    /// Forgets uncommitted reservations.
    pub fn abandon(&mut self) {
        self.cursor = self.committed;
    }

    /// Before waiting for space: asks to be kicked when the consumer frees
    /// some. `false` when space for `capacity` appeared meanwhile.
    pub fn prepare_wait(&mut self, capacity: usize) -> bool {
        // SAFETY: control offsets of a ring base.
        unsafe { word32(self.base, PRODUCER_WAITING) }.store(1, Ordering::SeqCst);
        let cursor = self.cursor;
        let fits = self.reserve(capacity).is_some();
        self.cursor = cursor;
        if fits {
            // SAFETY: as above.
            unsafe { word32(self.base, PRODUCER_WAITING) }.store(0, Ordering::SeqCst);
        }
        !fits
    }
}

/// What the consumer finds next.
#[derive(Debug, PartialEq, Eq)]
pub enum Next {
    /// A frame: `len` bytes at `data`, valid until released.
    Frame {
        data: *const u8,
        len: usize,
    },
    Empty,
    /// The producer wrote something impossible.
    Corrupt(&'static str),
}

/// The reading end of one ring.
#[derive(Debug)]
pub struct Consumer {
    _region: Arc<Region>,
    base: *mut u8,
    /// Start of the next record to read (ahead of `tail` until released).
    read: u64,
    /// The validated `next` of the record `peek` returned: re-reading it
    /// from shared memory would let the producer move us unchecked.
    peeked: Option<u64>,
}

// SAFETY: as for `Producer`.
unsafe impl Send for Consumer {}

impl Consumer {
    pub fn new(region: Arc<Region>, dir: Direction) -> Self {
        let base = ring_base(&region, dir);
        // SAFETY: control offsets of a ring base.
        let tail = unsafe { word64(base, TAIL) }.load(Ordering::Acquire);
        Self {
            _region: region,
            base,
            read: tail,
            peeked: None,
        }
    }

    /// The next frame, without consuming it; [`Consumer::advance`] moves
    /// past it.
    pub fn peek(&mut self) -> Next {
        self.peeked = None;
        // A wrap marker, then at most one real record.
        for _ in 0..2 {
            // SAFETY: control offsets of a ring base.
            let head = unsafe { word64(self.base, HEAD) }.load(Ordering::Acquire);
            if head < self.read || head - self.read > DATA as u64 {
                return Next::Corrupt("ring indices out of range");
            }
            if head == self.read {
                return Next::Empty;
            }
            let avail = (head - self.read) as usize;
            let pos = (self.read % DATA as u64) as usize;
            let till_end = DATA - pos;
            // `read` only ever moves by validated strides, so `pos` is
            // aligned and a header fits before the end.
            // SAFETY: `pos + HDR <= DATA`, inside this ring's data area.
            let (len, next) = unsafe {
                let p = self.base.add(CONTROL + pos);
                (load_field(p), load_field(p.add(4)) as usize)
            };
            if next > avail {
                return Next::Corrupt("record runs past the published head");
            }
            if len == WRAP {
                if next != till_end {
                    return Next::Corrupt("wrap marker not at the end");
                }
                self.read += next as u64;
                continue;
            }
            let len = len as usize;
            if len > MAX_FRAME || next % ALIGN != 0 || next < record_len(len) {
                return Next::Corrupt("bad record length");
            }
            if next > till_end {
                return Next::Corrupt("record overlaps the end of the ring");
            }
            // SAFETY: `pos + HDR + len <= pos + next <= DATA`.
            let data = unsafe { self.base.add(CONTROL + pos + HDR) };
            self.peeked = Some(next as u64);
            return Next::Frame { data, len };
        }
        Next::Corrupt("two wrap markers in a row")
    }

    /// Moves past the frame `peek` returned (it stays readable until
    /// [`Consumer::release`]). Nothing happens without one.
    pub fn advance(&mut self) {
        if let Some(next) = self.peeked.take() {
            self.read += next;
        }
    }

    /// Hands everything advanced past back to the producer. `true` when the
    /// producer is waiting for space and must be kicked.
    pub fn release(&mut self) -> bool {
        // SAFETY: control offsets of a ring base.
        unsafe { word64(self.base, TAIL) }.store(self.read, Ordering::SeqCst);
        // SAFETY: as above.
        unsafe { word32(self.base, PRODUCER_WAITING) }.swap(0, Ordering::SeqCst) != 0
    }

    /// Before sleeping: asks to be kicked when the producer publishes.
    /// `false` when something arrived meanwhile (do not sleep).
    pub fn prepare_sleep(&mut self) -> bool {
        // SAFETY: control offsets of a ring base.
        unsafe { word32(self.base, CONSUMER_SLEEPING) }.store(1, Ordering::SeqCst);
        // SAFETY: as above.
        let head = unsafe { word64(self.base, HEAD) }.load(Ordering::SeqCst);
        if head != self.read {
            // SAFETY: as above.
            unsafe { word32(self.base, CONSUMER_SLEEPING) }.store(0, Ordering::SeqCst);
            return false;
        }
        true
    }
}

#[cfg(test)]
mod tests;
