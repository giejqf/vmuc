// SPDX-License-Identifier: Apache-2.0
//! The FUSE wire protocol (Linux `include/uapi/linux/fuse.h`, 7.3x) as
//! the guest's virtiofs driver speaks it: little-endian structs read from
//! and written to byte buffers explicitly, no transmutes.

/// Protocol version we implement.
pub const KERNEL_VERSION: u32 = 7;
pub const KERNEL_MINOR_VERSION: u32 = 38;
/// The root inode's node ID.
pub const ROOT_ID: u64 = 1;

pub mod opcode {
    pub const LOOKUP: u32 = 1;
    pub const FORGET: u32 = 2;
    pub const GETATTR: u32 = 3;
    pub const SETATTR: u32 = 4;
    pub const READLINK: u32 = 5;
    pub const SYMLINK: u32 = 6;
    pub const MKNOD: u32 = 8;
    pub const MKDIR: u32 = 9;
    pub const UNLINK: u32 = 10;
    pub const RMDIR: u32 = 11;
    pub const RENAME: u32 = 12;
    pub const LINK: u32 = 13;
    pub const OPEN: u32 = 14;
    pub const READ: u32 = 15;
    pub const WRITE: u32 = 16;
    pub const STATFS: u32 = 17;
    pub const RELEASE: u32 = 18;
    pub const FSYNC: u32 = 20;
    pub const SETXATTR: u32 = 21;
    pub const GETXATTR: u32 = 22;
    pub const LISTXATTR: u32 = 23;
    pub const REMOVEXATTR: u32 = 24;
    pub const FLUSH: u32 = 25;
    pub const INIT: u32 = 26;
    pub const OPENDIR: u32 = 27;
    pub const READDIR: u32 = 28;
    pub const RELEASEDIR: u32 = 29;
    pub const FSYNCDIR: u32 = 30;
    pub const ACCESS: u32 = 34;
    pub const CREATE: u32 = 35;
    pub const INTERRUPT: u32 = 36;
    pub const DESTROY: u32 = 38;
    pub const BATCH_FORGET: u32 = 42;
    pub const FALLOCATE: u32 = 43;
    pub const READDIRPLUS: u32 = 44;
    pub const RENAME2: u32 = 45;
    pub const LSEEK: u32 = 46;
    pub const SETUPMAPPING: u32 = 48;
    pub const REMOVEMAPPING: u32 = 49;
    pub const SYNCFS: u32 = 50;
}

/// `FUSE_INIT` flags we may grant.
pub mod init {
    pub const ASYNC_READ: u64 = 1 << 0;
    pub const ATOMIC_O_TRUNC: u64 = 1 << 3;
    pub const BIG_WRITES: u64 = 1 << 5;
    pub const AUTO_INVAL_DATA: u64 = 1 << 12;
    pub const DO_READDIRPLUS: u64 = 1 << 13;
    pub const READDIRPLUS_AUTO: u64 = 1 << 14;
    pub const PARALLEL_DIROPS: u64 = 1 << 18;
    pub const MAX_PAGES: u64 = 1 << 22;
    pub const CACHE_SYMLINKS: u64 = 1 << 23;
    pub const MAP_ALIGNMENT: u64 = 1 << 26;
    pub const HAS_INODE_DAX: u64 = 1 << 33;
    pub const INIT_EXT: u64 = 1 << 30;
}

/// SETATTR `valid` bits.
pub mod setattr {
    pub const MODE: u32 = 1 << 0;
    pub const UID: u32 = 1 << 1;
    pub const GID: u32 = 1 << 2;
    pub const SIZE: u32 = 1 << 3;
    pub const ATIME: u32 = 1 << 4;
    pub const MTIME: u32 = 1 << 5;
    pub const FH: u32 = 1 << 6;
    pub const ATIME_NOW: u32 = 1 << 7;
    pub const MTIME_NOW: u32 = 1 << 8;
}

/// Reads a request body field by field.
#[derive(Debug)]
pub struct Reader<'a> {
    buf: &'a [u8],
    at: usize,
}

/// A request shorter than its opcode's struct.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("request truncated")]
pub struct Truncated;

impl<'a> Reader<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Self { buf, at: 0 }
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], Truncated> {
        let s = self.buf.get(self.at..self.at + n).ok_or(Truncated)?;
        self.at += n;
        Ok(s)
    }

    pub fn u16(&mut self) -> Result<u16, Truncated> {
        Ok(u16::from_le_bytes(
            self.take(2)?.try_into().map_err(|_| Truncated)?,
        ))
    }

    pub fn u32(&mut self) -> Result<u32, Truncated> {
        Ok(u32::from_le_bytes(
            self.take(4)?.try_into().map_err(|_| Truncated)?,
        ))
    }

    pub fn u64(&mut self) -> Result<u64, Truncated> {
        Ok(u64::from_le_bytes(
            self.take(8)?.try_into().map_err(|_| Truncated)?,
        ))
    }

    pub fn skip(&mut self, n: usize) -> Result<(), Truncated> {
        self.take(n).map(|_| ())
    }

    /// A NUL-terminated name.
    pub fn cstr(&mut self) -> Result<&'a [u8], Truncated> {
        let rest = &self.buf[self.at.min(self.buf.len())..];
        let n = rest.iter().position(|&b| b == 0).ok_or(Truncated)?;
        self.at += n + 1;
        Ok(&rest[..n])
    }

    /// Everything left (WRITE data, SETXATTR values).
    pub fn rest(&mut self) -> &'a [u8] {
        let r = &self.buf[self.at.min(self.buf.len())..];
        self.at = self.buf.len();
        r
    }
}

/// `fuse_in_header`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct InHeader {
    pub len: u32,
    pub opcode: u32,
    pub unique: u64,
    pub nodeid: u64,
    pub uid: u32,
    pub gid: u32,
    pub pid: u32,
}

pub const IN_HEADER_LEN: usize = 40;
pub const OUT_HEADER_LEN: usize = 16;

impl InHeader {
    pub fn read(r: &mut Reader) -> Result<Self, Truncated> {
        let h = Self {
            len: r.u32()?,
            opcode: r.u32()?,
            unique: r.u64()?,
            nodeid: r.u64()?,
            uid: r.u32()?,
            gid: r.u32()?,
            pid: r.u32()?,
        };
        r.skip(4)?; // total_extlen, padding
        Ok(h)
    }

    pub fn encode(&self) -> [u8; IN_HEADER_LEN] {
        let mut w = Writer::default();
        w.u32(self.len)
            .u32(self.opcode)
            .u64(self.unique)
            .u64(self.nodeid);
        w.u32(self.uid).u32(self.gid).u32(self.pid).u32(0);
        w.0.try_into().unwrap_or([0; IN_HEADER_LEN])
    }
}

/// Builds a reply body.
#[derive(Debug, Default, Clone)]
pub struct Writer(pub Vec<u8>);

impl Writer {
    pub fn u16(&mut self, v: u16) -> &mut Self {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }
    pub fn u32(&mut self, v: u32) -> &mut Self {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }
    pub fn u64(&mut self, v: u64) -> &mut Self {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }
    pub fn bytes(&mut self, b: &[u8]) -> &mut Self {
        self.0.extend_from_slice(b);
        self
    }
    /// Pads to an 8-byte boundary (dirents).
    pub fn align8(&mut self) -> &mut Self {
        let pad = self.0.len().next_multiple_of(8) - self.0.len();
        self.0.extend(std::iter::repeat_n(0, pad));
        self
    }
}

/// `struct fuse_attr`, filled from a host stat.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Attr {
    pub ino: u64,
    pub size: u64,
    pub blocks: u64,
    pub atime: (u64, u32),
    pub mtime: (u64, u32),
    pub ctime: (u64, u32),
    pub mode: u32,
    pub nlink: u32,
    pub uid: u32,
    pub gid: u32,
    pub rdev: u32,
    pub blksize: u32,
    /// `FUSE_ATTR_*`.
    pub flags: u32,
}

/// `fuse_attr.flags`: the guest may map this file through the DAX window
/// (when it mounted with `dax=inode`).
pub const ATTR_DAX: u32 = 1 << 1;

impl Attr {
    pub fn write(&self, w: &mut Writer) {
        w.u64(self.ino).u64(self.size).u64(self.blocks);
        w.u64(self.atime.0).u64(self.mtime.0).u64(self.ctime.0);
        w.u32(self.atime.1).u32(self.mtime.1).u32(self.ctime.1);
        w.u32(self.mode).u32(self.nlink).u32(self.uid).u32(self.gid);
        w.u32(self.rdev).u32(self.blksize).u32(self.flags);
    }
}

/// `fuse_entry_out`.
pub fn entry_out(w: &mut Writer, nodeid: u64, attr: &Attr, timeout_s: u64) {
    w.u64(nodeid)
        .u64(0)
        .u64(timeout_s)
        .u64(timeout_s)
        .u32(0)
        .u32(0);
    attr.write(w);
}

/// `fuse_attr_out`.
pub fn attr_out(w: &mut Writer, attr: &Attr, timeout_s: u64) {
    w.u64(timeout_s).u32(0).u32(0);
    attr.write(w);
}

/// `fuse_dirent` (name follows, padded to 8 bytes).
pub fn dirent(w: &mut Writer, ino: u64, off: u64, kind: u32, name: &[u8]) {
    w.u64(ino)
        .u64(off)
        .u32(name.len() as u32)
        .u32(kind)
        .bytes(name)
        .align8();
}

/// Size of one `fuse_dirent` with `name`.
pub fn dirent_size(name: &[u8]) -> usize {
    (24 + name.len()).next_multiple_of(8)
}

/// `fuse_out_header` followed by `body`.
pub fn reply(unique: u64, error: i32, body: &[u8]) -> Vec<u8> {
    let mut w = Writer::default();
    w.u32((OUT_HEADER_LEN + body.len()) as u32)
        .u32(error.cast_unsigned())
        .u64(unique);
    w.bytes(body);
    w.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_round_trip_and_sizes() {
        let h = InHeader {
            len: 48,
            opcode: opcode::LOOKUP,
            unique: 7,
            nodeid: ROOT_ID,
            uid: 501,
            gid: 20,
            pid: 99,
        };
        let b = h.encode();
        assert_eq!(InHeader::read(&mut Reader::new(&b)).unwrap(), h);
        let mut w = Writer::default();
        entry_out(&mut w, 2, &Attr::default(), 1);
        assert_eq!(w.0.len(), 128);
        let mut w = Writer::default();
        attr_out(&mut w, &Attr::default(), 1);
        assert_eq!(w.0.len(), 104);
        assert_eq!(dirent_size(b"a"), 32);
        assert_eq!(dirent_size(b"12345678"), 32);
        assert_eq!(dirent_size(b"123456789"), 40);
    }

    #[test]
    fn reader_bounds_and_names() {
        let mut r = Reader::new(b"abc\0de\0xyz");
        assert_eq!(r.cstr().unwrap(), b"abc");
        assert_eq!(r.cstr().unwrap(), b"de");
        assert_eq!(r.rest(), b"xyz");
        assert_eq!(Reader::new(b"no-nul").cstr(), Err(Truncated));
        assert_eq!(Reader::new(&[1, 2]).u32(), Err(Truncated));
    }

    #[test]
    fn reply_header_carries_length_and_errno() {
        let r = reply(9, -2, b"");
        assert_eq!(r.len(), 16);
        assert_eq!(u32::from_le_bytes(r[0..4].try_into().unwrap()), 16);
        assert_eq!(i32::from_le_bytes(r[4..8].try_into().unwrap()), -2);
        assert_eq!(u64::from_le_bytes(r[8..16].try_into().unwrap()), 9);
    }
}
