// SPDX-License-Identifier: Apache-2.0
//! The MMIO bus: which device answers a data abort at a guest physical
//! address. Built before the vCPUs start and immutable afterwards, so
//! vCPU threads share it without a lock.

use std::sync::Arc;

use vmuc_virtio::BusDevice;

#[derive(Debug, thiserror::Error)]
#[error("MMIO window {base:#x}+{size:#x} overlaps an existing device")]
pub struct Overlap {
    pub base: u64,
    pub size: u64,
}

struct Entry {
    base: u64,
    size: u64,
    device: Arc<dyn BusDevice>,
}

#[derive(Default)]
pub struct MmioBus {
    /// Sorted by base, non-overlapping.
    entries: Vec<Entry>,
}

impl std::fmt::Debug for MmioBus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list()
            .entries(self.entries.iter().map(|e| (e.base, e.size)))
            .finish()
    }
}

impl MmioBus {
    pub fn insert(
        &mut self,
        base: u64,
        size: u64,
        device: Arc<dyn BusDevice>,
    ) -> Result<(), Overlap> {
        let at = self.entries.partition_point(|e| e.base < base);
        let clashes_prev = at > 0 && self.entries[at - 1].base + self.entries[at - 1].size > base;
        let clashes_next = self.entries.get(at).is_some_and(|e| base + size > e.base);
        if size == 0 || clashes_prev || clashes_next {
            return Err(Overlap { base, size });
        }
        self.entries.insert(at, Entry { base, size, device });
        Ok(())
    }

    /// The device covering `addr` and the offset into its window.
    pub fn find(&self, addr: u64) -> Option<(&dyn BusDevice, u64)> {
        let at = self.entries.partition_point(|e| e.base <= addr);
        let e = self.entries.get(at.checked_sub(1)?)?;
        (addr - e.base < e.size).then(|| (&*e.device, addr - e.base))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    #[derive(Default)]
    struct Probe(AtomicU64);
    impl BusDevice for Probe {
        fn read(&self, offset: u64, data: &mut [u8]) {
            data[0] = offset as u8;
        }
        fn write(&self, offset: u64, _data: &[u8]) {
            self.0.store(offset, Ordering::SeqCst);
        }
    }

    #[test]
    fn finds_devices_and_rejects_overlaps() {
        let mut bus = MmioBus::default();
        let a = Arc::new(Probe::default());
        bus.insert(0x0900_0000, 0x1000, a.clone()).unwrap();
        bus.insert(0x0a00_0000, 0x1000, Arc::new(Probe::default()))
            .unwrap();
        bus.insert(0x0800_0000, 0x1000, Arc::new(Probe::default()))
            .unwrap();
        assert!(
            bus.insert(0x0900_0800, 0x1000, Arc::new(Probe::default()))
                .is_err()
        );
        assert!(
            bus.insert(0x08ff_f800, 0x1000, Arc::new(Probe::default()))
                .is_err()
        );
        assert!(
            bus.insert(0x0b00_0000, 0, Arc::new(Probe::default()))
                .is_err()
        );

        let (dev, off) = bus.find(0x0900_0018).unwrap();
        assert_eq!(off, 0x18);
        dev.write(off, &[0]);
        assert_eq!(a.0.load(Ordering::SeqCst), 0x18);
        assert!(bus.find(0x0900_1000).is_none());
        assert!(bus.find(0x07ff_ffff).is_none());
        assert!(bus.find(0x0a00_0fff).is_some());
    }
}
