// SPDX-License-Identifier: Apache-2.0
//! Sparse-file helpers: create a file of a given logical size without
//! allocating blocks, punch holes to give blocks back, and measure how many
//! bytes a file really occupies on disk.
//!
//! The guest disk is a raw sparse file; `discard` from the guest ends up in
//! [`SparseFileExt::punch_hole`], and `vmuc stat` reports
//! [`allocated_bytes`].
//!
//! APFS fills holes in: when it flushes a file, data written up to 16 MiB
//! past the file's allocated end is joined to it by allocated zeros, so
//! a file written front to back with gaps of up to 16 MiB ends up fully
//! allocated. A hole punched after the flush stays one
//! (docs/experiments/2026-10-13-e6-apfs-sparse-files.md), which is what
//! [`SparseFileExt::settle_holes`] does with the holes a writer left.

use std::fs::{File, OpenOptions};
use std::io::{self, Seek, SeekFrom, Write};
use std::ops::Range;
use std::os::fd::AsRawFd;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

/// Granularity at which [`SparseWriter`] decides whether a run of bytes is
/// all zero and can be left as a hole.
pub const ZERO_RUN: usize = 64 * 1024;

/// The block size holes are punched in (APFS's).
const PUNCH_ALIGN: u64 = 4096;

/// Adds `r` to sorted, disjoint `holes`, joining it to the last if they touch.
pub fn add_hole(holes: &mut Vec<Range<u64>>, r: Range<u64>) {
    if r.is_empty() {
        return;
    }
    match holes.last_mut() {
        Some(last) if last.end == r.start => last.end = r.end,
        _ => holes.push(r),
    }
}

/// Create (or truncate) `path` as a sparse file of `size` bytes.
///
/// No blocks are allocated; the file reads as zeros.
pub fn create_sparse(path: &Path, size: u64) -> io::Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)?;
    file.set_len(size)?;
    Ok(file)
}

/// Bytes actually allocated to `path` on disk (`st_blocks * 512`).
pub fn allocated_bytes(path: &Path) -> io::Result<u64> {
    Ok(std::fs::metadata(path)?.blocks() * 512)
}

/// Sparse-file operations on an open [`File`].
pub trait SparseFileExt {
    /// Deallocate `len` bytes at `offset` so they read back as zeros and no
    /// longer occupy disk blocks. The file size does not change.
    ///
    /// On Linux this is `fallocate(FALLOC_FL_PUNCH_HOLE | FALLOC_FL_KEEP_SIZE)`
    /// and partial blocks are zero-filled. On macOS it is `fcntl(F_PUNCHHOLE)`,
    /// which requires `offset` and `len` to be multiples of the filesystem
    /// block size (4 KiB on APFS); callers pass aligned ranges.
    fn punch_hole(&self, offset: u64, len: u64) -> io::Result<()>;

    /// Bytes allocated to this file on disk (`st_blocks * 512`).
    fn allocated_bytes(&self) -> io::Result<u64>;

    /// Flushes the file and makes `holes` (ranges a writer left unwritten)
    /// holes on disk: APFS fills some in when it flushes, see the module
    /// docs. Each is trimmed inward to whole blocks.
    fn settle_holes(&self, holes: &[Range<u64>]) -> io::Result<()>;
}

impl SparseFileExt for File {
    #[cfg(target_os = "linux")]
    fn punch_hole(&self, offset: u64, len: u64) -> io::Result<()> {
        if len == 0 {
            return Ok(());
        }
        let offset = libc::off_t::try_from(offset).map_err(|_| io::ErrorKind::InvalidInput)?;
        let len = libc::off_t::try_from(len).map_err(|_| io::ErrorKind::InvalidInput)?;
        // SAFETY: plain syscall on a file descriptor we own; no pointers.
        let rc = unsafe {
            libc::fallocate(
                self.as_raw_fd(),
                libc::FALLOC_FL_PUNCH_HOLE | libc::FALLOC_FL_KEEP_SIZE,
                offset,
                len,
            )
        };
        if rc == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    #[cfg(target_os = "macos")]
    fn punch_hole(&self, offset: u64, len: u64) -> io::Result<()> {
        if len == 0 {
            return Ok(());
        }
        let arg = libc::fpunchhole_t {
            fp_flags: 0,
            reserved: 0,
            fp_offset: libc::off_t::try_from(offset).map_err(|_| io::ErrorKind::InvalidInput)?,
            fp_length: libc::off_t::try_from(len).map_err(|_| io::ErrorKind::InvalidInput)?,
        };
        // SAFETY: `arg` is a fully initialised `fpunchhole_t` that outlives
        // the call; F_PUNCHHOLE only reads it.
        let rc = unsafe { libc::fcntl(self.as_raw_fd(), libc::F_PUNCHHOLE, &raw const arg) };
        if rc == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    fn punch_hole(&self, _offset: u64, _len: u64) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "hole punching is only implemented for Linux and macOS",
        ))
    }

    fn allocated_bytes(&self) -> io::Result<u64> {
        Ok(self.metadata()?.blocks() * 512)
    }

    fn settle_holes(&self, holes: &[Range<u64>]) -> io::Result<()> {
        self.sync_data()?;
        for h in holes {
            let (start, end) = (
                h.start.next_multiple_of(PUNCH_ALIGN),
                h.end / PUNCH_ALIGN * PUNCH_ALIGN,
            );
            if start < end {
                self.punch_hole(start, end - start)?;
            }
        }
        self.sync_all()
    }
}

/// A writer that leaves holes instead of writing zeros.
///
/// Bytes are buffered in [`ZERO_RUN`]-sized runs; a run that is entirely
/// zero is skipped with a seek, so the destination stays sparse wherever the
/// source was (once [`SparseFileExt::settle_holes`] has seen to the holes it
/// reports). [`finish`](Self::finish) extends the file to the full logical
/// length when the data ends in zeros.
pub struct SparseWriter<W: Write + Seek> {
    inner: W,
    buf: Vec<u8>,
    pos: u64,
    holes: Vec<Range<u64>>,
}

/// What a [`SparseWriter`] wrote.
#[derive(Debug)]
pub struct Written<W> {
    pub inner: W,
    /// Logical length.
    pub len: u64,
    /// The runs it skipped, sorted.
    pub holes: Vec<Range<u64>>,
}

impl<W: Write + Seek> SparseWriter<W> {
    /// Wrap `inner`, which must be positioned at offset 0.
    pub fn new(inner: W) -> Self {
        Self {
            inner,
            buf: Vec::with_capacity(ZERO_RUN),
            pos: 0,
            holes: Vec::new(),
        }
    }

    /// Logical bytes written so far.
    pub fn position(&self) -> u64 {
        self.pos + self.buf.len() as u64
    }

    fn flush_run(&mut self) -> io::Result<()> {
        if self.buf.is_empty() {
            return Ok(());
        }
        if self.buf.iter().all(|&b| b == 0) {
            self.inner.seek(SeekFrom::Current(self.buf.len() as i64))?;
            let len = self.buf.len() as u64;
            add_hole(&mut self.holes, self.pos..self.pos + len);
        } else {
            self.inner.write_all(&self.buf)?;
        }
        self.pos += self.buf.len() as u64;
        self.buf.clear();
        Ok(())
    }

    /// Flush the last run and return the inner writer positioned at the
    /// logical end. The caller sets the final length when the sink is a file
    /// (a trailing hole does not move EOF by itself).
    pub fn finish(mut self) -> io::Result<Written<W>> {
        self.flush_run()?;
        self.inner.flush()?;
        Ok(Written {
            inner: self.inner,
            len: self.pos,
            holes: self.holes,
        })
    }
}

impl<W: Write + Seek> Write for SparseWriter<W> {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        let room = ZERO_RUN - self.buf.len();
        let n = room.min(data.len());
        self.buf.extend_from_slice(&data[..n]);
        if self.buf.len() == ZERO_RUN {
            self.flush_run()?;
        }
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.flush_run()?;
        self.inner.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Read};
    use std::os::unix::fs::FileExt;

    #[test]
    fn create_is_sparse() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.img");
        let f = create_sparse(&p, 64 << 20).unwrap();
        assert_eq!(f.metadata().unwrap().len(), 64 << 20);
        assert_eq!(allocated_bytes(&p).unwrap(), 0);
        assert_eq!(f.allocated_bytes().unwrap(), 0);
    }

    #[test]
    fn punch_hole_frees_blocks() {
        use std::os::unix::fs::FileExt;
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("h.img");
        let f = create_sparse(&p, 8 << 20).unwrap();
        let data = vec![0xa5u8; 1 << 20];
        f.write_all_at(&data, 1 << 20).unwrap();
        f.write_all_at(&data, 4 << 20).unwrap();
        f.sync_all().unwrap();
        let before = allocated_bytes(&p).unwrap();
        assert!(before >= 2 << 20, "allocated {before}");

        f.punch_hole(1 << 20, 1 << 20).unwrap();
        f.sync_all().unwrap();
        let after = allocated_bytes(&p).unwrap();
        assert!(after < before, "{after} !< {before}");
        assert_eq!(f.metadata().unwrap().len(), 8 << 20);

        let mut back = vec![1u8; 1 << 20];
        f.read_exact_at(&mut back, 1 << 20).unwrap();
        assert!(back.iter().all(|&b| b == 0));
        f.read_exact_at(&mut back, 4 << 20).unwrap();
        assert_eq!(back, data);
    }

    #[test]
    fn sparse_writer_skips_zero_runs() {
        let mut w = SparseWriter::new(Cursor::new(Vec::new()));
        let zeros = vec![0u8; ZERO_RUN * 2];
        let ones = vec![1u8; ZERO_RUN + 10];
        w.write_all(&zeros).unwrap();
        w.write_all(&ones).unwrap();
        w.write_all(&zeros).unwrap();
        assert_eq!(w.position(), (ZERO_RUN * 5 + 10) as u64);
        let Written {
            inner: cur,
            len,
            holes,
        } = w.finish().unwrap();
        assert_eq!(len, (ZERO_RUN * 5 + 10) as u64);
        let r = ZERO_RUN as u64;
        assert_eq!(holes, vec![0..2 * r, 4 * r..5 * r + 10]);
        // Runs: [0,2R) zero (seeked over), [2R,3R) ones, [3R,4R) mixed
        // (written), [4R,5R+10) zero (seeked over). A Cursor only grows on
        // write, so the trailing hole is not materialised, exactly as a
        // file would leave it.
        let v = cur.into_inner();
        assert_eq!(v.len(), ZERO_RUN * 4);
        assert!(v[..ZERO_RUN * 2].iter().all(|&b| b == 0));
        assert!(v[ZERO_RUN * 2..ZERO_RUN * 3 + 10].iter().all(|&b| b == 1));
        assert!(v[ZERO_RUN * 3 + 10..].iter().all(|&b| b == 0));
    }

    #[test]
    fn sparse_writer_file_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("w.img");
        let f = create_sparse(&p, 0).unwrap();
        let mut w = SparseWriter::new(f);
        let total = 4 << 20;
        let mut expect = vec![0u8; total];
        expect[2 << 20..(2 << 20) + 4096].fill(0x5a);
        w.write_all(&expect).unwrap();
        let done = w.finish().unwrap();
        done.inner.set_len(done.len).unwrap();
        done.inner.settle_holes(&done.holes).unwrap();
        let mut got = Vec::new();
        File::open(&p).unwrap().read_to_end(&mut got).unwrap();
        assert_eq!(got, expect);
        // APFS would have filled the 2 MiB in front of the data.
        assert!(allocated_bytes(&p).unwrap() < 1 << 20);
    }

    #[test]
    fn settled_holes_are_whole_blocks_inside() {
        let mut holes = Vec::new();
        add_hole(&mut holes, 0..10);
        add_hole(&mut holes, 10..20);
        add_hole(&mut holes, 30..30);
        add_hole(&mut holes, 40..50);
        assert_eq!(holes, vec![0..20, 40..50], "joined, empty dropped");

        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("t.img");
        let f = create_sparse(&p, 64 << 10).unwrap();
        f.write_all_at(&vec![1u8; 64 << 10], 0).unwrap();
        // Only [8K, 12K) is a whole block inside the hole.
        let hole = 5000..13_000;
        f.settle_holes(std::slice::from_ref(&hole)).unwrap();
        let mut back = vec![0u8; 64 << 10];
        f.read_exact_at(&mut back, 0).unwrap();
        assert!(back[..8192].iter().all(|&b| b == 1));
        assert!(back[8192..12_288].iter().all(|&b| b == 0));
        assert!(back[12_288..].iter().all(|&b| b == 1));
    }
}
