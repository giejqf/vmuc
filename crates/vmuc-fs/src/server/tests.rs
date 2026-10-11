// SPDX-License-Identifier: Apache-2.0
//! The server driven with FUSE request bytes against a temp directory.

use std::path::Path;

use super::*;
use crate::abi::{InHeader, ROOT_ID, setattr};

struct Fs {
    srv: Server,
    dir: tempfile::TempDir,
    unique: u64,
}

const O_RDWR_CREAT: u32 = 0o102;

fn fs(read_only: bool) -> Fs {
    let dir = tempfile::tempdir().unwrap();
    let srv = Server::new(Passthrough::new(dir.path(), read_only).unwrap());
    Fs {
        srv,
        dir,
        unique: 0,
    }
}

fn u32_at(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}

fn u64_at(b: &[u8], o: usize) -> u64 {
    u64::from_le_bytes(b[o..o + 8].try_into().unwrap())
}

impl Fs {
    /// Sends one request; returns (errno, body).
    fn call(&mut self, op: u32, node: u64, body: &[u8]) -> (i32, Vec<u8>) {
        self.unique += 1;
        let h = InHeader {
            len: (40 + body.len()) as u32,
            opcode: op,
            unique: self.unique,
            nodeid: node,
            uid: 501,
            gid: 20,
            pid: 1,
        };
        let mut req = h.encode().to_vec();
        req.extend_from_slice(body);
        let reply = self.srv.handle(&req).expect("a reply");
        assert_eq!(u32_at(&reply, 0) as usize, reply.len());
        assert_eq!(u64_at(&reply, 8), self.unique);
        (-(u32_at(&reply, 4) as i32), reply[16..].to_vec())
    }

    fn ok(&mut self, op: u32, node: u64, body: &[u8]) -> Vec<u8> {
        let (e, b) = self.call(op, node, body);
        assert_eq!(e, 0, "opcode {op} failed with errno {e}");
        b
    }

    fn name(s: &str) -> Vec<u8> {
        let mut v = s.as_bytes().to_vec();
        v.push(0);
        v
    }

    /// (nodeid, size, mode)
    fn lookup(&mut self, parent: u64, name: &str) -> Result<(u64, u64, u32), i32> {
        let (e, b) = self.call(opcode::LOOKUP, parent, &Self::name(name));
        if e != 0 {
            return Err(e);
        }
        Ok((u64_at(&b, 0), u64_at(&b, 48), u32_at(&b, 100)))
    }

    fn create(&mut self, parent: u64, name: &str) -> (u64, u64) {
        let mut body = Writer::default();
        body.u32(O_RDWR_CREAT).u32(0o644).u32(0o022).u32(0);
        body.bytes(&Self::name(name));
        let b = self.ok(opcode::CREATE, parent, &body.0);
        (u64_at(&b, 0), u64_at(&b, 128))
    }

    fn mkdir(&mut self, parent: u64, name: &str) -> u64 {
        let mut body = Writer::default();
        body.u32(0o755).u32(0o022).bytes(&Self::name(name));
        u64_at(&self.ok(opcode::MKDIR, parent, &body.0), 0)
    }

    fn path(&self, rel: &str) -> std::path::PathBuf {
        self.dir.path().join(rel)
    }
}

#[test]
fn init_grants_only_what_both_sides_support() {
    let mut f = fs(false);
    let mut body = Writer::default();
    let offered = init::DO_READDIRPLUS | init::MAX_PAGES | (1 << 9) /* posix ACLs */;
    body.u32(7)
        .u32(41)
        .u32(128 << 10)
        .u32(offered as u32)
        .u32(0);
    body.bytes(&[0; 44]);
    let b = f.ok(opcode::INIT, 0, &body.0);
    assert_eq!(b.len(), 64);
    assert_eq!((u32_at(&b, 0), u32_at(&b, 4)), (7, 38));
    assert_eq!(
        u64::from(u32_at(&b, 12)),
        init::DO_READDIRPLUS | init::MAX_PAGES
    );
    assert_eq!(u32_at(&b, 20), MAX_WRITE);
}

#[test]
fn lookup_reports_attrs_and_linux_errnos() {
    let mut f = fs(false);
    std::fs::write(f.path("hello.txt"), b"hello").unwrap();
    let (node, size, mode) = f.lookup(ROOT_ID, "hello.txt").unwrap();
    assert!(node > ROOT_ID);
    assert_eq!(size, 5);
    assert_eq!(mode & 0o170_000, 0o100_000);
    assert_eq!(f.lookup(ROOT_ID, "missing"), Err(linux::ENOENT));
    assert_eq!(f.lookup(ROOT_ID, ".."), Err(linux::EINVAL));
    assert_eq!(f.lookup(ROOT_ID, "a/b"), Err(linux::EINVAL));
    // GETATTR on the root and the file.
    let b = f.ok(opcode::GETATTR, node, &[0; 16]);
    assert_eq!(u64_at(&b, 16 + 8), 5);
    f.ok(opcode::GETATTR, ROOT_ID, &[0; 16]);
}

#[test]
fn create_write_read_release() {
    let mut f = fs(false);
    let (node, fh) = f.create(ROOT_ID, "new.txt");
    let mut w = Writer::default();
    w.u64(fh)
        .u64(0)
        .u32(6)
        .u32(0)
        .u64(0)
        .u32(0)
        .u32(0)
        .bytes(b"abcdef");
    assert_eq!(u32_at(&f.ok(opcode::WRITE, node, &w.0), 0), 6);
    let mut r = Writer::default();
    r.u64(fh).u64(2).u32(3).u32(0).u64(0).u32(0).u32(0);
    assert_eq!(f.ok(opcode::READ, node, &r.0), b"cde");
    let mut rel = Writer::default();
    rel.u64(fh).u32(0).u32(0).u64(0);
    f.ok(opcode::RELEASE, node, &rel.0);
    assert_eq!(std::fs::read(f.path("new.txt")).unwrap(), b"abcdef");
    assert_eq!(
        f.call(opcode::READ, node, &r.0).0,
        linux::EBADF,
        "released handle"
    );
}

#[test]
fn directories_list_rename_and_refuse_removal_when_full() {
    let mut f = fs(false);
    let d = f.mkdir(ROOT_ID, "src");
    let (file, fh) = f.create(d, "lib.rs");
    f.ok(opcode::RELEASE, file, &{
        let mut w = Writer::default();
        w.u64(fh).u32(0).u32(0).u64(0);
        w.0
    });
    let dh = u64_at(&f.ok(opcode::OPENDIR, d, &[0; 8]), 0);
    let mut rd = Writer::default();
    rd.u64(dh).u64(0).u32(4096).u32(0).u64(0).u32(0).u32(0);
    let list = f.ok(opcode::READDIR, d, &rd.0);
    let names: Vec<&[u8]> = {
        let mut out = Vec::new();
        let mut at = 0;
        while at < list.len() {
            let len = u32_at(&list, at + 16) as usize;
            out.push(&list[at + 24..at + 24 + len]);
            at += abi::dirent_size(&list[at + 24..at + 24 + len]);
        }
        out
    };
    assert!(names.contains(&&b"lib.rs"[..]), "{names:?}");
    // READDIRPLUS carries entry_out ahead of each dirent.
    let plus = f.ok(opcode::READDIRPLUS, d, &rd.0);
    assert!(plus.len() > list.len());
    let mut rmdir = Fs::name("src");
    assert_eq!(f.call(opcode::RMDIR, ROOT_ID, &rmdir).0, linux::ENOTEMPTY);
    // Rename the directory: the file's node keeps working.
    let mut ren = Writer::default();
    ren.u64(ROOT_ID)
        .bytes(&Fs::name("src"))
        .bytes(&Fs::name("dst"));
    f.ok(opcode::RENAME, ROOT_ID, &ren.0);
    f.ok(opcode::GETATTR, file, &[0; 16]);
    assert!(f.path("dst/lib.rs").exists());
    f.ok(opcode::UNLINK, d, &Fs::name("lib.rs"));
    rmdir = Fs::name("dst");
    f.ok(opcode::RMDIR, ROOT_ID, &rmdir);
}

#[test]
fn setattr_truncates_and_chmods() {
    let mut f = fs(false);
    std::fs::write(f.path("t"), b"0123456789").unwrap();
    let (node, _, _) = f.lookup(ROOT_ID, "t").unwrap();
    let mut w = Writer::default();
    let valid = setattr::SIZE | setattr::MODE;
    w.u32(valid)
        .u32(0)
        .u64(0)
        .u64(3)
        .u64(0)
        .u64(0)
        .u64(0)
        .u64(0);
    w.u32(0)
        .u32(0)
        .u32(0)
        .u32(0o600)
        .u32(0)
        .u32(0)
        .u32(0)
        .u32(0);
    let b = f.ok(opcode::SETATTR, node, &w.0);
    assert_eq!(u64_at(&b, 16 + 8), 3);
    assert_eq!(u32_at(&b, 16 + 60) & 0o777, 0o600);
    assert_eq!(std::fs::read(f.path("t")).unwrap(), b"012");
}

#[test]
fn symlinks_are_readable_but_never_traversed() {
    let mut f = fs(false);
    let mut s = Fs::name("esc");
    s.extend(Fs::name("/etc"));
    let b = f.ok(opcode::SYMLINK, ROOT_ID, &s);
    let link = u64_at(&b, 0);
    assert_eq!(f.ok(opcode::READLINK, link, &[]), b"/etc");
    assert!(Path::new(&f.path("esc")).is_symlink());
    // Looking up through the link must not reach the host's /etc.
    #[cfg(target_os = "macos")]
    assert_eq!(f.lookup(link, "hosts"), Err(linux::ELOOP));
    // Opening the link itself fails rather than following it.
    let (e, _) = f.call(opcode::OPEN, link, &[0; 8]);
    assert_ne!(e, 0);
}

#[test]
fn read_only_shares_refuse_changes() {
    let mut f = fs(true);
    std::fs::write(f.path("r"), b"x").unwrap();
    let mut body = Writer::default();
    body.u32(O_RDWR_CREAT)
        .u32(0o644)
        .u32(0)
        .u32(0)
        .bytes(&Fs::name("new"));
    assert_eq!(f.call(opcode::CREATE, ROOT_ID, &body.0).0, linux::EROFS);
    let (node, _, _) = f.lookup(ROOT_ID, "r").unwrap();
    assert_eq!(
        f.call(opcode::OPEN, node, &[2, 0, 0, 0, 0, 0, 0, 0]).0,
        linux::EROFS
    );
    f.ok(opcode::OPEN, node, &[0; 8]);
    assert_eq!(
        f.call(opcode::UNLINK, ROOT_ID, &Fs::name("r")).0,
        linux::EROFS
    );
}

#[test]
fn forget_releases_nodes_and_unknown_opcodes_are_enosys() {
    let mut f = fs(false);
    std::fs::write(f.path("a"), b"").unwrap();
    let (node, _, _) = f.lookup(ROOT_ID, "a").unwrap();
    let before = f.srv.fs().rel(node).is_ok();
    assert!(before);
    let mut req = InHeader {
        len: 48,
        opcode: opcode::FORGET,
        unique: 99,
        nodeid: node,
        ..InHeader::default()
    }
    .encode()
    .to_vec();
    req.extend_from_slice(&1u64.to_le_bytes());
    assert!(f.srv.handle(&req).is_none(), "FORGET gets no reply");
    assert!(f.srv.fs().rel(node).is_err());
    assert_eq!(f.call(9999, ROOT_ID, &[]).0, linux::ENOSYS);
}

#[test]
fn xattrs_are_enosys_unless_enabled() {
    let mut f = fs(false);
    std::fs::write(f.path("x"), b"").unwrap();
    let (node, _, _) = f.lookup(ROOT_ID, "x").unwrap();
    let mut set = Writer::default();
    set.u32(3).u32(0).bytes(&Fs::name("user.k")).bytes(b"val");
    assert_eq!(f.call(opcode::SETXATTR, node, &set.0).0, linux::ENOSYS);
    let mut get = Writer::default();
    get.u32(0).u32(0).bytes(&Fs::name("security.capability"));
    assert_eq!(f.call(opcode::GETXATTR, node, &get.0).0, linux::ENOSYS);

    f.srv = Server::with_xattrs(Passthrough::new(f.dir.path(), false).unwrap());
    let (node, _, _) = f.lookup(ROOT_ID, "x").unwrap();
    f.ok(opcode::SETXATTR, node, &set.0);
    let mut get = Writer::default();
    get.u32(16).u32(0).bytes(&Fs::name("user.k"));
    assert_eq!(f.ok(opcode::GETXATTR, node, &get.0), b"val");
    let mut size = Writer::default();
    size.u32(0).u32(0);
    assert!(u32_at(&f.ok(opcode::LISTXATTR, node, &size.0), 0) >= 7);
}
