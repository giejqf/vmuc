// SPDX-License-Identifier: Apache-2.0
//! ARM PrimeCell PL011 UART: the early console (`earlycon=pl011`) and a
//! serial console for kernels booted without virtio-console.
//!
//! Interrupt model, matched to the Linux `amba-pl011` driver:
//! - TX drains instantly, so the TX raw status is always set; the driver
//!   writes while TXIM is enabled and masks it when its buffer is empty.
//! - RX and receive-timeout raw status mean "input is waiting"; the
//!   driver reads `DR` until `FR.RXFE` and never clears them through ICR.
//! - Loopback (`CR.LBE`) sends transmitted bytes to the receive side.
//!
//! Not the data path: one lock per register access is fine.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, PoisonError};

use vmuc_virtio::bus::{le_value, put_le};
use vmuc_virtio::{BusDevice, Interrupt};

use super::console::ConsoleOut;
use super::primecell_id;

const DR: u64 = 0x000;
const RSR: u64 = 0x004;
const FR: u64 = 0x018;
const ILPR: u64 = 0x020;
const IBRD: u64 = 0x024;
const FBRD: u64 = 0x028;
const LCR_H: u64 = 0x02c;
const CR: u64 = 0x030;
const IFLS: u64 = 0x034;
const IMSC: u64 = 0x038;
const RIS: u64 = 0x03c;
const MIS: u64 = 0x040;
const ICR: u64 = 0x044;
const DMACR: u64 = 0x048;

const FR_RXFE: u64 = 1 << 4;
const FR_RXFF: u64 = 1 << 6;
const FR_TXFE: u64 = 1 << 7;

const INT_RX: u32 = 1 << 4;
const INT_TX: u32 = 1 << 5;
const INT_RT: u32 = 1 << 6;
const INT_ALL: u32 = 0x7ff;

const CR_LBE: u32 = 1 << 7;

/// PL011 r1p5, as the Linux driver expects to find.
const PERIPH_ID: [u8; 4] = [0x11, 0x10, 0x34, 0x00];

/// Host-side input held for the guest; more than the 32-byte hardware
/// FIFO so a paste is not dropped while the guest catches up.
const RX_LIMIT: usize = 64 * 1024;

#[derive(Default)]
struct Regs {
    rx: VecDeque<u8>,
    /// Raw status bits other than RX/RT/TX, which are derived.
    ris_latched: u32,
    imsc: u32,
    cr: u32,
    lcr_h: u32,
    ibrd: u32,
    fbrd: u32,
    ifls: u32,
    ilpr: u32,
    dmacr: u32,
    level: bool,
}

impl Regs {
    fn ris(&self) -> u32 {
        let rx = if self.rx.is_empty() {
            0
        } else {
            INT_RX | INT_RT
        };
        self.ris_latched | rx | INT_TX
    }

    fn mis(&self) -> u32 {
        self.ris() & self.imsc
    }
}

pub struct Pl011 {
    regs: Mutex<Regs>,
    irq: Arc<dyn Interrupt>,
    out: ConsoleOut,
}

impl Pl011 {
    pub fn new(irq: Arc<dyn Interrupt>, out: ConsoleOut) -> Self {
        Self {
            regs: Mutex::new(Regs {
                cr: 0x300,
                ifls: 0x12,
                ..Regs::default()
            }),
            irq,
            out,
        }
    }

    /// Queues host input (keyboard) for the guest.
    pub fn receive(&self, bytes: &[u8]) {
        let mut r = self.lock();
        let room = RX_LIMIT.saturating_sub(r.rx.len());
        if room < bytes.len() {
            tracing::warn!(dropped = bytes.len() - room, "pl011 input overflow");
        }
        r.rx.extend(&bytes[..bytes.len().min(room)]);
        self.update(&mut r);
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Regs> {
        self.regs.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Drives the SPI to `MIS != 0`, touching the GIC only on a change.
    fn update(&self, r: &mut Regs) {
        let level = r.mis() != 0;
        if level != r.level {
            r.level = level;
            self.irq.set_level(level);
        }
    }
}

impl BusDevice for Pl011 {
    fn read(&self, offset: u64, data: &mut [u8]) {
        let mut r = self.lock();
        let v = match offset {
            DR => {
                let v = r.rx.pop_front().map_or(0, u64::from);
                self.update(&mut r);
                v
            }
            FR => {
                let mut fr = FR_TXFE;
                fr |= if r.rx.is_empty() { FR_RXFE } else { 0 };
                fr |= if r.rx.len() >= 32 { FR_RXFF } else { 0 };
                fr
            }
            ILPR => u64::from(r.ilpr),
            IBRD => u64::from(r.ibrd),
            FBRD => u64::from(r.fbrd),
            LCR_H => u64::from(r.lcr_h),
            CR => u64::from(r.cr),
            IFLS => u64::from(r.ifls),
            IMSC => u64::from(r.imsc),
            RIS => u64::from(r.ris()),
            MIS => u64::from(r.mis()),
            DMACR => u64::from(r.dmacr),
            _ => primecell_id(offset, PERIPH_ID).unwrap_or(0),
        };
        put_le(data, v);
    }

    fn write(&self, offset: u64, data: &[u8]) {
        let v = le_value(data) as u32;
        let mut r = self.lock();
        match offset {
            DR => {
                let byte = v as u8;
                if r.cr & CR_LBE != 0 {
                    r.rx.push_back(byte);
                } else {
                    self.out.push(&[byte]);
                }
            }
            RSR => {}
            ILPR => r.ilpr = v,
            IBRD => r.ibrd = v,
            FBRD => r.fbrd = v,
            LCR_H => r.lcr_h = v,
            CR => r.cr = v,
            IFLS => r.ifls = v,
            IMSC => r.imsc = v & INT_ALL,
            ICR => r.ris_latched &= !v,
            DMACR => r.dmacr = v,
            _ => tracing::debug!(offset, v, "pl011: write to unknown register"),
        }
        self.update(&mut r);
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use vmuc_virtio::TestIrq;

    use super::*;

    #[derive(Clone, Default)]
    struct Sink(Arc<Mutex<Vec<u8>>>);
    impl Write for Sink {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn rd(u: &Pl011, off: u64) -> u32 {
        let mut d = [0u8; 4];
        u.read(off, &mut d);
        u32::from_le_bytes(d)
    }

    fn wr(u: &Pl011, off: u64, v: u32) {
        u.write(off, &v.to_le_bytes());
    }

    fn uart() -> (Pl011, Arc<TestIrq>, Sink, std::thread::JoinHandle<()>) {
        let irq = Arc::new(TestIrq::default());
        let sink = Sink::default();
        let (out, h) = ConsoleOut::spawn(vec![Box::new(sink.clone())]);
        (Pl011::new(irq.clone(), out), irq, sink, h)
    }

    #[test]
    fn transmit_reaches_console_and_tx_interrupt_follows_mask() {
        let (u, irq, sink, h) = uart();
        for b in b"hi\n" {
            wr(&u, DR, u32::from(*b));
        }
        assert!(!irq.level());
        wr(&u, IMSC, INT_TX);
        assert!(irq.level(), "TX FIFO is always empty, so TXIM asserts");
        assert_eq!(rd(&u, MIS), INT_TX);
        wr(&u, IMSC, 0);
        assert!(!irq.level());
        u.out.close();
        h.join().unwrap();
        assert_eq!(*sink.0.lock().unwrap(), b"hi\n");
    }

    #[test]
    fn receive_raises_rx_until_drained() {
        let (u, irq, _sink, _h) = uart();
        wr(&u, IMSC, INT_RX | INT_RT);
        assert_eq!(rd(&u, FR) & FR_RXFE as u32, FR_RXFE as u32);
        u.receive(b"ab");
        assert!(irq.level());
        assert_eq!(rd(&u, FR) & FR_RXFE as u32, 0);
        // The driver's ICR write leaves RX/RT alone; draining clears them.
        wr(&u, ICR, INT_ALL & !(INT_TX | INT_RT | INT_RX));
        assert!(irq.level());
        assert_eq!(rd(&u, DR), u32::from(b'a'));
        assert!(irq.level());
        assert_eq!(rd(&u, DR), u32::from(b'b'));
        assert!(!irq.level());
        assert_eq!(irq.history(), vec![true, false]);
    }

    #[test]
    fn loopback_and_identification() {
        let (u, _irq, sink, h) = uart();
        wr(&u, CR, CR_LBE | 0x301);
        wr(&u, DR, u32::from(b'x'));
        assert_eq!(rd(&u, DR), u32::from(b'x'));
        assert_eq!(rd(&u, 0xfe0), 0x11);
        assert_eq!(rd(&u, 0xff0), 0x0d);
        assert_eq!(rd(&u, 0xffc), 0xb1);
        u.out.close();
        h.join().unwrap();
        assert!(sink.0.lock().unwrap().is_empty());
    }
}
