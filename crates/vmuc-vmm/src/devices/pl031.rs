// SPDX-License-Identifier: Apache-2.0
//! ARM PrimeCell PL031 real-time clock: gives the guest wall-clock time
//! at boot (`RTC_HCTOSYS`), so it does not start in 1970. The counter is
//! the host's Unix time plus whatever offset the guest loads into `LR`.
//! The alarm is stored but never fires; nothing in a vmuc guest uses it.

use std::sync::{Mutex, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use vmuc_virtio::BusDevice;
use vmuc_virtio::bus::{le_value, put_le};

use super::primecell_id;

const DR: u64 = 0x000;
const MR: u64 = 0x004;
const LR: u64 = 0x008;
const CR: u64 = 0x00c;
const IMSC: u64 = 0x010;
const RIS: u64 = 0x014;
const MIS: u64 = 0x018;
const ICR: u64 = 0x01c;

const PERIPH_ID: [u8; 4] = [0x31, 0x10, 0x04, 0x00];

#[derive(Default)]
struct Regs {
    /// Seconds added to host time (set by writes to `LR`).
    offset: i64,
    mr: u32,
    imsc: u32,
}

/// Source of host Unix time, injectable for tests.
pub type Clock = fn() -> i64;

fn host_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

pub struct Pl031 {
    regs: Mutex<Regs>,
    clock: Clock,
}

impl Default for Pl031 {
    fn default() -> Self {
        Self::with_clock(host_now)
    }
}

impl Pl031 {
    pub fn with_clock(clock: Clock) -> Self {
        Self {
            regs: Mutex::new(Regs::default()),
            clock,
        }
    }

    fn now(&self, r: &Regs) -> u32 {
        ((self.clock)() + r.offset) as u32
    }
}

impl BusDevice for Pl031 {
    fn read(&self, offset: u64, data: &mut [u8]) {
        let r = self.regs.lock().unwrap_or_else(PoisonError::into_inner);
        let v = match offset {
            // LR reads back the last loaded value, i.e. the running count.
            DR | LR => u64::from(self.now(&r)),
            MR => u64::from(r.mr),
            CR => 1,
            IMSC => u64::from(r.imsc),
            RIS | MIS => 0,
            _ => primecell_id(offset, PERIPH_ID).unwrap_or(0),
        };
        put_le(data, v);
    }

    fn write(&self, offset: u64, data: &[u8]) {
        let v = le_value(data) as u32;
        let mut r = self.regs.lock().unwrap_or_else(PoisonError::into_inner);
        match offset {
            LR => r.offset = i64::from(v) - (self.clock)(),
            MR => r.mr = v,
            IMSC => r.imsc = v & 1,
            CR | ICR => {}
            _ => tracing::debug!(offset, v, "pl031: write to unknown register"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rd(d: &Pl031, off: u64) -> u32 {
        let mut b = [0u8; 4];
        d.read(off, &mut b);
        u32::from_le_bytes(b)
    }

    #[test]
    fn counts_host_time_and_honours_load() {
        let rtc = Pl031::with_clock(|| 1_791_000_000);
        assert_eq!(rd(&rtc, DR), 1_791_000_000);
        assert_eq!(rd(&rtc, CR), 1);
        rtc.write(LR, &1_000u32.to_le_bytes());
        assert_eq!(rd(&rtc, DR), 1_000);
        assert_eq!(rd(&rtc, 0xfe0), 0x31);
        assert_eq!(rd(&rtc, 0xfe8), 0x04);
    }

    #[test]
    fn real_clock_is_after_2026() {
        assert!(rd(&Pl031::default(), DR) > 1_767_225_600);
    }
}
