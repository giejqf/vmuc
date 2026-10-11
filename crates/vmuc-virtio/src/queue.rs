// SPDX-License-Identifier: Apache-2.0
//! The split virtqueue (virtio 1.2 §2.7), device side.
//!
//! A worker pops every available chain, completes them with
//! [`Queue::add_used`], publishes the batch once with
//! [`Queue::publish_used`], and interrupts only if
//! [`Queue::needs_interrupt`] says the driver wants one. With
//! `VIRTIO_F_RING_EVENT_IDX` both directions are suppressed through the
//! event indices; [`Queue::enable_notification`] re-arms driver kicks and
//! reports whether more work slipped in meanwhile.

use std::sync::atomic::{Ordering, fence};

use crate::mem::{Gpa, GuestMemory, MemError};

pub const VIRTQ_DESC_F_NEXT: u16 = 1;
pub const VIRTQ_DESC_F_WRITE: u16 = 2;
pub const VIRTQ_DESC_F_INDIRECT: u16 = 4;
const VIRTQ_AVAIL_F_NO_INTERRUPT: u16 = 1;
const VIRTQ_USED_F_NO_NOTIFY: u16 = 1;

/// Largest split queue the spec allows.
pub const MAX_QUEUE_SIZE: u16 = 32768;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum QueueError {
    #[error(transparent)]
    Memory(#[from] MemError),
    #[error("driver published {0} new buffers, more than the queue holds")]
    AvailOverrun(u16),
    #[error("descriptor index {0} out of range")]
    BadIndex(u16),
    #[error("descriptor chain longer than the queue (loop?)")]
    ChainTooLong,
    #[error("indirect table of {0} bytes is malformed")]
    BadIndirect(u32),
}

/// One buffer of a chain, in guest memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Desc {
    pub addr: Gpa,
    pub len: u32,
    /// Device-writable (`VIRTQ_DESC_F_WRITE`).
    pub write: bool,
}

/// A popped descriptor chain: complete it with its `head`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DescChain {
    pub head: u16,
    pub descs: Vec<Desc>,
}

impl DescChain {
    pub fn readable(&self) -> impl Iterator<Item = &Desc> {
        self.descs.iter().filter(|d| !d.write)
    }

    pub fn writable(&self) -> impl Iterator<Item = &Desc> {
        self.descs.iter().filter(|d| d.write)
    }

    pub fn readable_len(&self) -> u64 {
        self.readable().map(|d| u64::from(d.len)).sum()
    }

    pub fn writable_len(&self) -> u64 {
        self.writable().map(|d| u64::from(d.len)).sum()
    }
}

/// Driver-configured queue state plus the device's ring cursors.
#[derive(Debug, Clone, Default)]
pub struct Queue {
    pub max_size: u16,
    pub size: u16,
    pub ready: bool,
    pub desc: Gpa,
    pub avail: Gpa,
    pub used: Gpa,
    pub event_idx: bool,
    next_avail: u16,
    next_used: u16,
    /// `next_used` when the driver was last interrupted about this queue.
    signalled_used: u16,
}

impl Queue {
    pub fn new(max_size: u16) -> Self {
        Self {
            max_size,
            size: max_size,
            ..Self::default()
        }
    }

    /// Whether the driver's configuration is usable: a power-of-two
    /// size within bounds and aligned rings.
    pub fn is_valid(&self) -> bool {
        self.ready
            && self.size.is_power_of_two()
            && self.size <= self.max_size
            && self.desc.is_multiple_of(16)
            && self.avail.is_multiple_of(2)
            && self.used.is_multiple_of(4)
    }

    /// Back to the state after device reset (configuration kept for the
    /// transport to overwrite).
    pub fn reset(&mut self) {
        *self = Self::new(self.max_size);
    }

    fn avail_ring(&self, slot: u16) -> Gpa {
        self.avail + 4 + 2 * u64::from(slot % self.size)
    }

    fn used_event_addr(&self) -> Gpa {
        self.avail + 4 + 2 * u64::from(self.size)
    }

    fn avail_event_addr(&self) -> Gpa {
        self.used + 4 + 8 * u64::from(self.size)
    }

    /// Pops the next available chain, `None` when the ring is empty.
    pub fn pop(&mut self, mem: &GuestMemory) -> Result<Option<DescChain>, QueueError> {
        let avail_idx = mem.load_u16_acquire(self.avail + 2)?;
        let pending = avail_idx.wrapping_sub(self.next_avail);
        if pending == 0 {
            return Ok(None);
        }
        if pending > self.size {
            return Err(QueueError::AvailOverrun(pending));
        }
        let head: u16 = mem.read_obj(self.avail_ring(self.next_avail))?;
        self.next_avail = self.next_avail.wrapping_add(1);
        let mut chain = DescChain {
            head,
            descs: Vec::with_capacity(4),
        };
        self.read_chain(mem, head, &mut chain.descs)?;
        Ok(Some(chain))
    }

    fn read_desc(
        mem: &GuestMemory,
        table: Gpa,
        index: u16,
    ) -> Result<(Desc, u16, u16), QueueError> {
        let at = table + 16 * u64::from(index);
        let addr: u64 = mem.read_obj(at)?;
        let len: u32 = mem.read_obj(at + 8)?;
        let flags: u16 = mem.read_obj(at + 12)?;
        let next: u16 = mem.read_obj(at + 14)?;
        let desc = Desc {
            addr,
            len,
            write: flags & VIRTQ_DESC_F_WRITE != 0,
        };
        Ok((desc, flags, next))
    }

    fn read_chain(
        &self,
        mem: &GuestMemory,
        head: u16,
        out: &mut Vec<Desc>,
    ) -> Result<(), QueueError> {
        let mut index = head;
        for _ in 0..self.size {
            if index >= self.size {
                return Err(QueueError::BadIndex(index));
            }
            let (desc, flags, next) = Self::read_desc(mem, self.desc, index)?;
            if flags & VIRTQ_DESC_F_INDIRECT != 0 {
                Self::read_indirect(mem, desc, out)?;
            } else {
                out.push(desc);
            }
            if flags & VIRTQ_DESC_F_NEXT == 0 {
                return Ok(());
            }
            index = next;
        }
        Err(QueueError::ChainTooLong)
    }

    fn read_indirect(
        mem: &GuestMemory,
        table: Desc,
        out: &mut Vec<Desc>,
    ) -> Result<(), QueueError> {
        if table.len == 0 || !table.len.is_multiple_of(16) {
            return Err(QueueError::BadIndirect(table.len));
        }
        let count = (table.len / 16) as u16;
        let mut index = 0u16;
        for _ in 0..count {
            if index >= count {
                return Err(QueueError::BadIndex(index));
            }
            let (desc, flags, next) = Self::read_desc(mem, table.addr, index)?;
            if flags & VIRTQ_DESC_F_INDIRECT != 0 {
                return Err(QueueError::BadIndirect(table.len));
            }
            out.push(desc);
            if flags & VIRTQ_DESC_F_NEXT == 0 {
                return Ok(());
            }
            index = next;
        }
        Err(QueueError::ChainTooLong)
    }

    /// Records a completed chain; invisible to the driver until
    /// [`Queue::publish_used`].
    pub fn add_used(&mut self, mem: &GuestMemory, head: u16, len: u32) -> Result<(), QueueError> {
        let at = self.used + 4 + 8 * u64::from(self.next_used % self.size);
        mem.write_obj(at, u32::from(head))?;
        mem.write_obj(at + 4, len)?;
        self.next_used = self.next_used.wrapping_add(1);
        Ok(())
    }

    /// Makes every `add_used` so far visible (release-ordered).
    pub fn publish_used(&mut self, mem: &GuestMemory) -> Result<(), QueueError> {
        mem.store_u16_release(self.used + 2, self.next_used)?;
        Ok(())
    }

    /// After [`Queue::publish_used`]: does the driver want an interrupt
    /// for the buffers used since the last one?
    pub fn needs_interrupt(&mut self, mem: &GuestMemory) -> Result<bool, QueueError> {
        let (old, new) = (self.signalled_used, self.next_used);
        if old == new {
            return Ok(false);
        }
        // The used index store must be visible before the driver's
        // event/flags are read, or both sides can decide to sleep.
        fence(Ordering::SeqCst);
        let want = if self.event_idx {
            let event: u16 = mem.read_obj(self.used_event_addr())?;
            new.wrapping_sub(event).wrapping_sub(1) < new.wrapping_sub(old)
        } else {
            let flags: u16 = mem.read_obj(self.avail)?;
            flags & VIRTQ_AVAIL_F_NO_INTERRUPT == 0
        };
        if want {
            self.signalled_used = new;
        }
        Ok(want)
    }

    /// Asks the driver to kick on the next buffer, then reports whether
    /// buffers arrived before the request was visible (the caller must
    /// process them instead of sleeping).
    pub fn enable_notification(&mut self, mem: &GuestMemory) -> Result<bool, QueueError> {
        if self.event_idx {
            mem.write_obj(self.avail_event_addr(), self.next_avail)?;
        } else {
            mem.write_obj(self.used, 0u16)?;
        }
        fence(Ordering::SeqCst);
        Ok(mem.load_u16_acquire(self.avail + 2)? != self.next_avail)
    }

    /// Tells the driver not to kick (without EVENT_IDX; with it, simply
    /// not advancing `avail_event` has the same effect).
    pub fn disable_notification(&mut self, mem: &GuestMemory) -> Result<(), QueueError> {
        if !self.event_idx {
            mem.write_obj(self.used, VIRTQ_USED_F_NO_NOTIFY)?;
        }
        Ok(())
    }
}

#[cfg(test)]
pub(crate) mod testing {
    //! A guest-side driver for unit tests: lays out a queue in a
    //! Vec-backed guest memory, posts chains, reads completions.

    use super::*;

    pub struct Driver {
        pub mem: GuestMemory,
        pub queue: Queue,
        free_desc: u16,
        avail_idx: u16,
        used_seen: u16,
        /// Next free byte for buffers.
        heap: Gpa,
    }

    pub const BASE: Gpa = 0x1000;

    impl Driver {
        pub fn new(size: u16, event_idx: bool) -> Self {
            Self::in_region(
                GuestMemory::from_vec(BASE, vec![0; 1 << 20]),
                BASE,
                size,
                event_idx,
            )
        }

        /// `n` queues sharing one guest memory, each in its own 1 MiB region.
        pub fn many(n: usize, size: u16, event_idx: bool) -> Vec<Self> {
            let mem = GuestMemory::from_vec(BASE, vec![0; n << 20]);
            (0..n)
                .map(|i| Self::in_region(mem.clone(), BASE + ((i as u64) << 20), size, event_idx))
                .collect()
        }

        fn in_region(mem: GuestMemory, base: Gpa, size: u16, event_idx: bool) -> Self {
            let desc = base;
            let avail = desc + 16 * u64::from(size);
            let used = (avail + 6 + 2 * u64::from(size)).next_multiple_of(4);
            let heap = (used + 6 + 8 * u64::from(size)).next_multiple_of(4096);
            let queue = Queue {
                max_size: size,
                size,
                ready: true,
                desc,
                avail,
                used,
                event_idx,
                ..Queue::default()
            };
            Self {
                mem,
                queue,
                free_desc: 0,
                avail_idx: 0,
                used_seen: 0,
                heap,
            }
        }

        /// Allocates a guest buffer holding `data` (or `len` zero bytes).
        pub fn buffer(&mut self, data: &[u8], len: u32) -> Gpa {
            let at = self.heap;
            self.mem.write(at, data).unwrap();
            self.heap = (at + u64::from(len.max(data.len() as u32))).next_multiple_of(16);
            at
        }

        fn write_desc(&self, i: u16, addr: Gpa, len: u32, flags: u16, next: u16) {
            let at = self.queue.desc + 16 * u64::from(i);
            self.mem.write_obj(at, addr).unwrap();
            self.mem.write_obj(at + 8, len).unwrap();
            self.mem.write_obj(at + 12, flags).unwrap();
            self.mem.write_obj(at + 14, next).unwrap();
        }

        /// Posts a chain of `(addr, len, device_writable)` and returns its head.
        pub fn post(&mut self, bufs: &[(Gpa, u32, bool)]) -> u16 {
            let head = self.free_desc;
            for (n, &(addr, len, w)) in bufs.iter().enumerate() {
                let i = self.free_desc;
                self.free_desc += 1;
                let last = n + 1 == bufs.len();
                let flags =
                    (u16::from(w) * VIRTQ_DESC_F_WRITE) | (u16::from(!last) * VIRTQ_DESC_F_NEXT);
                self.write_desc(i, addr, len, flags, i + 1);
            }
            self.publish(head);
            head
        }

        /// Posts one indirect descriptor pointing at a table of `bufs`.
        pub fn post_indirect(&mut self, bufs: &[(Gpa, u32, bool)]) -> u16 {
            let table = self.buffer(&[], 16 * bufs.len() as u32);
            for (n, &(addr, len, w)) in bufs.iter().enumerate() {
                let at = table + 16 * n as u64;
                let last = n + 1 == bufs.len();
                let flags =
                    (u16::from(w) * VIRTQ_DESC_F_WRITE) | (u16::from(!last) * VIRTQ_DESC_F_NEXT);
                self.mem.write_obj(at, addr).unwrap();
                self.mem.write_obj(at + 8, len).unwrap();
                self.mem.write_obj(at + 12, flags).unwrap();
                self.mem.write_obj(at + 14, n as u16 + 1).unwrap();
            }
            let head = self.free_desc;
            self.free_desc += 1;
            self.write_desc(
                head,
                table,
                16 * bufs.len() as u32,
                VIRTQ_DESC_F_INDIRECT,
                0,
            );
            self.publish(head);
            head
        }

        fn publish(&mut self, head: u16) {
            let slot = self.avail_idx % self.queue.size;
            self.mem
                .write_obj(self.queue.avail + 4 + 2 * u64::from(slot), head)
                .unwrap();
            self.avail_idx = self.avail_idx.wrapping_add(1);
            self.mem
                .store_u16_release(self.queue.avail + 2, self.avail_idx)
                .unwrap();
        }

        /// Completions the device published since the last call.
        pub fn used(&mut self) -> Vec<(u32, u32)> {
            let idx = self.mem.load_u16_acquire(self.queue.used + 2).unwrap();
            let mut out = Vec::new();
            while self.used_seen != idx {
                let at = self.queue.used + 4 + 8 * u64::from(self.used_seen % self.queue.size);
                out.push((
                    self.mem.read_obj(at).unwrap(),
                    self.mem.read_obj(at + 4).unwrap(),
                ));
                self.used_seen = self.used_seen.wrapping_add(1);
            }
            out
        }

        /// Driver-side `used_event` (EVENT_IDX): interrupt me after this.
        pub fn set_used_event(&self, v: u16) {
            let at = self.queue.avail + 4 + 2 * u64::from(self.queue.size);
            self.mem.write_obj(at, v).unwrap();
        }

        pub fn avail_event(&self) -> u16 {
            let at = self.queue.used + 4 + 8 * u64::from(self.queue.size);
            self.mem.read_obj(at).unwrap()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::Driver;
    use super::*;

    #[test]
    fn pops_chains_in_order_and_splits_readable_writable() {
        let mut d = Driver::new(8, false);
        let hdr = d.buffer(b"header", 6);
        let out = d.buffer(&[], 64);
        d.post(&[(hdr, 6, false), (out, 64, true)]);
        d.post(&[(hdr, 6, false)]);
        let mem = d.mem.clone();
        let mut q = d.queue.clone();
        let c = q.pop(&mem).unwrap().unwrap();
        assert_eq!(c.head, 0);
        assert_eq!((c.readable_len(), c.writable_len()), (6, 64));
        assert_eq!(q.pop(&mem).unwrap().unwrap().head, 2);
        assert_eq!(q.pop(&mem).unwrap(), None);
    }

    #[test]
    fn indirect_tables_are_flattened() {
        let mut d = Driver::new(4, false);
        let first = d.buffer(b"abc", 3);
        let second = d.buffer(&[], 8);
        d.post_indirect(&[(first, 3, false), (second, 8, true)]);
        let mem = d.mem.clone();
        let mut q = d.queue.clone();
        let c = q.pop(&mem).unwrap().unwrap();
        assert_eq!(
            c.descs,
            vec![
                Desc {
                    addr: first,
                    len: 3,
                    write: false
                },
                Desc {
                    addr: second,
                    len: 8,
                    write: true
                },
            ]
        );
    }

    #[test]
    fn used_ring_publishes_in_batches() {
        let mut d = Driver::new(4, false);
        let buf = d.buffer(&[], 16);
        let h0 = d.post(&[(buf, 16, true)]);
        let h1 = d.post(&[(buf, 16, true)]);
        let mem = d.mem.clone();
        let mut q = d.queue.clone();
        q.pop(&mem).unwrap();
        q.pop(&mem).unwrap();
        q.add_used(&mem, h0, 5).unwrap();
        q.add_used(&mem, h1, 7).unwrap();
        assert!(d.used().is_empty(), "nothing visible before publish");
        q.publish_used(&mem).unwrap();
        assert_eq!(d.used(), vec![(u32::from(h0), 5), (u32::from(h1), 7)]);
        assert!(q.needs_interrupt(&mem).unwrap());
        assert!(!q.needs_interrupt(&mem).unwrap(), "already signalled");
    }

    #[test]
    fn event_idx_suppresses_interrupts_until_the_driver_asks() {
        let mut d = Driver::new(8, true);
        let buf = d.buffer(&[], 16);
        let mem = d.mem.clone();
        let mut q = d.queue.clone();
        // The driver wants an interrupt only after the third completion.
        d.set_used_event(2);
        for _ in 0..3 {
            let h = d.post(&[(buf, 16, true)]);
            q.pop(&mem).unwrap();
            q.add_used(&mem, h, 1).unwrap();
            q.publish_used(&mem).unwrap();
            let want = q.needs_interrupt(&mem).unwrap();
            assert_eq!(want, q.next_used == 3, "after {} used", q.next_used);
        }
    }

    #[test]
    fn enable_notification_reports_buffers_that_raced_in() {
        let mut d = Driver::new(8, true);
        let buf = d.buffer(&[], 16);
        let mem = d.mem.clone();
        let mut q = d.queue.clone();
        assert!(!q.enable_notification(&mem).unwrap());
        assert_eq!(d.avail_event(), 0);
        d.post(&[(buf, 16, true)]);
        assert!(q.enable_notification(&mem).unwrap(), "must not sleep now");
        q.pop(&mem).unwrap();
        assert!(!q.enable_notification(&mem).unwrap());
        assert_eq!(d.avail_event(), 1);
    }

    #[test]
    fn malformed_rings_are_errors_not_hangs() {
        let mut d = Driver::new(4, false);
        let buf = d.buffer(&[], 16);
        let mem = d.mem.clone();
        // A descriptor whose `next` points at itself.
        let at = d.queue.desc;
        mem.write_obj(at, buf).unwrap();
        mem.write_obj(at + 8, 16u32).unwrap();
        mem.write_obj(at + 12, VIRTQ_DESC_F_NEXT).unwrap();
        mem.write_obj(at + 14, 0u16).unwrap();
        mem.write_obj(d.queue.avail + 4, 0u16).unwrap();
        mem.store_u16_release(d.queue.avail + 2, 1).unwrap();
        let mut q = d.queue.clone();
        assert_eq!(q.pop(&mem), Err(QueueError::ChainTooLong));
        // The driver claims more buffers than the ring has.
        let mut q = d.queue.clone();
        mem.store_u16_release(d.queue.avail + 2, 9).unwrap();
        assert_eq!(q.pop(&mem), Err(QueueError::AvailOverrun(9)));
        // A head beyond the table.
        let mut q = d.queue.clone();
        mem.write_obj(d.queue.avail + 4, 7u16).unwrap();
        mem.store_u16_release(d.queue.avail + 2, 1).unwrap();
        assert_eq!(q.pop(&mem), Err(QueueError::BadIndex(7)));
    }

    #[test]
    fn validity_checks_size_and_alignment() {
        let mut q = Driver::new(8, false).queue;
        assert!(q.is_valid());
        q.size = 6;
        assert!(!q.is_valid());
        q.size = 16;
        assert!(!q.is_valid(), "above max");
        q.size = 8;
        q.used += 2;
        assert!(!q.is_valid());
    }
}
