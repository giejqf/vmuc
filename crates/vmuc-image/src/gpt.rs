// SPDX-License-Identifier: Apache-2.0
//! GPT parsing for raw disk images and root-partition selection.
//!
//! Reads the protective MBR (LBA 0), the primary GPT header (LBA 1) and the
//! partition entry array, verifying the signature and both CRC32s. Sectors
//! are 512 bytes, which is what virtio-blk presents to the guest.
//! The backup header is not consulted: a damaged primary header is an error.

use std::fmt;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::str::FromStr;

use crate::{Error, Result};

const SECTOR: u64 = 512;
const GPT_SIGNATURE: &[u8; 8] = b"EFI PART";
const MBR_SIGNATURE: [u8; 2] = [0x55, 0xaa];
const MBR_PROTECTIVE_TYPE: u8 = 0xee;
const MIN_HEADER_SIZE: u32 = 92;
const MIN_ENTRY_SIZE: u32 = 128;
/// Refuse to read entry arrays larger than this (the spec minimum is 16 KiB).
const MAX_ENTRY_ARRAY: u64 = 16 << 20;

/// A GUID in its on-disk (mixed-endian) byte order.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Guid([u8; 16]);

impl Guid {
    /// All-zero GUID, marking an unused partition entry.
    pub const UNUSED: Guid = Guid([0; 16]);
    /// EFI System Partition `C12A7328-F81F-11D2-BA4B-00A0C93EC93B`.
    pub const EFI_SYSTEM: Guid = Guid([
        0x28, 0x73, 0x2a, 0xc1, 0x1f, 0xf8, 0xd2, 0x11, 0xba, 0x4b, 0x00, 0xa0, 0xc9, 0x3e, 0xc9,
        0x3b,
    ]);
    /// Linux root (arm64) `B921B045-1DF0-41C3-AF44-4C6F280D3FAE`.
    pub const LINUX_ROOT_ARM64: Guid = Guid([
        0x45, 0xb0, 0x21, 0xb9, 0xf0, 0x1d, 0xc3, 0x41, 0xaf, 0x44, 0x4c, 0x6f, 0x28, 0x0d, 0x3f,
        0xae,
    ]);
    /// Linux filesystem data `0FC63DAF-8483-4772-8E79-3D69D8477DE4`.
    pub const LINUX_FILESYSTEM: Guid = Guid([
        0xaf, 0x3d, 0xc6, 0x0f, 0x83, 0x84, 0x72, 0x47, 0x8e, 0x79, 0x3d, 0x69, 0xd8, 0x47, 0x7d,
        0xe4,
    ]);

    /// Wrap on-disk bytes.
    pub const fn from_bytes(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    /// The on-disk bytes.
    pub const fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }
}

impl fmt::Display for Guid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let b = &self.0;
        write!(
            f,
            "{:02X}{:02X}{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}",
            b[3],
            b[2],
            b[1],
            b[0],
            b[5],
            b[4],
            b[7],
            b[6],
            b[8],
            b[9],
            b[10],
            b[11],
            b[12],
            b[13],
            b[14],
            b[15]
        )
    }
}

impl fmt::Debug for Guid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl FromStr for Guid {
    type Err = Error;

    /// Parse the canonical `8-4-4-4-12` hex form (any case).
    fn from_str(s: &str) -> Result<Self> {
        let bad = || Error::InvalidGpt(format!("malformed GUID {s:?}"));
        let groups: Vec<&str> = s.split('-').collect();
        if groups.len() != 5
            || [8, 4, 4, 4, 12]
                != [
                    groups[0].len(),
                    groups[1].len(),
                    groups[2].len(),
                    groups[3].len(),
                    groups[4].len(),
                ]
        {
            return Err(bad());
        }
        let mut raw = [0u8; 16];
        hex::decode_to_slice(groups.concat(), &mut raw).map_err(|_| bad())?;
        let mut b = raw;
        b[..4].reverse();
        b[4..6].reverse();
        b[6..8].reverse();
        Ok(Self(b))
    }
}

/// One GPT partition entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Partition {
    /// 1-based index, as in `/dev/vdaN`.
    pub index: u32,
    /// Partition type.
    pub type_guid: Guid,
    /// Unique partition GUID.
    pub unique_guid: Guid,
    /// First sector (inclusive).
    pub first_lba: u64,
    /// Last sector (inclusive).
    pub last_lba: u64,
    /// Attribute flags.
    pub attributes: u64,
    /// Partition name (decoded from UTF-16LE, NUL-terminated).
    pub name: String,
}

impl Partition {
    /// Size in bytes.
    pub fn size_bytes(&self) -> u64 {
        (self.last_lba - self.first_lba + 1) * SECTOR
    }
}

/// A parsed primary GPT.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Gpt {
    /// Disk GUID.
    pub disk_guid: Guid,
    /// First usable sector.
    pub first_usable_lba: u64,
    /// Last usable sector.
    pub last_usable_lba: u64,
    /// Used entries, in table order.
    pub partitions: Vec<Partition>,
}

fn le_u32(b: &[u8], off: usize) -> u32 {
    u32::from_le_bytes(b[off..off + 4].try_into().expect("4 bytes"))
}

fn le_u64(b: &[u8], off: usize) -> u64 {
    u64::from_le_bytes(b[off..off + 8].try_into().expect("8 bytes"))
}

fn guid_at(b: &[u8], off: usize) -> Guid {
    Guid(b[off..off + 16].try_into().expect("16 bytes"))
}

/// Read exactly `len` bytes at `offset`, or fewer at EOF.
fn read_at<R: Read + Seek>(r: &mut R, offset: u64, len: usize) -> std::io::Result<Vec<u8>> {
    r.seek(SeekFrom::Start(offset))?;
    let mut buf = vec![0u8; len];
    let mut done = 0;
    while done < len {
        let n = r.read(&mut buf[done..])?;
        if n == 0 {
            break;
        }
        done += n;
    }
    buf.truncate(done);
    Ok(buf)
}

impl Gpt {
    /// Read the partition table of the raw image at `path`.
    ///
    /// Returns `Ok(None)` for an unpartitioned image (no GPT signature and no
    /// MBR partition entries), meaning the whole disk is the filesystem.
    pub fn read(path: &Path) -> Result<Option<Self>> {
        Self::parse(&mut File::open(path)?)
    }

    /// Like [`Gpt::read`] on any seekable reader.
    pub fn parse<R: Read + Seek>(r: &mut R) -> Result<Option<Self>> {
        let lba0 = read_at(r, 0, SECTOR as usize)?;
        let lba1 = read_at(r, SECTOR, SECTOR as usize)?;
        if lba1.len() < SECTOR as usize {
            return Ok(None);
        }
        let has_gpt = lba1[..8] == GPT_SIGNATURE[..];
        let has_mbr = lba0[510..512] == MBR_SIGNATURE;
        let mbr_types: Vec<u8> = (0..4).map(|i| lba0[446 + i * 16 + 4]).collect();
        let protective = has_mbr && mbr_types.contains(&MBR_PROTECTIVE_TYPE);
        let mbr_partitions = has_mbr
            && mbr_types
                .iter()
                .any(|&t| t != 0 && t != MBR_PROTECTIVE_TYPE);

        if has_gpt {
            return Self::parse_header(r, &lba1).map(Some);
        }
        if protective {
            return Err(Error::InvalidGpt(
                "protective MBR present but no GPT header at LBA 1".into(),
            ));
        }
        if mbr_partitions {
            return Err(Error::Unsupported(
                "MBR-partitioned disk (only GPT or unpartitioned images are supported)".into(),
            ));
        }
        Ok(None)
    }

    fn parse_header<R: Read + Seek>(r: &mut R, lba1: &[u8]) -> Result<Self> {
        let header_size = le_u32(lba1, 12);
        if header_size < MIN_HEADER_SIZE || u64::from(header_size) > SECTOR {
            return Err(Error::InvalidGpt(format!("header size {header_size}")));
        }
        let mut header = lba1[..header_size as usize].to_vec();
        let stored_crc = le_u32(&header, 16);
        header[16..20].fill(0);
        let computed = crc32fast::hash(&header);
        if computed != stored_crc {
            return Err(Error::InvalidGpt(format!(
                "header CRC32 mismatch (stored {stored_crc:#010x}, computed {computed:#010x})"
            )));
        }
        let my_lba = le_u64(lba1, 24);
        if my_lba != 1 {
            return Err(Error::InvalidGpt(format!(
                "header claims to be at LBA {my_lba}"
            )));
        }
        let first_usable_lba = le_u64(lba1, 40);
        let last_usable_lba = le_u64(lba1, 48);
        let disk_guid = guid_at(lba1, 56);
        let entries_lba = le_u64(lba1, 72);
        let num_entries = le_u32(lba1, 80);
        let entry_size = le_u32(lba1, 84);
        let entries_crc = le_u32(lba1, 88);

        if entry_size < MIN_ENTRY_SIZE || !entry_size.is_power_of_two() {
            return Err(Error::InvalidGpt(format!(
                "partition entry size {entry_size}"
            )));
        }
        let array_len = u64::from(num_entries) * u64::from(entry_size);
        if array_len == 0 || array_len > MAX_ENTRY_ARRAY {
            return Err(Error::InvalidGpt(format!(
                "partition entry array of {array_len} bytes"
            )));
        }
        let array = read_at(r, entries_lba * SECTOR, array_len as usize)?;
        if array.len() as u64 != array_len {
            return Err(Error::InvalidGpt(
                "partition entry array is truncated".into(),
            ));
        }
        let computed = crc32fast::hash(&array);
        if computed != entries_crc {
            return Err(Error::InvalidGpt(format!(
                "partition entry CRC32 mismatch (stored {entries_crc:#010x}, computed {computed:#010x})"
            )));
        }

        let mut partitions = Vec::new();
        for (i, e) in array.chunks_exact(entry_size as usize).enumerate() {
            let type_guid = guid_at(e, 0);
            if type_guid == Guid::UNUSED {
                continue;
            }
            let first_lba = le_u64(e, 32);
            let last_lba = le_u64(e, 40);
            if last_lba < first_lba {
                return Err(Error::InvalidGpt(format!(
                    "partition {} ends ({last_lba}) before it starts ({first_lba})",
                    i + 1
                )));
            }
            let utf16: Vec<u16> = e[56..128]
                .as_chunks::<2>()
                .0
                .iter()
                .map(|c| u16::from_le_bytes(*c))
                .take_while(|&c| c != 0)
                .collect();
            partitions.push(Partition {
                index: i as u32 + 1,
                type_guid,
                unique_guid: guid_at(e, 16),
                first_lba,
                last_lba,
                attributes: le_u64(e, 48),
                name: String::from_utf16_lossy(&utf16),
            });
        }
        Ok(Self {
            disk_guid,
            first_usable_lba,
            last_usable_lba,
            partitions,
        })
    }

    /// The 1-based index of the partition to boot as `root=/dev/vdaN`.
    ///
    /// Prefers the arm64 Linux root type; otherwise the largest partition of
    /// the generic Linux filesystem type; otherwise [`Error::NoRootPartition`].
    pub fn find_root_partition(&self) -> Result<u32> {
        if let Some(p) = self
            .partitions
            .iter()
            .find(|p| p.type_guid == Guid::LINUX_ROOT_ARM64)
        {
            return Ok(p.index);
        }
        self.partitions
            .iter()
            .filter(|p| p.type_guid == Guid::LINUX_FILESYSTEM)
            .max_by_key(|p| (p.size_bytes(), std::cmp::Reverse(p.index)))
            .map(|p| p.index)
            .ok_or(Error::NoRootPartition)
    }
}

/// The partition number for `root=/dev/vdaN` of the raw image at `path`, or
/// `None` when the image is unpartitioned and `/dev/vda` itself is the root.
pub fn root_partition_index(path: &Path) -> Result<Option<u32>> {
    Gpt::read(path)?
        .map(|g| g.find_root_partition())
        .transpose()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Write};
    use std::process::Command;

    #[test]
    fn guid_round_trip() {
        for (g, s) in [
            (Guid::EFI_SYSTEM, "C12A7328-F81F-11D2-BA4B-00A0C93EC93B"),
            (
                Guid::LINUX_ROOT_ARM64,
                "B921B045-1DF0-41C3-AF44-4C6F280D3FAE",
            ),
            (
                Guid::LINUX_FILESYSTEM,
                "0FC63DAF-8483-4772-8E79-3D69D8477DE4",
            ),
        ] {
            assert_eq!(g.to_string(), s);
            assert_eq!(s.parse::<Guid>().unwrap(), g);
            assert_eq!(s.to_lowercase().parse::<Guid>().unwrap(), g);
        }
        assert!("not-a-guid".parse::<Guid>().is_err());
        assert!(
            "C12A7328-F81F-11D2-BA4B-00A0C93EC93"
                .parse::<Guid>()
                .is_err()
        );
    }

    /// Partition spec for the in-memory builder: (type, first, last, name).
    type Spec<'a> = (Guid, u64, u64, &'a str);

    /// Build a 64 MiB image (in memory) with a protective MBR and primary GPT.
    fn build_image(parts: &[Spec<'_>]) -> Vec<u8> {
        let sectors = 64 * 1024 * 1024 / SECTOR;
        let mut img = vec![0u8; (sectors * SECTOR) as usize];
        // Protective MBR.
        img[446 + 4] = MBR_PROTECTIVE_TYPE;
        img[446 + 8..446 + 12].copy_from_slice(&1u32.to_le_bytes());
        img[446 + 12..446 + 16].copy_from_slice(&((sectors - 1) as u32).to_le_bytes());
        img[510..512].copy_from_slice(&MBR_SIGNATURE);
        // Entry array at LBA 2, 128 entries of 128 bytes.
        let mut array = vec![0u8; 128 * 128];
        for (i, (ty, first, last, name)) in parts.iter().enumerate() {
            let e = &mut array[i * 128..(i + 1) * 128];
            e[..16].copy_from_slice(ty.as_bytes());
            e[16..32].copy_from_slice(&[i as u8 + 1; 16]);
            e[32..40].copy_from_slice(&first.to_le_bytes());
            e[40..48].copy_from_slice(&last.to_le_bytes());
            for (j, u) in name.encode_utf16().enumerate() {
                e[56 + j * 2..58 + j * 2].copy_from_slice(&u.to_le_bytes());
            }
        }
        img[1024..1024 + array.len()].copy_from_slice(&array);
        // Header at LBA 1.
        let mut h = vec![0u8; 92];
        h[..8].copy_from_slice(GPT_SIGNATURE);
        h[8..12].copy_from_slice(&0x0001_0000u32.to_le_bytes());
        h[12..16].copy_from_slice(&92u32.to_le_bytes());
        h[24..32].copy_from_slice(&1u64.to_le_bytes());
        h[32..40].copy_from_slice(&(sectors - 1).to_le_bytes());
        h[40..48].copy_from_slice(&34u64.to_le_bytes());
        h[48..56].copy_from_slice(&(sectors - 34).to_le_bytes());
        h[56..72].copy_from_slice(&[0xdd; 16]);
        h[72..80].copy_from_slice(&2u64.to_le_bytes());
        h[80..84].copy_from_slice(&128u32.to_le_bytes());
        h[84..88].copy_from_slice(&128u32.to_le_bytes());
        h[88..92].copy_from_slice(&crc32fast::hash(&array).to_le_bytes());
        let crc = crc32fast::hash(&h);
        h[16..20].copy_from_slice(&crc.to_le_bytes());
        img[512..512 + 92].copy_from_slice(&h);
        img
    }

    #[test]
    fn parses_synthetic_table() {
        let img = build_image(&[
            (Guid::EFI_SYSTEM, 2048, 22527, "EFI System"),
            (Guid::LINUX_FILESYSTEM, 22528, 131_038, "root ü"),
        ]);
        let gpt = Gpt::parse(&mut Cursor::new(&img)).unwrap().unwrap();
        assert_eq!(gpt.disk_guid, Guid([0xdd; 16]));
        assert_eq!(gpt.first_usable_lba, 34);
        assert_eq!(gpt.partitions.len(), 2);
        let p = &gpt.partitions[1];
        assert_eq!(p.index, 2);
        assert_eq!(p.name, "root ü");
        assert_eq!(p.type_guid, Guid::LINUX_FILESYSTEM);
        assert_eq!(p.unique_guid, Guid([2; 16]));
        assert_eq!((p.first_lba, p.last_lba), (22528, 131_038));
        assert_eq!(p.size_bytes(), (131_038 - 22528 + 1) * 512);
        assert_eq!(gpt.find_root_partition().unwrap(), 2);
    }

    #[test]
    fn root_selection_rules() {
        // arm64 root wins even when a bigger generic partition exists.
        let img = build_image(&[
            (Guid::LINUX_FILESYSTEM, 2048, 100_000, "big"),
            (Guid::LINUX_ROOT_ARM64, 100_001, 110_000, "root"),
        ]);
        assert_eq!(
            Gpt::parse(&mut Cursor::new(&img))
                .unwrap()
                .unwrap()
                .find_root_partition()
                .unwrap(),
            2
        );
        // Largest generic Linux partition, skipping an unused slot.
        let img = build_image(&[
            (Guid::EFI_SYSTEM, 2048, 4095, "esp"),
            (Guid::UNUSED, 0, 0, ""),
            (Guid::LINUX_FILESYSTEM, 4096, 8191, "small"),
            (Guid::LINUX_FILESYSTEM, 8192, 100_000, "large"),
        ]);
        let gpt = Gpt::parse(&mut Cursor::new(&img)).unwrap().unwrap();
        assert_eq!(
            gpt.partitions.iter().map(|p| p.index).collect::<Vec<_>>(),
            [1, 3, 4]
        );
        assert_eq!(gpt.find_root_partition().unwrap(), 4);
        // Nothing Linux-like.
        let img = build_image(&[(Guid::EFI_SYSTEM, 2048, 4095, "esp")]);
        assert!(matches!(
            Gpt::parse(&mut Cursor::new(&img))
                .unwrap()
                .unwrap()
                .find_root_partition(),
            Err(Error::NoRootPartition)
        ));
    }

    #[test]
    fn corruption_is_detected() {
        let good = build_image(&[(Guid::LINUX_FILESYSTEM, 2048, 4095, "r")]);
        let mut bad = good.clone();
        bad[512 + 60] ^= 1; // disk GUID byte: header CRC fails
        let err = Gpt::parse(&mut Cursor::new(&bad)).unwrap_err();
        assert!(
            matches!(err, Error::InvalidGpt(ref m) if m.contains("header CRC32")),
            "{err}"
        );

        let mut bad = good.clone();
        bad[1024 + 32] ^= 1; // first_lba of entry 1: entry CRC fails
        let err = Gpt::parse(&mut Cursor::new(&bad)).unwrap_err();
        assert!(
            matches!(err, Error::InvalidGpt(ref m) if m.contains("entry CRC32")),
            "{err}"
        );

        let mut bad = good.clone();
        bad[512..520].fill(0); // no GPT signature but a protective MBR
        let err = Gpt::parse(&mut Cursor::new(&bad)).unwrap_err();
        assert!(matches!(err, Error::InvalidGpt(_)), "{err}");

        let mut truncated = good;
        truncated.truncate(1024 + 100);
        assert!(Gpt::parse(&mut Cursor::new(&truncated)).is_err());
    }

    #[test]
    fn unpartitioned_and_mbr_images() {
        let zeros = vec![0u8; 1 << 20];
        assert_eq!(Gpt::parse(&mut Cursor::new(&zeros)).unwrap(), None);
        assert_eq!(Gpt::parse(&mut Cursor::new(&zeros[..100])).unwrap(), None);
        // A FAT-style boot sector: 55AA signature, no partition entries.
        let mut fat = vec![0u8; 1 << 20];
        fat[510..512].copy_from_slice(&MBR_SIGNATURE);
        assert_eq!(Gpt::parse(&mut Cursor::new(&fat)).unwrap(), None);
        // A real MBR partition table.
        let mut mbr = fat;
        mbr[446 + 4] = 0x83;
        assert!(matches!(
            Gpt::parse(&mut Cursor::new(&mbr)),
            Err(Error::Unsupported(_))
        ));

        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("plain.img");
        std::fs::write(&p, &zeros).unwrap();
        assert_eq!(root_partition_index(&p).unwrap(), None);
    }

    #[test]
    fn sfdisk_built_image() {
        if Command::new("sfdisk").arg("--version").output().is_err() {
            eprintln!("skipping: sfdisk not installed");
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("disk.img");
        crate::sparse::create_sparse(&p, 64 << 20).unwrap();
        let script = "label: gpt\nunit: sectors\n\
            start=2048, size=20480, type=C12A7328-F81F-11D2-BA4B-00A0C93EC93B, name=\"EFI System\"\n\
            start=22528, size=4096, type=0FC63DAF-8483-4772-8E79-3D69D8477DE4, name=\"small\"\n\
            start=26624, type=0FC63DAF-8483-4772-8E79-3D69D8477DE4, name=\"cloudimg-rootfs\"\n";
        let mut child = Command::new("sfdisk")
            .args(["--quiet", "--no-reread", "--no-tell-kernel"])
            .arg(&p)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(script.as_bytes())
            .unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );

        let gpt = Gpt::read(&p).unwrap().expect("partitioned");
        let names: Vec<&str> = gpt.partitions.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["EFI System", "small", "cloudimg-rootfs"]);
        assert_eq!(gpt.partitions[0].type_guid, Guid::EFI_SYSTEM);
        assert_eq!(gpt.partitions[0].first_lba, 2048);
        assert_eq!(gpt.partitions[0].last_lba, 22527);
        assert_eq!(gpt.partitions[2].first_lba, 26624);
        assert_eq!(gpt.find_root_partition().unwrap(), 3);
        assert_eq!(root_partition_index(&p).unwrap(), Some(3));
    }
}
