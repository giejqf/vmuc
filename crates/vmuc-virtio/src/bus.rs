// SPDX-License-Identifier: Apache-2.0
//! Devices reached through guest MMIO.

/// A device behind a window of guest physical space. The VMM calls it
/// from whichever vCPU thread took the data abort, with `offset` relative
/// to the window base and `data` 1, 2, 4 or 8 bytes long (little-endian).
pub trait BusDevice: Send + Sync {
    fn read(&self, offset: u64, data: &mut [u8]);
    fn write(&self, offset: u64, data: &[u8]);
}

/// Little-endian value of an MMIO access buffer (at most 8 bytes).
pub fn le_value(data: &[u8]) -> u64 {
    let mut b = [0u8; 8];
    let n = data.len().min(8);
    b[..n].copy_from_slice(&data[..n]);
    u64::from_le_bytes(b)
}

/// Fills an MMIO read buffer with the low bytes of `value`.
pub fn put_le(data: &mut [u8], value: u64) {
    let b = value.to_le_bytes();
    let n = data.len().min(8);
    data[..n].copy_from_slice(&b[..n]);
}

#[cfg(test)]
mod tests {
    #[test]
    fn le_helpers() {
        assert_eq!(super::le_value(&[0x78, 0x56, 0x34, 0x12]), 0x1234_5678);
        let mut d = [0u8; 2];
        super::put_le(&mut d, 0xaabb_ccdd);
        assert_eq!(d, [0xdd, 0xcc]);
    }
}
