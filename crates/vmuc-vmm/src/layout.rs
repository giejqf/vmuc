// SPDX-License-Identifier: Apache-2.0
//! The guest physical map. Fixed: the FDT generator, the GIC setup and
//! the MMIO bus all read it from here.
//!
//! | IPA            | Contents                                            |
//! |----------------|-----------------------------------------------------|
//! | `0x0800_0000`  | GICv3 distributor (64 KiB)                          |
//! | `0x0900_0000`  | PL011 UART (4 KiB), SPI 1                           |
//! | `0x0901_0000`  | PL031 RTC (4 KiB), SPI 2                            |
//! | `0x0A00_0000`  | virtio-mmio, 32 slots × 4 KiB, SPI 32 + slot        |
//! | `0x0C00_0000`  | GICv3 redistributors (window of 64 MiB)             |
//! | `0x4000_0000`  | RAM                                                 |
//!
//! The redistributor window sits above the devices because the framework
//! sizes its region for every vCPU it supports (32 MiB on an M4 Pro), see
//! `docs/experiments/2026-10-11-hv-contract-m4-pro.md`.

pub const GICD_BASE: u64 = 0x0800_0000;
pub const GICD_SIZE: u64 = 0x1_0000;

pub const PL011_BASE: u64 = 0x0900_0000;
pub const PL031_BASE: u64 = 0x0901_0000;
/// Window size of each PrimeCell device.
pub const PRIMECELL_SIZE: u64 = 0x1000;

pub const VIRTIO_BASE: u64 = 0x0A00_0000;
pub const VIRTIO_SLOT_SIZE: u64 = 0x1000;
pub const VIRTIO_SLOTS: u32 = 32;

pub const GICR_BASE: u64 = 0x0C00_0000;
/// The redistributor region the framework reports must fit in this.
pub const GICR_WINDOW: u64 = 0x0400_0000;

pub const RAM_BASE: u64 = 0x4000_0000;

/// SPI numbers (FDT numbering; the GIC INTID is `32 + spi`).
pub const PL011_SPI: u32 = 1;
pub const PL031_SPI: u32 = 2;
pub const VIRTIO_SPI_BASE: u32 = 32;

/// The GIC INTID of an SPI.
pub const fn spi_intid(spi: u32) -> u32 {
    32 + spi
}

/// Guest physical base of virtio-mmio slot `slot`.
pub const fn virtio_slot_base(slot: u32) -> u64 {
    VIRTIO_BASE + slot as u64 * VIRTIO_SLOT_SIZE
}

/// SPI number of virtio-mmio slot `slot`.
pub const fn virtio_slot_spi(slot: u32) -> u32 {
    VIRTIO_SPI_BASE + slot
}

/// MPIDR_EL1 affinity of vCPU `index`: 16 CPUs per Aff1 cluster, the
/// GICv3 SGI target-list limit, as QEMU's virt machine does.
pub const fn mpidr(index: u32) -> u64 {
    (((index / 16) as u64) << 8) | (index % 16) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn regions_do_not_overlap() {
        let mut regions = [
            (GICD_BASE, GICD_SIZE),
            (PL011_BASE, PRIMECELL_SIZE),
            (PL031_BASE, PRIMECELL_SIZE),
            (VIRTIO_BASE, u64::from(VIRTIO_SLOTS) * VIRTIO_SLOT_SIZE),
            (GICR_BASE, GICR_WINDOW),
            (RAM_BASE, 1),
        ];
        regions.sort_unstable();
        for w in regions.windows(2) {
            assert!(
                w[0].0 + w[0].1 <= w[1].0,
                "{:x?} overlaps {:x?}",
                w[0],
                w[1]
            );
        }
    }

    #[test]
    fn affinity_and_interrupts() {
        assert_eq!(mpidr(0), 0);
        assert_eq!(mpidr(15), 15);
        assert_eq!(mpidr(16), 0x100);
        assert_eq!(mpidr(33), 0x201);
        assert_eq!(spi_intid(PL011_SPI), 33);
        assert_eq!(spi_intid(virtio_slot_spi(0)), 64);
        assert_eq!(virtio_slot_base(2), 0x0A00_2000);
    }
}
