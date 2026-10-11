// SPDX-License-Identifier: Apache-2.0
//! The virtio-vsock packet header (`struct virtio_vsock_hdr`, 44 bytes,
//! little-endian) and its constants.

pub const HDR_LEN: usize = 44;
pub const TYPE_STREAM: u16 = 1;
/// The host's CID.
pub const HOST_CID: u64 = 2;

pub mod op {
    pub const REQUEST: u16 = 1;
    pub const RESPONSE: u16 = 2;
    pub const RST: u16 = 3;
    pub const SHUTDOWN: u16 = 4;
    pub const RW: u16 = 5;
    pub const CREDIT_UPDATE: u16 = 6;
    pub const CREDIT_REQUEST: u16 = 7;
}

/// SHUTDOWN flags: the sender will receive / send nothing more.
pub const SHUTDOWN_RCV: u32 = 1;
pub const SHUTDOWN_SEND: u32 = 2;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Hdr {
    pub src_cid: u64,
    pub dst_cid: u64,
    pub src_port: u32,
    pub dst_port: u32,
    pub len: u32,
    pub kind: u16,
    pub op: u16,
    pub flags: u32,
    pub buf_alloc: u32,
    pub fwd_cnt: u32,
}

impl Hdr {
    pub fn parse(b: &[u8; HDR_LEN]) -> Self {
        let u64_at = |o: usize| u64::from_le_bytes(b[o..o + 8].try_into().unwrap_or_default());
        let u32_at = |o: usize| u32::from_le_bytes(b[o..o + 4].try_into().unwrap_or_default());
        let u16_at = |o: usize| u16::from_le_bytes(b[o..o + 2].try_into().unwrap_or_default());
        Self {
            src_cid: u64_at(0),
            dst_cid: u64_at(8),
            src_port: u32_at(16),
            dst_port: u32_at(20),
            len: u32_at(24),
            kind: u16_at(28),
            op: u16_at(30),
            flags: u32_at(32),
            buf_alloc: u32_at(36),
            fwd_cnt: u32_at(40),
        }
    }

    pub fn encode(&self) -> [u8; HDR_LEN] {
        let mut b = [0u8; HDR_LEN];
        b[0..8].copy_from_slice(&self.src_cid.to_le_bytes());
        b[8..16].copy_from_slice(&self.dst_cid.to_le_bytes());
        b[16..20].copy_from_slice(&self.src_port.to_le_bytes());
        b[20..24].copy_from_slice(&self.dst_port.to_le_bytes());
        b[24..28].copy_from_slice(&self.len.to_le_bytes());
        b[28..30].copy_from_slice(&self.kind.to_le_bytes());
        b[30..32].copy_from_slice(&self.op.to_le_bytes());
        b[32..36].copy_from_slice(&self.flags.to_le_bytes());
        b[36..40].copy_from_slice(&self.buf_alloc.to_le_bytes());
        b[40..44].copy_from_slice(&self.fwd_cnt.to_le_bytes());
        b
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_at_the_spec_offsets() {
        let h = Hdr {
            src_cid: 3,
            dst_cid: HOST_CID,
            src_port: 7700,
            dst_port: 1024,
            len: 5,
            kind: TYPE_STREAM,
            op: op::RW,
            flags: 0,
            buf_alloc: 262_144,
            fwd_cnt: 9,
        };
        let b = h.encode();
        assert_eq!(&b[16..20], &7700u32.to_le_bytes());
        assert_eq!(&b[30..32], &op::RW.to_le_bytes());
        assert_eq!(Hdr::parse(&b), h);
    }
}
