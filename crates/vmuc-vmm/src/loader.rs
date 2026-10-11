// SPDX-License-Identifier: Apache-2.0
//! Direct boot: place an arm64 Linux `Image`, an optional initrd and the
//! device tree in guest RAM (Documentation/arch/arm64/booting.rst).
//!
//! The kernel goes at the start of RAM plus its `text_offset` (0 for any
//! kernel since 3.17), the initrd at `max(kernel end, min(RAM/2, 128 MiB))`
//! rounded up to 2 MiB as QEMU's virt machine does, and the device tree
//! in the last 2 MiB of RAM, a 2 MiB-aligned region of its own as the
//! boot protocol asks.

use std::ops::Range;

use vmuc_virtio::{GuestMemory, MemError};

use crate::fdt::FDT_MAX_SIZE;
use crate::layout::RAM_BASE;

const IMAGE_MAGIC: u32 = 0x644d_5241; // "ARM\x64"
const HEADER_LEN: usize = 64;
const TWO_MIB: u64 = 2 << 20;

#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    #[error("not an arm64 Linux Image (bad header magic)")]
    NotAnImage,
    #[error("{what} ends at {end:#x}, past {limit:#x}; give the VM more memory")]
    DoesNotFit {
        what: &'static str,
        end: u64,
        limit: u64,
    },
    #[error("RAM size {0:#x} must be a non-zero multiple of 2 MiB")]
    BadRamSize(u64),
    #[error(transparent)]
    Memory(#[from] MemError),
}

/// The fields of the 64-byte arm64 Image header the loader needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageHeader {
    pub text_offset: u64,
    /// Bytes the kernel occupies once running, BSS included; 0 on
    /// kernels older than 3.17 (then the file size is used).
    pub image_size: u64,
}

impl ImageHeader {
    pub fn parse(image: &[u8]) -> Result<Self, LoadError> {
        if image.len() < HEADER_LEN {
            return Err(LoadError::NotAnImage);
        }
        let u64_at = |o: usize| u64::from_le_bytes(image[o..o + 8].try_into().unwrap_or_default());
        let magic = u32::from_le_bytes(image[56..60].try_into().unwrap_or_default());
        if magic != IMAGE_MAGIC {
            return Err(LoadError::NotAnImage);
        }
        Ok(Self {
            text_offset: u64_at(8),
            image_size: u64_at(16),
        })
    }
}

/// Where everything goes, decided before anything is written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootLayout {
    /// Kernel entry point: the first byte of the loaded image.
    pub entry: u64,
    pub kernel: Range<u64>,
    pub initrd: Option<Range<u64>>,
    /// Guest physical address of the device tree, passed in X0.
    pub fdt: u64,
}

impl BootLayout {
    pub fn plan(
        ram_size: u64,
        header: ImageHeader,
        image_len: u64,
        initrd_len: Option<u64>,
    ) -> Result<Self, LoadError> {
        if ram_size == 0 || !ram_size.is_multiple_of(TWO_MIB) || ram_size < 2 * FDT_MAX_SIZE {
            return Err(LoadError::BadRamSize(ram_size));
        }
        let fdt = RAM_BASE + ram_size - FDT_MAX_SIZE;
        let entry = RAM_BASE + header.text_offset;
        let kernel = entry..entry + header.image_size.max(image_len);
        if kernel.end > fdt {
            return Err(LoadError::DoesNotFit {
                what: "kernel",
                end: kernel.end,
                limit: fdt,
            });
        }
        let initrd = initrd_len.map(|len| {
            let floor = RAM_BASE + (ram_size / 2).min(128 << 20);
            let start = kernel.end.next_multiple_of(TWO_MIB).max(floor);
            start..start + len
        });
        if let Some(r) = &initrd
            && r.end > fdt
        {
            return Err(LoadError::DoesNotFit {
                what: "initrd",
                end: r.end,
                limit: fdt,
            });
        }
        Ok(Self {
            entry,
            kernel,
            initrd,
            fdt,
        })
    }

    /// Copies the kernel and initrd into guest RAM. The device tree is
    /// written separately because it names the initrd range.
    pub fn load(
        &self,
        mem: &GuestMemory,
        image: &[u8],
        initrd: Option<&[u8]>,
    ) -> Result<(), LoadError> {
        mem.write(self.entry, image)?;
        if let (Some(range), Some(bytes)) = (&self.initrd, initrd) {
            mem.write(range.start, bytes)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(text_offset: u64, image_size: u64, len: usize) -> Vec<u8> {
        let mut v = vec![0u8; len];
        v[8..16].copy_from_slice(&text_offset.to_le_bytes());
        v[16..24].copy_from_slice(&image_size.to_le_bytes());
        v[56..60].copy_from_slice(&IMAGE_MAGIC.to_le_bytes());
        v
    }

    #[test]
    fn header_parses_and_rejects_garbage() {
        let h = ImageHeader::parse(&image(0, 0x1ee_0000, 64)).unwrap();
        assert_eq!(h.image_size, 0x1ee_0000);
        assert!(ImageHeader::parse(&[0u8; 64]).is_err());
        assert!(ImageHeader::parse(&[0u8; 10]).is_err());
    }

    #[test]
    fn layout_follows_qemu_virt_placement() {
        let h = ImageHeader {
            text_offset: 0,
            image_size: 0x1ee_0000,
        };
        let l = BootLayout::plan(1 << 30, h, 0x1e5_0000, Some(0x10_0000)).unwrap();
        assert_eq!(l.entry, RAM_BASE);
        assert_eq!(l.kernel.end, RAM_BASE + 0x1ee_0000);
        assert_eq!(
            l.initrd,
            Some(RAM_BASE + (128 << 20)..RAM_BASE + (129 << 20))
        );
        assert_eq!(l.fdt, RAM_BASE + (1 << 30) - FDT_MAX_SIZE);

        // Small RAM: the initrd floor is RAM/2, but never below the kernel.
        let l = BootLayout::plan(64 << 20, h, 0, Some(0x1000)).unwrap();
        assert_eq!(l.initrd.unwrap().start, RAM_BASE + (32 << 20));
        let big = ImageHeader {
            text_offset: 0x8_0000,
            image_size: 40 << 20,
        };
        let l = BootLayout::plan(64 << 20, big, 0, Some(0x1000)).unwrap();
        assert_eq!(l.entry, RAM_BASE + 0x8_0000);
        assert_eq!(l.initrd.unwrap().start, RAM_BASE + (42 << 20));
    }

    #[test]
    fn refuses_what_does_not_fit() {
        let h = ImageHeader {
            text_offset: 0,
            image_size: 63 << 20,
        };
        assert!(matches!(
            BootLayout::plan(64 << 20, h, 0, None),
            Err(LoadError::DoesNotFit { what: "kernel", .. })
        ));
        let h = ImageHeader {
            text_offset: 0,
            image_size: 1 << 20,
        };
        assert!(matches!(
            BootLayout::plan(64 << 20, h, 0, Some(40 << 20)),
            Err(LoadError::DoesNotFit { what: "initrd", .. })
        ));
        assert!(matches!(
            BootLayout::plan((64 << 20) + 4096, h, 0, None),
            Err(LoadError::BadRamSize(_))
        ));
    }

    #[test]
    fn load_writes_kernel_and_initrd() {
        let ram = 16 << 20;
        let mem = GuestMemory::from_vec(RAM_BASE, vec![0; ram as usize]);
        let img = image(0, 0x1000, 0x800);
        let h = ImageHeader::parse(&img).unwrap();
        let l = BootLayout::plan(ram, h, img.len() as u64, Some(4)).unwrap();
        l.load(&mem, &img, Some(b"cpio")).unwrap();
        assert_eq!(mem.read_obj::<u32>(RAM_BASE + 56).unwrap(), IMAGE_MAGIC);
        let mut b = [0u8; 4];
        mem.read(l.initrd.unwrap().start, &mut b).unwrap();
        assert_eq!(&b, b"cpio");
    }
}
