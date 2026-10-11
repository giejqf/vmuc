// SPDX-License-Identifier: Apache-2.0
//! Platform devices that are not virtio: the PrimeCell UART and RTC,
//! and the console output they share with virtio-console.

pub mod console;
pub mod pl011;
pub mod pl031;

/// PrimeCell identification registers (`0xFE0..=0xFFC`), byte per word.
fn primecell_id(offset: u64, periph: [u8; 4]) -> Option<u64> {
    const CELL: [u8; 4] = [0x0d, 0xf0, 0x05, 0xb1];
    match offset {
        0xfe0..=0xfec => Some(u64::from(periph[(offset as usize - 0xfe0) / 4])),
        0xff0..=0xffc => Some(u64::from(CELL[(offset as usize - 0xff0) / 4])),
        _ => None,
    }
}
