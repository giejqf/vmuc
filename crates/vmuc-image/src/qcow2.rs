// SPDX-License-Identifier: Apache-2.0
//! Read-only qcow2 reader and conversion to a sparse raw file.
//!
//! Supports version 2 and 3 headers, cluster sizes from 512 bytes to 2 MiB,
//! standard, zero (`QCOW_OFLAG_ZERO`), unallocated and zlib-compressed
//! clusters. Backing files, encryption, external data files, extended L2
//! entries and non-zlib compression are rejected with
//! [`Error::Unsupported`]; structurally broken images produce
//! [`Error::InvalidImage`]. Refcount tables and snapshots are ignored, which
//! is fine for reading the active layer.
//!
//! Layout reference: `docs/interop/qcow2.txt` in the QEMU tree.

use std::fs::File;
use std::io::Read;
use std::os::unix::fs::FileExt;
use std::path::Path;

use flate2::read::DeflateDecoder;

use crate::sparse::{ZERO_RUN, create_sparse};
use crate::{Error, Result};

const MAGIC: [u8; 4] = *b"QFI\xfb";
const V2_HEADER_LEN: usize = 72;
const V3_HEADER_LEN: usize = 104;
const MIN_CLUSTER_BITS: u32 = 9;
const MAX_CLUSTER_BITS: u32 = 21;
/// Same cap as QEMU: the L1 table never exceeds 32 MiB.
const MAX_L1_BYTES: u64 = 32 << 20;

const OFFSET_MASK: u64 = 0x00ff_ffff_ffff_fe00;
const OFLAG_COMPRESSED: u64 = 1 << 62;
const OFLAG_ZERO: u64 = 1;

const INCOMPAT_DIRTY: u64 = 1 << 0;
const INCOMPAT_CORRUPT: u64 = 1 << 1;
const INCOMPAT_DATA_FILE: u64 = 1 << 2;
const INCOMPAT_COMPRESSION_TYPE: u64 = 1 << 3;
const INCOMPAT_EXTENDED_L2: u64 = 1 << 4;
const INCOMPAT_KNOWN: u64 = INCOMPAT_DIRTY
    | INCOMPAT_CORRUPT
    | INCOMPAT_DATA_FILE
    | INCOMPAT_COMPRESSION_TYPE
    | INCOMPAT_EXTENDED_L2;

/// The parsed, validated qcow2 header.
#[derive(Debug, Clone)]
pub struct Header {
    /// 2 or 3.
    pub version: u32,
    /// `log2(cluster size)`, 9..=21.
    pub cluster_bits: u32,
    /// Virtual disk size in bytes.
    pub size: u64,
    /// Number of L1 entries.
    pub l1_size: u32,
    /// File offset of the L1 table.
    pub l1_table_offset: u64,
    /// `incompatible_features` (0 for version 2).
    pub incompatible_features: u64,
    /// Compression type byte (0 = zlib).
    pub compression_type: u8,
}

fn be_u32(b: &[u8], off: usize) -> u32 {
    u32::from_be_bytes(b[off..off + 4].try_into().expect("4 bytes"))
}

fn be_u64(b: &[u8], off: usize) -> u64 {
    u64::from_be_bytes(b[off..off + 8].try_into().expect("8 bytes"))
}

impl Header {
    /// Parse and validate the first bytes of an image.
    pub fn parse(buf: &[u8]) -> Result<Self> {
        if buf.len() < V2_HEADER_LEN || buf[..4] != MAGIC {
            return Err(Error::InvalidImage("not a qcow2 image (bad magic)".into()));
        }
        let version = be_u32(buf, 4);
        if version != 2 && version != 3 {
            return Err(Error::Unsupported(format!("qcow2 version {version}")));
        }
        let backing_file_offset = be_u64(buf, 8);
        let cluster_bits = be_u32(buf, 20);
        let size = be_u64(buf, 24);
        let crypt_method = be_u32(buf, 32);
        let l1_size = be_u32(buf, 36);
        let l1_table_offset = be_u64(buf, 40);

        if backing_file_offset != 0 {
            return Err(Error::Unsupported(
                "qcow2 backing files (flatten with the image's own tooling first)".into(),
            ));
        }
        if crypt_method != 0 {
            return Err(Error::Unsupported("qcow2 encryption".into()));
        }
        if !(MIN_CLUSTER_BITS..=MAX_CLUSTER_BITS).contains(&cluster_bits) {
            return Err(Error::InvalidImage(format!(
                "cluster_bits {cluster_bits} outside {MIN_CLUSTER_BITS}..={MAX_CLUSTER_BITS}"
            )));
        }

        let mut incompatible_features = 0;
        let mut compression_type = 0;
        if version == 3 {
            if buf.len() < V3_HEADER_LEN {
                return Err(Error::InvalidImage("truncated v3 header".into()));
            }
            incompatible_features = be_u64(buf, 72);
            let header_length = be_u32(buf, 100) as usize;
            if header_length < V3_HEADER_LEN {
                return Err(Error::InvalidImage(format!(
                    "header_length {header_length} < {V3_HEADER_LEN}"
                )));
            }
            if incompatible_features & INCOMPAT_COMPRESSION_TYPE != 0 {
                compression_type = *buf.get(V3_HEADER_LEN).ok_or_else(|| {
                    Error::InvalidImage("compression_type flag set but header too short".into())
                })?;
            }
        }
        if incompatible_features & INCOMPAT_DATA_FILE != 0 {
            return Err(Error::Unsupported("qcow2 external data file".into()));
        }
        if incompatible_features & INCOMPAT_EXTENDED_L2 != 0 {
            return Err(Error::Unsupported("qcow2 extended L2 entries".into()));
        }
        if incompatible_features & !INCOMPAT_KNOWN != 0 {
            return Err(Error::Unsupported(format!(
                "qcow2 incompatible feature bits {:#x}",
                incompatible_features & !INCOMPAT_KNOWN
            )));
        }
        if incompatible_features & INCOMPAT_CORRUPT != 0 {
            return Err(Error::InvalidImage("image is marked corrupt".into()));
        }
        if compression_type != 0 {
            return Err(Error::Unsupported(format!(
                "qcow2 compression type {compression_type} (only zlib/deflate is supported)"
            )));
        }

        let cluster_size = 1u64 << cluster_bits;
        if !l1_table_offset.is_multiple_of(cluster_size) {
            return Err(Error::InvalidImage(
                "L1 table offset is not cluster aligned".into(),
            ));
        }
        if u64::from(l1_size) * 8 > MAX_L1_BYTES {
            return Err(Error::InvalidImage(format!(
                "L1 table too large ({l1_size} entries)"
            )));
        }
        let l2_entries = cluster_size / 8;
        let needed_l1 = size.div_ceil(cluster_size * l2_entries);
        if u64::from(l1_size) < needed_l1 {
            return Err(Error::InvalidImage(format!(
                "L1 table has {l1_size} entries, {needed_l1} needed for {size} bytes"
            )));
        }

        Ok(Self {
            version,
            cluster_bits,
            size,
            l1_size,
            l1_table_offset,
            incompatible_features,
            compression_type,
        })
    }

    /// Cluster size in bytes.
    pub fn cluster_size(&self) -> u64 {
        1 << self.cluster_bits
    }
}

/// How one guest cluster is stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cluster {
    /// Never written; reads as zeros.
    Unallocated,
    /// Explicitly zero (`QCOW_OFLAG_ZERO`); reads as zeros.
    Zero,
    /// Stored uncompressed at this file offset.
    Standard {
        /// File offset of the cluster data.
        host_offset: u64,
    },
    /// Stored as a raw deflate stream.
    Compressed {
        /// File offset of the compressed stream.
        host_offset: u64,
        /// Length in bytes of the compressed data (rounded up to the sector).
        size: u32,
    },
}

/// A read-only qcow2 image.
pub struct Qcow2 {
    file: File,
    file_len: u64,
    header: Header,
    l1: Vec<u64>,
    /// The most recently loaded L2 table and its L1 index.
    l2_cache: Option<(usize, Vec<u64>)>,
}

impl std::fmt::Debug for Qcow2 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Qcow2")
            .field("header", &self.header)
            .finish_non_exhaustive()
    }
}

impl Qcow2 {
    /// Open and validate `path`.
    pub fn open(path: &Path) -> Result<Self> {
        let file = File::open(path)?;
        let file_len = file.metadata()?.len();
        let mut head = vec![0u8; V3_HEADER_LEN + 1];
        let n = read_up_to(&file, &mut head, 0)?;
        let header = Header::parse(&head[..n])?;

        let l1_bytes = u64::from(header.l1_size) * 8;
        if header.l1_table_offset + l1_bytes > file_len {
            return Err(Error::InvalidImage(
                "L1 table lies beyond end of file".into(),
            ));
        }
        let mut raw = vec![0u8; l1_bytes as usize];
        file.read_exact_at(&mut raw, header.l1_table_offset)?;
        let l1 = raw
            .as_chunks::<8>()
            .0
            .iter()
            .map(|c| u64::from_be_bytes(*c))
            .collect();
        Ok(Self {
            file,
            file_len,
            header,
            l1,
            l2_cache: None,
        })
    }

    /// The validated header.
    pub fn header(&self) -> &Header {
        &self.header
    }

    /// Virtual disk size in bytes.
    pub fn virtual_size(&self) -> u64 {
        self.header.size
    }

    /// Cluster size in bytes.
    pub fn cluster_size(&self) -> u64 {
        self.header.cluster_size()
    }

    /// Number of clusters covering the virtual disk.
    pub fn cluster_count(&self) -> u64 {
        self.header.size.div_ceil(self.cluster_size())
    }

    fn l2_entries(&self) -> u64 {
        self.cluster_size() / 8
    }

    fn check_range(&self, offset: u64, len: u64, what: &str) -> Result<()> {
        if offset
            .checked_add(len)
            .is_none_or(|end| end > self.file_len)
        {
            return Err(Error::InvalidImage(format!(
                "{what} at {offset:#x}+{len:#x} lies beyond end of file ({:#x})",
                self.file_len
            )));
        }
        Ok(())
    }

    /// The L2 entry for guest cluster `index`, or `None` when its L2 table
    /// is unallocated.
    fn l2_entry(&mut self, index: u64) -> Result<Option<u64>> {
        let l1_index = (index / self.l2_entries()) as usize;
        let l2_index = (index % self.l2_entries()) as usize;
        let Some(&l1_entry) = self.l1.get(l1_index) else {
            return Err(Error::InvalidImage(format!(
                "cluster {index} beyond the L1 table"
            )));
        };
        let l2_offset = l1_entry & OFFSET_MASK;
        if l2_offset == 0 {
            return Ok(None);
        }
        if !matches!(&self.l2_cache, Some((i, _)) if *i == l1_index) {
            if !l2_offset.is_multiple_of(self.cluster_size()) {
                return Err(Error::InvalidImage(format!(
                    "L2 table {l1_index} at {l2_offset:#x} is not cluster aligned"
                )));
            }
            self.check_range(l2_offset, self.cluster_size(), "L2 table")?;
            let mut raw = vec![0u8; self.cluster_size() as usize];
            self.file.read_exact_at(&mut raw, l2_offset)?;
            let table = raw
                .as_chunks::<8>()
                .0
                .iter()
                .map(|c| u64::from_be_bytes(*c))
                .collect();
            self.l2_cache = Some((l1_index, table));
        }
        let (_, table) = self.l2_cache.as_ref().expect("just loaded");
        Ok(Some(table[l2_index]))
    }

    /// Decode the L2 entry for guest cluster `index`.
    fn cluster_by_index(&mut self, index: u64) -> Result<Cluster> {
        let Some(entry) = self.l2_entry(index)? else {
            return Ok(Cluster::Unallocated);
        };
        let cluster_bits = self.header.cluster_bits;
        if entry & OFLAG_COMPRESSED != 0 {
            let csize_shift = 62 - (cluster_bits - 8);
            let host_offset = entry & ((1u64 << csize_shift) - 1);
            let csize_mask = (1u64 << (cluster_bits - 8)) - 1;
            let nb_csectors = ((entry >> csize_shift) & csize_mask) + 1;
            let size = nb_csectors * 512 - (host_offset & 511);
            self.check_range(host_offset, size, "compressed cluster")?;
            return Ok(Cluster::Compressed {
                host_offset,
                size: size as u32,
            });
        }
        if entry & OFLAG_ZERO != 0 {
            if self.header.version < 3 {
                return Err(Error::InvalidImage(format!(
                    "zero flag set on cluster {index} in a version 2 image"
                )));
            }
            return Ok(Cluster::Zero);
        }
        let host_offset = entry & OFFSET_MASK;
        if host_offset == 0 {
            return Ok(Cluster::Unallocated);
        }
        if !host_offset.is_multiple_of(self.cluster_size()) {
            return Err(Error::InvalidImage(format!(
                "cluster {index} at {host_offset:#x} is not cluster aligned"
            )));
        }
        self.check_range(host_offset, self.cluster_size(), "cluster")?;
        Ok(Cluster::Standard { host_offset })
    }

    /// How the cluster containing guest byte `offset` is stored.
    pub fn cluster_at(&mut self, offset: u64) -> Result<Cluster> {
        if offset >= self.header.size {
            return Err(Error::InvalidImage(format!(
                "offset {offset:#x} beyond virtual size {:#x}",
                self.header.size
            )));
        }
        self.cluster_by_index(offset >> self.header.cluster_bits)
    }

    /// Whether the cluster containing guest byte `offset` holds data
    /// (standard or compressed) rather than being unallocated or zero.
    pub fn cluster_allocated(&mut self, offset: u64) -> Result<bool> {
        Ok(matches!(
            self.cluster_at(offset)?,
            Cluster::Standard { .. } | Cluster::Compressed { .. }
        ))
    }

    /// Fill `out` (exactly one cluster long) with guest cluster `index`.
    fn read_cluster(&mut self, index: u64, out: &mut [u8]) -> Result<Cluster> {
        debug_assert_eq!(out.len() as u64, self.cluster_size());
        let cluster = self.cluster_by_index(index)?;
        match cluster {
            Cluster::Unallocated | Cluster::Zero => out.fill(0),
            Cluster::Standard { host_offset } => self.file.read_exact_at(out, host_offset)?,
            Cluster::Compressed { host_offset, size } => {
                let mut comp = vec![0u8; size as usize];
                self.file.read_exact_at(&mut comp, host_offset)?;
                DeflateDecoder::new(&comp[..])
                    .read_exact(out)
                    .map_err(|e| Error::InvalidImage(format!("compressed cluster {index}: {e}")))?;
            }
        }
        Ok(cluster)
    }

    /// Read guest bytes starting at `offset` into `buf`, like `pread`: the
    /// return value is the number of bytes read, short only at the end of
    /// the virtual disk (0 at or past the end).
    pub fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<usize> {
        if offset >= self.header.size {
            return Ok(0);
        }
        let want = (buf.len() as u64).min(self.header.size - offset) as usize;
        let cluster_size = self.cluster_size();
        let mut scratch = vec![0u8; cluster_size as usize];
        let mut done = 0usize;
        while done < want {
            let pos = offset + done as u64;
            let index = pos / cluster_size;
            let within = (pos % cluster_size) as usize;
            let n = (cluster_size as usize - within).min(want - done);
            let dst = &mut buf[done..done + n];
            match self.cluster_by_index(index)? {
                Cluster::Unallocated | Cluster::Zero => dst.fill(0),
                Cluster::Standard { host_offset } => {
                    self.file.read_exact_at(dst, host_offset + within as u64)?;
                }
                Cluster::Compressed { .. } => {
                    self.read_cluster(index, &mut scratch)?;
                    dst.copy_from_slice(&scratch[within..within + n]);
                }
            }
            done += n;
        }
        Ok(done)
    }
}

/// Read as many bytes as are available at `offset`, up to `buf.len()`.
fn read_up_to(file: &File, buf: &mut [u8], offset: u64) -> std::io::Result<usize> {
    let mut done = 0;
    while done < buf.len() {
        let n = file.read_at(&mut buf[done..], offset + done as u64)?;
        if n == 0 {
            break;
        }
        done += n;
    }
    Ok(done)
}

/// Convert the qcow2 at `src` into a sparse raw file at `dst`.
///
/// Only data that is non-zero is written: unallocated and zero clusters are
/// skipped outright, and allocated clusters are written in
/// [`ZERO_RUN`]-byte pieces with all-zero pieces left as holes, so the
/// output's allocated size tracks the input's used data even with 2 MiB
/// clusters. `progress(done, total)` is called per cluster with byte counts.
/// `dst` is created (or truncated) and sized to the virtual disk.
pub fn convert_to_raw(src: &Path, dst: &Path, mut progress: impl FnMut(u64, u64)) -> Result<()> {
    let mut image = Qcow2::open(src)?;
    let total = image.virtual_size();
    let cluster_size = image.cluster_size();
    let out = create_sparse(dst, total)?;
    let mut buf = vec![0u8; cluster_size as usize];
    progress(0, total);
    for index in 0..image.cluster_count() {
        let offset = index * cluster_size;
        let len = cluster_size.min(total - offset) as usize;
        let cluster = image.cluster_by_index(index)?;
        if matches!(
            cluster,
            Cluster::Standard { .. } | Cluster::Compressed { .. }
        ) {
            image.read_cluster(index, &mut buf)?;
            for (i, run) in buf[..len].chunks(ZERO_RUN).enumerate() {
                if run.iter().any(|&b| b != 0) {
                    out.write_all_at(run, offset + (i * ZERO_RUN) as u64)?;
                }
            }
        }
        progress(offset + len as u64, total);
    }
    out.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sparse::allocated_bytes;
    use std::fs;
    use std::path::PathBuf;
    use std::process::Command;

    fn have(tool: &str) -> bool {
        Command::new(tool)
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success())
    }

    fn run(tool: &str, args: &[&str]) {
        let out = Command::new(tool).args(args).output().unwrap();
        assert!(
            out.status.success(),
            "{tool} {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn qemu_io(img: &Path, cmds: &[&str]) {
        let mut args = vec!["-f", "qcow2"];
        for c in cmds {
            args.push("-c");
            args.push(c);
        }
        args.push(img.to_str().unwrap());
        run("qemu-io", &args);
    }

    /// Build a 16 MiB qcow2 with a mix of cluster kinds, return the path and
    /// the path to the `qemu-img convert -O raw` reference.
    fn make_image(dir: &Path, name: &str, opts: &str, compress: bool) -> (PathBuf, PathBuf) {
        let qcow = dir.join(format!("{name}.qcow2"));
        run(
            "qemu-img",
            &[
                "create",
                "-q",
                "-f",
                "qcow2",
                "-o",
                opts,
                qcow.to_str().unwrap(),
                "16M",
            ],
        );
        qemu_io(
            &qcow,
            &[
                "write -q -P 0xab 1M 64k",
                "write -q -P 0xcd 2M 1k",
                "write -q -P 0x11 3071k 2k",
                "write -q -P 0x22 5M 700k",
                "write -q -z 8M 1M",
                "write -q -P 0 10M 256k",
                "write -q -P 0x33 16380k 4k",
            ],
        );
        let qcow = if compress {
            let c = dir.join(format!("{name}.c.qcow2"));
            run(
                "qemu-img",
                &[
                    "convert",
                    "-q",
                    "-c",
                    "-f",
                    "qcow2",
                    "-O",
                    "qcow2",
                    "-o",
                    opts,
                    qcow.to_str().unwrap(),
                    c.to_str().unwrap(),
                ],
            );
            c
        } else {
            qcow
        };
        let reference = dir.join(format!("{name}.ref.raw"));
        run(
            "qemu-img",
            &[
                "convert",
                "-q",
                "-f",
                "qcow2",
                "-O",
                "raw",
                qcow.to_str().unwrap(),
                reference.to_str().unwrap(),
            ],
        );
        (qcow, reference)
    }

    fn check_image(qcow: &Path, reference: &Path, compressed: bool) {
        let expect = fs::read(reference).unwrap();
        let out = qcow.with_extension("ours.raw");
        let mut last = (0, 0);
        convert_to_raw(qcow, &out, |d, t| last = (d, t)).unwrap();
        assert_eq!(last, (expect.len() as u64, expect.len() as u64));
        let got = fs::read(&out).unwrap();
        assert!(
            got == expect,
            "{}: convert_to_raw output differs",
            qcow.display()
        );
        // Written data is about 64k+1k+2k+700k+4k; the holes must stay holes.
        let alloc = allocated_bytes(&out).unwrap();
        if cfg!(target_os = "linux") {
            assert!(alloc < 2 << 20, "{}: allocated {alloc}", qcow.display());
        }

        let mut img = Qcow2::open(qcow).unwrap();
        assert_eq!(img.virtual_size(), 16 << 20);
        // Random-ish reads crossing cluster boundaries.
        let mut buf = vec![0u8; 300 * 1024 + 7];
        for &off in &[
            0u64,
            1 << 20,
            (1 << 20) - 1,
            3_000_000,
            5 << 20,
            (8 << 20) - 3,
            16_000_000,
        ] {
            let n = img.read_at(off, &mut buf).unwrap();
            let end = (off as usize + buf.len()).min(expect.len());
            assert_eq!(n, end - off as usize);
            assert_eq!(&buf[..n], &expect[off as usize..end], "read at {off}");
        }
        assert_eq!(img.read_at(16 << 20, &mut buf).unwrap(), 0);
        let mut one = [0u8; 1];
        assert_eq!(img.read_at((16 << 20) - 1, &mut one).unwrap(), 1);
        assert_eq!(one[0], 0x33);

        assert!(img.cluster_allocated(1 << 20).unwrap());
        assert!(!img.cluster_allocated(6 << 20).unwrap());
        assert!(!img.cluster_allocated(12 << 20).unwrap());
        // `qemu-io write -z` sets the zero flag on v3 images; v2 has no
        // zero flag and `qemu-img convert -c` drops zero clusters entirely.
        let zeroed = img.cluster_at(8 << 20).unwrap();
        if img.header().version >= 3 && !compressed {
            assert_eq!(zeroed, Cluster::Zero);
        } else {
            assert!(
                matches!(zeroed, Cluster::Unallocated | Cluster::Zero),
                "{zeroed:?}"
            );
        }
        assert!(matches!(
            img.cluster_at(15 << 20),
            Err(Error::InvalidImage(_)) | Ok(_)
        ));
        assert!(img.cluster_at(16 << 20).is_err());
    }

    #[test]
    fn matches_qemu_img_across_cluster_sizes_and_compression() {
        if !have("qemu-img") || !have("qemu-io") {
            eprintln!("skipping: qemu-img/qemu-io not installed");
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        for (i, cs) in ["512", "4k", "64k", "2M"].iter().enumerate() {
            for compress in [false, true] {
                let name = format!("cs{i}{}", if compress { "c" } else { "" });
                let opts = format!("cluster_size={cs}");
                let (qcow, reference) = make_image(tmp.path(), &name, &opts, compress);
                let mut img = Qcow2::open(&qcow).unwrap();
                assert_eq!(img.header().version, 3);
                if compress {
                    assert!(matches!(
                        img.cluster_at(1 << 20).unwrap(),
                        Cluster::Compressed { .. }
                    ));
                } else {
                    assert!(matches!(
                        img.cluster_at(1 << 20).unwrap(),
                        Cluster::Standard { .. }
                    ));
                }
                check_image(&qcow, &reference, compress);
            }
        }
    }

    #[test]
    fn version_2_image() {
        if !have("qemu-img") || !have("qemu-io") {
            eprintln!("skipping: qemu-img/qemu-io not installed");
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        for compress in [false, true] {
            let name = if compress { "v2c" } else { "v2" };
            let (qcow, reference) =
                make_image(tmp.path(), name, "compat=0.10,cluster_size=64k", compress);
            let img = Qcow2::open(&qcow).unwrap();
            assert_eq!(img.header().version, 2);
            check_image(&qcow, &reference, compress);
        }
    }

    #[test]
    fn backing_file_is_rejected() {
        if !have("qemu-img") {
            eprintln!("skipping: qemu-img not installed");
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().join("base.qcow2");
        let overlay = tmp.path().join("overlay.qcow2");
        run(
            "qemu-img",
            &["create", "-q", "-f", "qcow2", base.to_str().unwrap(), "8M"],
        );
        run(
            "qemu-img",
            &[
                "create",
                "-q",
                "-f",
                "qcow2",
                "-b",
                base.to_str().unwrap(),
                "-F",
                "qcow2",
                overlay.to_str().unwrap(),
            ],
        );
        let err = Qcow2::open(&overlay).unwrap_err();
        assert!(
            matches!(err, Error::Unsupported(ref m) if m.contains("backing")),
            "{err}"
        );
        assert!(Qcow2::open(&base).is_ok());
    }

    /// A minimal v3 header with the given tweaks applied.
    fn synthetic_header(edit: impl FnOnce(&mut Vec<u8>)) -> Vec<u8> {
        let mut h = vec![0u8; 112];
        h[..4].copy_from_slice(&MAGIC);
        h[4..8].copy_from_slice(&3u32.to_be_bytes());
        h[20..24].copy_from_slice(&16u32.to_be_bytes()); // 64 KiB clusters
        h[24..32].copy_from_slice(&(1u64 << 20).to_be_bytes());
        h[36..40].copy_from_slice(&1u32.to_be_bytes());
        h[40..48].copy_from_slice(&(1u64 << 16).to_be_bytes());
        h[100..104].copy_from_slice(&104u32.to_be_bytes());
        edit(&mut h);
        h
    }

    type Case = (
        &'static str,
        Box<dyn FnOnce(&mut Vec<u8>)>,
        fn(&Error) -> bool,
    );

    #[test]
    fn header_rejections() {
        let ok = synthetic_header(|_| {});
        assert_eq!(Header::parse(&ok).unwrap().cluster_size(), 65536);

        let cases: Vec<Case> = vec![
            ("magic", Box::new(|h| h[0] = b'X'), |e| {
                matches!(e, Error::InvalidImage(_))
            }),
            (
                "version",
                Box::new(|h| h[4..8].copy_from_slice(&4u32.to_be_bytes())),
                |e| matches!(e, Error::Unsupported(_)),
            ),
            (
                "encryption",
                Box::new(|h| h[32..36].copy_from_slice(&1u32.to_be_bytes())),
                |e| matches!(e, Error::Unsupported(m) if m.contains("encryption")),
            ),
            (
                "backing",
                Box::new(|h| h[8..16].copy_from_slice(&512u64.to_be_bytes())),
                |e| matches!(e, Error::Unsupported(m) if m.contains("backing")),
            ),
            (
                "data file",
                Box::new(|h| h[72..80].copy_from_slice(&INCOMPAT_DATA_FILE.to_be_bytes())),
                |e| matches!(e, Error::Unsupported(m) if m.contains("external data")),
            ),
            (
                "extended l2",
                Box::new(|h| h[72..80].copy_from_slice(&INCOMPAT_EXTENDED_L2.to_be_bytes())),
                |e| matches!(e, Error::Unsupported(m) if m.contains("extended L2")),
            ),
            (
                "zstd",
                Box::new(|h| {
                    h[72..80].copy_from_slice(&INCOMPAT_COMPRESSION_TYPE.to_be_bytes());
                    h[104] = 1;
                }),
                |e| matches!(e, Error::Unsupported(m) if m.contains("compression type 1")),
            ),
            (
                "unknown incompat",
                Box::new(|h| h[72..80].copy_from_slice(&(1u64 << 40).to_be_bytes())),
                |e| matches!(e, Error::Unsupported(_)),
            ),
            (
                "cluster bits",
                Box::new(|h| h[20..24].copy_from_slice(&22u32.to_be_bytes())),
                |e| matches!(e, Error::InvalidImage(_)),
            ),
            (
                "l1 too small",
                Box::new(|h| h[24..32].copy_from_slice(&(1u64 << 40).to_be_bytes())),
                |e| matches!(e, Error::InvalidImage(_)),
            ),
        ];
        for (name, edit, check) in cases {
            let err = Header::parse(&synthetic_header(edit)).unwrap_err();
            assert!(check(&err), "{name}: {err}");
        }
        // zlib compression type explicitly flagged is fine.
        let zlib = synthetic_header(|h| {
            h[72..80].copy_from_slice(&INCOMPAT_COMPRESSION_TYPE.to_be_bytes());
        });
        assert_eq!(Header::parse(&zlib).unwrap().compression_type, 0);
    }

    #[test]
    fn truncated_tables_are_invalid() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("t.qcow2");
        fs::write(&p, synthetic_header(|_| {})).unwrap();
        let err = Qcow2::open(&p).unwrap_err();
        assert!(
            matches!(err, Error::InvalidImage(ref m) if m.contains("L1")),
            "{err}"
        );

        // L1 present, pointing at an L2 table beyond EOF.
        let mut img = synthetic_header(|_| {});
        img.resize(2 << 16, 0);
        img[1 << 16..(1 << 16) + 8].copy_from_slice(&(5u64 << 16).to_be_bytes());
        fs::write(&p, &img).unwrap();
        let mut q = Qcow2::open(&p).unwrap();
        assert!(matches!(q.cluster_at(0), Err(Error::InvalidImage(_))));
    }
}
