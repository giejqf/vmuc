// SPDX-License-Identifier: Apache-2.0
//! Checksum and segmentation offload, passed through to a peer that
//! speaks the same 12-byte `virtio_net_hdr` (vmnet on macOS 15.4 and
//! later): the guest hands over 64 KiB TCP segments with partial
//! checksums and receives them the same way, so neither side segments or
//! checksums per 1500-byte frame.
//!
//! The peer does not know what the driver accepted, so frames coming in
//! are fitted to it here: a checksum it cannot finish is finished, a GSO
//! frame it cannot take is dropped. (Linux accepts everything offered.)

use super::HDR_LEN;

pub const F_CSUM: u64 = 1 << 0;
pub const F_GUEST_CSUM: u64 = 1 << 1;
pub const F_GUEST_TSO4: u64 = 1 << 7;
pub const F_GUEST_TSO6: u64 = 1 << 8;
pub const F_HOST_TSO4: u64 = 1 << 11;
pub const F_HOST_TSO6: u64 = 1 << 12;

/// Offered when the peer speaks virtio headers. Not the ECN variants: a
/// guest without HOST_ECN segments ECN-marked bursts itself, so nothing
/// reaches the peer in a form it might refuse.
pub const OFFERED: u64 =
    F_CSUM | F_GUEST_CSUM | F_GUEST_TSO4 | F_GUEST_TSO6 | F_HOST_TSO4 | F_HOST_TSO6;

const NEEDS_CSUM: u8 = 1;
const GSO_NONE: u8 = 0;
const GSO_TCPV4: u8 = 1;
const GSO_TCPV6: u8 = 4;
const GSO_ECN: u8 = 0x80;

/// What to do with a frame from the peer.
#[derive(Debug, PartialEq, Eq)]
pub enum Fit {
    Deliver,
    Drop(&'static str),
}

/// Fits `pkt` (header, then the Ethernet frame) to the driver's
/// `features`, finishing its checksum if the driver cannot.
pub fn fit_for_guest(pkt: &mut [u8], features: u64) -> Fit {
    if pkt.len() < HDR_LEN {
        return Fit::Drop("shorter than the virtio-net header");
    }
    let accepted = match pkt[1] & !GSO_ECN {
        GSO_NONE => true,
        GSO_TCPV4 => features & F_GUEST_TSO4 != 0,
        GSO_TCPV6 => features & F_GUEST_TSO6 != 0,
        _ => false,
    };
    if !accepted {
        return Fit::Drop("a GSO type the driver did not accept");
    }
    if pkt[0] & NEEDS_CSUM != 0 && features & F_GUEST_CSUM == 0 && !complete_checksum(pkt) {
        return Fit::Drop("a partial checksum outside the frame");
    }
    Fit::Deliver
}

/// Finishes a partial checksum as Linux's `skb_checksum_help` does: the
/// ones' complement sum from `csum_start` to the end (the field already
/// holds the pseudo-header sum), folded, stored at `csum_offset`.
fn complete_checksum(pkt: &mut [u8]) -> bool {
    let start = HDR_LEN + usize::from(u16::from_le_bytes([pkt[6], pkt[7]]));
    let field = start + usize::from(u16::from_le_bytes([pkt[8], pkt[9]]));
    if field + 2 > pkt.len() {
        return false;
    }
    let mut sum: u64 = pkt[start..]
        .chunks(2)
        .map(|c| u64::from(u16::from_be_bytes([c[0], *c.get(1).unwrap_or(&0)])))
        .sum();
    while sum >> 16 != 0 {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    // A computed zero goes out as 0xffff (UDP's "no checksum" is zero).
    let csum = match !(sum as u16) {
        0 => 0xffff,
        c => c,
    };
    pkt[field..field + 2].copy_from_slice(&csum.to_be_bytes());
    pkt[0] &= !NEEDS_CSUM;
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A virtio header, then an Ethernet + IPv4 + UDP frame carrying
    /// `payload`, with the UDP checksum left partial (pseudo-header only).
    fn udp_needing_csum(payload: &[u8]) -> Vec<u8> {
        let (src, dst) = ([192, 168, 2, 3], [192, 168, 2, 1]);
        let udp_len = 8 + payload.len() as u16;
        let mut p = vec![0u8; HDR_LEN];
        p[0] = NEEDS_CSUM;
        p[6..8].copy_from_slice(&(14u16 + 20).to_le_bytes()); // csum_start
        p[8..10].copy_from_slice(&6u16.to_le_bytes()); // csum_offset
        p.extend_from_slice(&[0; 12]);
        p.extend_from_slice(&[0x08, 0x00]);
        p.extend_from_slice(&[0x45, 0, 0, 0, 0, 0, 0, 0, 64, 17, 0, 0]);
        p.extend_from_slice(&src);
        p.extend_from_slice(&dst);
        p.extend_from_slice(&5353u16.to_be_bytes());
        p.extend_from_slice(&53u16.to_be_bytes());
        p.extend_from_slice(&udp_len.to_be_bytes());
        // Partial checksum: the folded pseudo-header sum, not inverted.
        let pseudo = pseudo_sum(src, dst, 17, udp_len);
        p.extend_from_slice(&pseudo.to_be_bytes());
        p.extend_from_slice(payload);
        p
    }

    fn pseudo_sum(src: [u8; 4], dst: [u8; 4], proto: u8, len: u16) -> u16 {
        let words = [
            u16::from_be_bytes([src[0], src[1]]),
            u16::from_be_bytes([src[2], src[3]]),
            u16::from_be_bytes([dst[0], dst[1]]),
            u16::from_be_bytes([dst[2], dst[3]]),
            u16::from(proto),
            len,
        ];
        let mut s: u32 = words.iter().map(|&w| u32::from(w)).sum();
        while s >> 16 != 0 {
            s = (s & 0xffff) + (s >> 16);
        }
        s as u16
    }

    /// The whole UDP datagram plus pseudo-header sums to 0xffff when the
    /// checksum is right.
    fn verifies(p: &[u8]) -> bool {
        let udp = &p[HDR_LEN + 34..];
        let src = p[HDR_LEN + 26..HDR_LEN + 30].try_into().unwrap();
        let dst = p[HDR_LEN + 30..HDR_LEN + 34].try_into().unwrap();
        let mut s = u32::from(pseudo_sum(src, dst, 17, udp.len() as u16));
        for c in udp.chunks(2) {
            s += u32::from(u16::from_be_bytes([c[0], *c.get(1).unwrap_or(&0)]));
        }
        while s >> 16 != 0 {
            s = (s & 0xffff) + (s >> 16);
        }
        s == 0xffff
    }

    #[test]
    fn checksums_are_finished_only_for_drivers_that_cannot() {
        for payload in [&b"odd length"[..], b"even!!", b""] {
            let mut p = udp_needing_csum(payload);
            assert!(!verifies(&p));
            assert_eq!(fit_for_guest(&mut p, F_GUEST_CSUM), Fit::Deliver);
            assert_eq!(p[0], NEEDS_CSUM, "left for the guest");
            assert_eq!(fit_for_guest(&mut p, 0), Fit::Deliver);
            assert_eq!(p[0], 0);
            assert!(verifies(&p), "payload {payload:?}");
        }
    }

    #[test]
    fn gso_frames_need_the_matching_feature() {
        let mut p = vec![0u8; HDR_LEN + 60];
        for (gso, feature) in [(GSO_TCPV4, F_GUEST_TSO4), (GSO_TCPV6, F_GUEST_TSO6)] {
            p[1] = gso;
            assert!(matches!(fit_for_guest(&mut p, 0), Fit::Drop(_)));
            assert_eq!(fit_for_guest(&mut p, feature), Fit::Deliver);
            p[1] = gso | GSO_ECN;
            assert_eq!(fit_for_guest(&mut p, feature), Fit::Deliver);
        }
        p[1] = 3; // UDP fragmentation offload: never offered
        assert!(matches!(fit_for_guest(&mut p, OFFERED), Fit::Drop(_)));
        assert!(matches!(
            fit_for_guest(&mut [0u8; 4], OFFERED),
            Fit::Drop(_)
        ));
    }

    #[test]
    fn a_checksum_field_past_the_end_is_dropped() {
        let mut p = vec![0u8; HDR_LEN + 20];
        p[0] = NEEDS_CSUM;
        p[6..8].copy_from_slice(&18u16.to_le_bytes());
        p[8..10].copy_from_slice(&6u16.to_le_bytes());
        assert!(matches!(fit_for_guest(&mut p, 0), Fit::Drop(_)));
    }
}
