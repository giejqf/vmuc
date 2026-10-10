// SPDX-License-Identifier: Apache-2.0
//! ISO 9660 writer for cloud-init NoCloud seeds.
//!
//! Produces a small, deterministic image: 2048-byte sectors, a primary
//! volume descriptor, a set terminator, L and M path tables, directory
//! extents and file data. Names in the ISO namespace are ISO 9660 level 2
//! (upper-case d-characters, `NAME.EXT;1`); the POSIX names and modes the
//! guest actually sees travel in Rock Ridge (RRIP 1.10) `NM` and `PX`
//! entries, with `SP` and `ER` (via a `CE` continuation) on the root. No
//! Joliet, no symlinks, no timestamps beyond a fixed 2000-01-01.
//!
//! cloud-init looks for a volume labelled `cidata` (case-insensitively)
//! holding `user-data` and `meta-data`; [`write_iso`] with that label and
//! those files is the whole seed.

use std::collections::VecDeque;
use std::path::Path;

use crate::{Error, Result};

const SECTOR: usize = 2048;
const PVD_SECTOR: u32 = 16;
const DIR_MODE: u32 = 0o755;
const S_IFREG: u32 = 0o100_000;
const S_IFDIR: u32 = 0o040_000;
const MAX_NAME: usize = 128;
const RECORD_DATE: [u8; 7] = [100, 1, 1, 0, 0, 0, 0]; // 2000-01-01 00:00:00 UTC
const VOLUME_DATE: &[u8; 17] = b"2000010100000000\0";
const NO_DATE: &[u8; 17] = b"0000000000000000\0";
const ER_ID: &[u8] = b"RRIP_1991A";
const ER_DESC: &[u8] =
    b"THE ROCK RIDGE INTERCHANGE PROTOCOL PROVIDES SUPPORT FOR POSIX FILE SYSTEM SEMANTICS";
const ER_SRC: &[u8] = b"PLEASE CONTACT DISC PUBLISHER FOR SPECIFICATION SOURCE.  SEE PUBLISHER IDENTIFIER IN PRIMARY VOLUME DESCRIPTOR FOR CONTACT INFORMATION.";

/// One file to place in the seed image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeedFile {
    /// Slash-separated path relative to the root, e.g. `boot/00-x.sh`.
    pub path: String,
    /// File contents.
    pub data: Vec<u8>,
    /// POSIX permission bits, e.g. `0o644` or `0o755`.
    pub mode: u32,
}

impl SeedFile {
    /// A regular `0o644` file.
    pub fn new(path: impl Into<String>, data: impl Into<Vec<u8>>) -> Self {
        Self {
            path: path.into(),
            data: data.into(),
            mode: 0o644,
        }
    }

    /// Set the permission bits (`0o755` marks a script executable).
    #[must_use]
    pub fn with_mode(mut self, mode: u32) -> Self {
        self.mode = mode & 0o7777;
        self
    }
}

struct DirNode {
    name: String,
    iso_id: Vec<u8>,
    parent: usize,
    children: Vec<Child>,
    number: u16,
    extent: u32,
    size: u32,
}

struct FileNode {
    name: String,
    iso_id: Vec<u8>,
    input: usize,
    extent: u32,
}

/// ISO sort key: `(base, extension)` d-character parts.
type IsoKey = (Vec<u8>, Vec<u8>);

#[derive(Clone, Copy)]
enum Child {
    Dir(usize),
    File(usize),
}

struct Tree {
    dirs: Vec<DirNode>,
    files: Vec<FileNode>,
}

fn validate_component(c: &str, path: &str) -> Result<()> {
    if c.is_empty() || c == "." || c == ".." {
        return Err(Error::Iso(format!(
            "invalid path component {c:?} in {path:?}"
        )));
    }
    if c.len() > MAX_NAME {
        return Err(Error::Iso(format!(
            "name longer than {MAX_NAME} bytes in {path:?}"
        )));
    }
    if c.bytes().any(|b| b < 0x20 || b == 0x7f) {
        return Err(Error::Iso(format!("control character in {path:?}")));
    }
    Ok(())
}

impl Tree {
    fn build(inputs: &[SeedFile]) -> Result<Self> {
        let mut tree = Tree {
            dirs: vec![DirNode {
                name: String::new(),
                iso_id: vec![0],
                parent: 0,
                children: Vec::new(),
                number: 1,
                extent: 0,
                size: 0,
            }],
            files: Vec::new(),
        };
        for (input, f) in inputs.iter().enumerate() {
            let comps: Vec<&str> = f.path.trim_start_matches('/').split('/').collect();
            for c in &comps {
                validate_component(c, &f.path)?;
            }
            let (leaf, dirs) = comps.split_last().expect("split yields at least one");
            let mut cur = 0;
            for &c in dirs {
                cur = match tree.lookup(cur, c) {
                    Some(Child::Dir(d)) => d,
                    Some(Child::File(_)) => {
                        return Err(Error::Iso(format!("{c:?} is both a file and a directory")));
                    }
                    None => tree.add_dir(cur, c),
                };
            }
            if tree.lookup(cur, leaf).is_some() {
                return Err(Error::Iso(format!("duplicate path {:?}", f.path)));
            }
            tree.files.push(FileNode {
                name: (*leaf).to_owned(),
                iso_id: Vec::new(),
                input,
                extent: 0,
            });
            let id = tree.files.len() - 1;
            tree.dirs[cur].children.push(Child::File(id));
        }
        for d in 0..tree.dirs.len() {
            tree.assign_iso_names(d);
        }
        Ok(tree)
    }

    fn child_name(&self, c: Child) -> &str {
        match c {
            Child::Dir(d) => &self.dirs[d].name,
            Child::File(f) => &self.files[f].name,
        }
    }

    fn lookup(&self, dir: usize, name: &str) -> Option<Child> {
        self.dirs[dir]
            .children
            .iter()
            .copied()
            .find(|&c| self.child_name(c) == name)
    }

    fn add_dir(&mut self, parent: usize, name: &str) -> usize {
        self.dirs.push(DirNode {
            name: name.to_owned(),
            iso_id: Vec::new(),
            parent,
            children: Vec::new(),
            number: 0,
            extent: 0,
            size: 0,
        });
        let id = self.dirs.len() - 1;
        self.dirs[parent].children.push(Child::Dir(id));
        id
    }

    /// Give every child of `dir` a unique ISO identifier and sort them.
    fn assign_iso_names(&mut self, dir: usize) {
        let children = self.dirs[dir].children.clone();
        let mut used: Vec<IsoKey> = Vec::new();
        let mut keyed: Vec<(IsoKey, Child)> = Vec::new();
        for c in children {
            let is_dir = matches!(c, Child::Dir(_));
            let (mut base, ext) = iso_parts(self.child_name(c), is_dir);
            let mut n = 0u32;
            while used.contains(&(base.clone(), ext.clone())) {
                n += 1;
                let (stem, _) = iso_parts(self.child_name(c), is_dir);
                let keep = stem.len().min(if is_dir { 28 } else { 24 });
                base = stem[..keep].to_vec();
                base.extend_from_slice(format!("{n:03}").as_bytes());
            }
            used.push((base.clone(), ext.clone()));
            match c {
                Child::Dir(d) => self.dirs[d].iso_id.clone_from(&base),
                Child::File(f) => {
                    let mut id = base.clone();
                    id.push(b'.');
                    id.extend_from_slice(&ext);
                    id.extend_from_slice(b";1");
                    self.files[f].iso_id = id;
                }
            }
            keyed.push(((base, ext), c));
        }
        keyed.sort_by(|a, b| a.0.cmp(&b.0));
        self.dirs[dir].children = keyed.into_iter().map(|(_, c)| c).collect();
    }

    /// Directories in path-table order (breadth first, siblings sorted).
    fn bfs(&self) -> Vec<usize> {
        let mut order = Vec::with_capacity(self.dirs.len());
        let mut queue = VecDeque::from([0usize]);
        while let Some(d) = queue.pop_front() {
            order.push(d);
            for c in &self.dirs[d].children {
                if let Child::Dir(sub) = *c {
                    queue.push_back(sub);
                }
            }
        }
        order
    }

    fn subdir_count(&self, dir: usize) -> u32 {
        self.dirs[dir]
            .children
            .iter()
            .filter(|c| matches!(c, Child::Dir(_)))
            .count() as u32
    }

    fn path_table(&self, order: &[usize], big_endian: bool) -> Vec<u8> {
        let mut out = Vec::new();
        for &d in order {
            let dir = &self.dirs[d];
            out.push(dir.iso_id.len() as u8);
            out.push(0);
            let parent = self.dirs[dir.parent].number;
            if big_endian {
                out.extend_from_slice(&dir.extent.to_be_bytes());
                out.extend_from_slice(&parent.to_be_bytes());
            } else {
                out.extend_from_slice(&dir.extent.to_le_bytes());
                out.extend_from_slice(&parent.to_le_bytes());
            }
            out.extend_from_slice(&dir.iso_id);
            if dir.iso_id.len() % 2 == 1 {
                out.push(0);
            }
        }
        out
    }

    /// The directory extent of `d`, padded to whole sectors. Locations are
    /// whatever the nodes currently hold, so this is called once to size
    /// and once more to emit.
    fn dir_extent(&self, d: usize, inputs: &[SeedFile], ce_block: u32) -> Vec<u8> {
        let dir = &self.dirs[d];
        let parent = &self.dirs[dir.parent];
        let mut out = Vec::new();
        let nlink = 2 + self.subdir_count(d);
        let mut self_susp = Vec::new();
        if d == 0 {
            self_susp.extend_from_slice(&[b'S', b'P', 7, 1, 0xbe, 0xef, 0]);
        }
        self_susp.extend(susp_px(S_IFDIR | DIR_MODE, nlink));
        if d == 0 {
            self_susp.extend(susp_ce(ce_block, susp_er().len() as u32));
        }
        push_record(&mut out, dir.extent, dir.size, true, &[0], &self_susp);
        let parent_susp = susp_px(S_IFDIR | DIR_MODE, 2 + self.subdir_count(dir.parent));
        push_record(
            &mut out,
            parent.extent,
            parent.size,
            true,
            &[1],
            &parent_susp,
        );
        for &c in &dir.children {
            let mut susp = Vec::new();
            match c {
                Child::Dir(sub) => {
                    let node = &self.dirs[sub];
                    susp.extend(susp_nm(&node.name));
                    susp.extend(susp_px(S_IFDIR | DIR_MODE, 2 + self.subdir_count(sub)));
                    push_record(&mut out, node.extent, node.size, true, &node.iso_id, &susp);
                }
                Child::File(f) => {
                    let node = &self.files[f];
                    let input = &inputs[node.input];
                    susp.extend(susp_nm(&node.name));
                    susp.extend(susp_px(S_IFREG | input.mode, 1));
                    let len = input.data.len() as u32;
                    push_record(&mut out, node.extent, len, false, &node.iso_id, &susp);
                }
            }
        }
        out.resize(out.len().div_ceil(SECTOR) * SECTOR, 0);
        out
    }
}

/// Map a POSIX name to ISO level 2 `(base, ext)` d-character parts.
fn iso_parts(name: &str, is_dir: bool) -> IsoKey {
    let dchar = |c: u8| match c.to_ascii_uppercase() {
        c @ (b'A'..=b'Z' | b'0'..=b'9' | b'_') => c,
        _ => b'_',
    };
    let (stem, ext) = if is_dir {
        (name, "")
    } else {
        match name.rsplit_once('.') {
            Some((s, e)) if !s.is_empty() && !e.is_empty() => (s, e),
            _ => (name, ""),
        }
    };
    let mut base: Vec<u8> = stem.bytes().map(dchar).collect();
    let mut ext: Vec<u8> = ext.bytes().map(dchar).collect();
    ext.truncate(3);
    base.truncate(if is_dir { 31 } else { 30 - ext.len() });
    (base, ext)
}

fn both_u16(v: u16) -> [u8; 4] {
    let (le, be) = (v.to_le_bytes(), v.to_be_bytes());
    [le[0], le[1], be[0], be[1]]
}

fn both_u32(v: u32) -> [u8; 8] {
    let mut out = [0u8; 8];
    out[..4].copy_from_slice(&v.to_le_bytes());
    out[4..].copy_from_slice(&v.to_be_bytes());
    out
}

fn susp_px(mode: u32, nlink: u32) -> Vec<u8> {
    let mut e = vec![b'P', b'X', 36, 1];
    e.extend_from_slice(&both_u32(mode));
    e.extend_from_slice(&both_u32(nlink));
    e.extend_from_slice(&both_u32(0));
    e.extend_from_slice(&both_u32(0));
    e
}

fn susp_nm(name: &str) -> Vec<u8> {
    let mut e = vec![b'N', b'M', (5 + name.len()) as u8, 1, 0];
    e.extend_from_slice(name.as_bytes());
    e
}

fn susp_ce(block: u32, len: u32) -> Vec<u8> {
    let mut e = vec![b'C', b'E', 28, 1];
    e.extend_from_slice(&both_u32(block));
    e.extend_from_slice(&both_u32(0));
    e.extend_from_slice(&both_u32(len));
    e
}

fn susp_er() -> Vec<u8> {
    let len = 8 + ER_ID.len() + ER_DESC.len() + ER_SRC.len();
    let mut e = vec![b'E', b'R', len as u8, 1];
    e.extend_from_slice(&[
        ER_ID.len() as u8,
        ER_DESC.len() as u8,
        ER_SRC.len() as u8,
        1,
    ]);
    e.extend_from_slice(ER_ID);
    e.extend_from_slice(ER_DESC);
    e.extend_from_slice(ER_SRC);
    e
}

/// Append a directory record, moving to the next sector first if it would
/// straddle a sector boundary (ECMA-119 6.8.1.1).
fn push_record(out: &mut Vec<u8>, extent: u32, size: u32, is_dir: bool, id: &[u8], susp: &[u8]) {
    let mut rec = vec![0u8; 33];
    rec.extend_from_slice(id);
    if id.len().is_multiple_of(2) {
        rec.push(0);
    }
    rec.extend_from_slice(susp);
    if rec.len() % 2 == 1 {
        rec.push(0);
    }
    assert!(rec.len() <= 255, "directory record too long");
    rec[0] = rec.len() as u8;
    rec[2..10].copy_from_slice(&both_u32(extent));
    rec[10..18].copy_from_slice(&both_u32(size));
    rec[18..25].copy_from_slice(&RECORD_DATE);
    rec[25] = u8::from(is_dir) << 1;
    rec[28..32].copy_from_slice(&both_u16(1));
    rec[32] = id.len() as u8;
    let used = out.len() % SECTOR;
    if used != 0 && used + rec.len() > SECTOR {
        out.resize(out.len() + SECTOR - used, 0);
    }
    out.extend_from_slice(&rec);
}

fn put_str(buf: &mut [u8], s: &[u8]) {
    buf.fill(b' ');
    let n = s.len().min(buf.len());
    buf[..n].copy_from_slice(&s[..n]);
}

/// Build an ISO 9660 image with Rock Ridge extensions in memory.
pub fn write_iso(volume_id: &str, files: &[SeedFile]) -> Result<Vec<u8>> {
    if volume_id.is_empty()
        || volume_id.len() > 32
        || !volume_id.bytes().all(|b| (0x20..0x7f).contains(&b))
    {
        return Err(Error::Iso(format!("invalid volume label {volume_id:?}")));
    }
    let mut tree = Tree::build(files)?;
    let order = tree.bfs();
    for (n, &d) in order.iter().enumerate() {
        tree.dirs[d].number = n as u16 + 1;
    }

    let pt_len = tree.path_table(&order, false).len();
    let pt_sectors = pt_len.div_ceil(SECTOR) as u32;
    let l_table = PVD_SECTOR + 2;
    let m_table = l_table + pt_sectors;
    let ce_block = m_table + pt_sectors;
    let mut next = ce_block + 1;
    for &d in &order {
        let size = tree.dir_extent(d, files, ce_block).len() as u32;
        tree.dirs[d].size = size;
        tree.dirs[d].extent = next;
        next += size / SECTOR as u32;
    }
    for &d in &order {
        for c in tree.dirs[d].children.clone() {
            if let Child::File(f) = c {
                tree.files[f].extent = next;
                next += files[tree.files[f].input].data.len().div_ceil(SECTOR) as u32;
            }
        }
    }
    let total_sectors = next;
    let mut img = vec![0u8; total_sectors as usize * SECTOR];
    let sector = |n: u32| n as usize * SECTOR..(n as usize + 1) * SECTOR;

    // Primary volume descriptor.
    let root = &tree.dirs[0];
    let mut root_record = Vec::new();
    push_record(&mut root_record, root.extent, root.size, true, &[0], &[]);
    let pvd = &mut img[sector(PVD_SECTOR)];
    pvd[0] = 1;
    pvd[1..6].copy_from_slice(b"CD001");
    pvd[6] = 1;
    put_str(&mut pvd[8..40], b"");
    put_str(&mut pvd[40..72], volume_id.as_bytes());
    pvd[80..88].copy_from_slice(&both_u32(total_sectors));
    pvd[120..124].copy_from_slice(&both_u16(1));
    pvd[124..128].copy_from_slice(&both_u16(1));
    pvd[128..132].copy_from_slice(&both_u16(SECTOR as u16));
    pvd[132..140].copy_from_slice(&both_u32(pt_len as u32));
    pvd[140..144].copy_from_slice(&l_table.to_le_bytes());
    pvd[148..152].copy_from_slice(&m_table.to_be_bytes());
    pvd[156..190].copy_from_slice(&root_record);
    put_str(&mut pvd[190..318], b"");
    put_str(&mut pvd[318..446], b"");
    put_str(&mut pvd[446..574], b"");
    put_str(&mut pvd[574..702], b"VMUC");
    put_str(&mut pvd[702..739], b"");
    put_str(&mut pvd[739..776], b"");
    put_str(&mut pvd[776..813], b"");
    pvd[813..830].copy_from_slice(VOLUME_DATE);
    pvd[830..847].copy_from_slice(VOLUME_DATE);
    pvd[847..864].copy_from_slice(NO_DATE);
    pvd[864..881].copy_from_slice(NO_DATE);
    pvd[881] = 1;

    // Terminator.
    let term = &mut img[sector(PVD_SECTOR + 1)];
    term[0] = 255;
    term[1..6].copy_from_slice(b"CD001");
    term[6] = 1;

    // Path tables, continuation area, directories, files.
    let l = tree.path_table(&order, false);
    let m = tree.path_table(&order, true);
    img[l_table as usize * SECTOR..][..l.len()].copy_from_slice(&l);
    img[m_table as usize * SECTOR..][..m.len()].copy_from_slice(&m);
    let er = susp_er();
    img[ce_block as usize * SECTOR..][..er.len()].copy_from_slice(&er);
    for &d in &order {
        let ext = tree.dir_extent(d, files, ce_block);
        let start = tree.dirs[d].extent as usize * SECTOR;
        img[start..start + ext.len()].copy_from_slice(&ext);
    }
    for f in &tree.files {
        let data = &files[f.input].data;
        let start = f.extent as usize * SECTOR;
        img[start..start + data.len()].copy_from_slice(data);
    }
    Ok(img)
}

/// [`write_iso`] straight to a file.
pub fn write_iso_file(path: &Path, volume_id: &str, files: &[SeedFile]) -> Result<()> {
    std::fs::write(path, write_iso(volume_id, files)?)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn seed() -> Vec<SeedFile> {
        let big: Vec<u8> = (0..5000u32).map(|i| (i % 251) as u8).collect();
        vec![
            SeedFile::new("user-data", "#cloud-config\nhostname: test\n"),
            SeedFile::new("meta-data", "instance-id: i-1\nlocal-hostname: test\n"),
            SeedFile::new("boot/00-hello.sh", "#!/bin/sh\necho hi\n").with_mode(0o755),
            SeedFile::new("boot/sub/deep.txt", "deep"),
            SeedFile::new("empty", ""),
            SeedFile::new("vmuc-agent", big),
            SeedFile::new("network-config", "version: 2\n"),
        ]
    }

    fn isoinfo(args: &[&str], img: &Path) -> Option<String> {
        let out = Command::new("isoinfo")
            .args(args)
            .arg("-i")
            .arg(img)
            .output()
            .ok()?;
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        Some(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    #[test]
    fn structure() {
        let img = write_iso("cidata", &seed()).unwrap();
        assert_eq!(img.len() % SECTOR, 0);
        let pvd = &img[16 * SECTOR..17 * SECTOR];
        assert_eq!(&pvd[..7], b"\x01CD001\x01");
        assert_eq!(&pvd[40..72], b"cidata                          ");
        let sectors = (img.len() / SECTOR) as u32;
        assert_eq!(&pvd[80..88], &both_u32(sectors));
        assert_eq!(&pvd[128..132], &[0, 8, 8, 0]);
        let pt_len = u32::from_le_bytes(pvd[132..136].try_into().unwrap());
        assert_eq!(
            u32::from_be_bytes(pvd[136..140].try_into().unwrap()),
            pt_len
        );
        let l_loc = u32::from_le_bytes(pvd[140..144].try_into().unwrap()) as usize;
        let m_loc = u32::from_be_bytes(pvd[148..152].try_into().unwrap()) as usize;
        assert_eq!((l_loc, m_loc), (18, 19));
        let (l, m) = (
            &img[l_loc * SECTOR..][..pt_len as usize],
            &img[m_loc * SECTOR..][..pt_len as usize],
        );
        // Root, boot, boot/sub: 10 + 12 + 12 bytes (odd identifiers are padded).
        assert_eq!(pt_len, 34);
        assert_eq!(&l[8..9], b"\0");
        assert_eq!(&l[18..22], b"BOOT");
        assert_eq!(&m[18..22], b"BOOT");
        assert_eq!(l[16..18], [1, 0]);
        assert_eq!(m[16..18], [0, 1]);
        assert_eq!(&l[30..33], b"SUB");
        assert_eq!(l[28..30], [2, 0]);
        assert_eq!(&pvd[881..883], &[1, 0]);
        assert_eq!(&img[17 * SECTOR..17 * SECTOR + 7], b"\xffCD001\x01");
        // Root record in the PVD points at the root extent whose first
        // record carries SP.
        let root_extent = u32::from_le_bytes(pvd[158..162].try_into().unwrap()) as usize;
        let root = &img[root_extent * SECTOR..];
        assert_eq!(root[32..34], [1, 0]);
        assert_eq!(&root[34..41], &[b'S', b'P', 7, 1, 0xbe, 0xef, 0]);
        assert_eq!(&root[41..45], &[b'P', b'X', 36, 1]);
        assert_eq!(&root[77..81], &[b'C', b'E', 28, 1]);
        assert_eq!(root[0] % 2, 0);
        // Deterministic.
        assert_eq!(write_iso("cidata", &seed()).unwrap(), img);
    }

    #[test]
    fn iso_name_mapping() {
        assert_eq!(
            iso_parts("user-data", false),
            (b"USER_DATA".to_vec(), vec![])
        );
        assert_eq!(
            iso_parts("00-hello.sh", false),
            (b"00_HELLO".to_vec(), b"SH".to_vec())
        );
        assert_eq!(
            iso_parts("a.tar.gz", false),
            (b"A_TAR".to_vec(), b"GZ".to_vec())
        );
        assert_eq!(iso_parts(".hidden", false), (b"_HIDDEN".to_vec(), vec![]));
        assert_eq!(iso_parts("boot.d", true), (b"BOOT_D".to_vec(), vec![]));
        let long = "x".repeat(40);
        assert_eq!(iso_parts(&long, false).0.len(), 30);
        assert_eq!(
            iso_parts(&format!("{long}.yaml"), false),
            (vec![b'X'; 27], b"YAM".to_vec())
        );
    }

    #[test]
    fn colliding_iso_names_stay_unique() {
        let files = vec![
            SeedFile::new("a-b", "1"),
            SeedFile::new("a_b", "2"),
            SeedFile::new("A.B", "3"),
            SeedFile::new("a.b", "4"),
        ];
        let tree = Tree::build(&files).unwrap();
        let mut ids: Vec<&[u8]> = tree.files.iter().map(|f| f.iso_id.as_slice()).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), 4);
        assert!(ids.contains(&&b"A_B001.;1"[..]), "{ids:?}");
        assert!(ids.contains(&&b"A001.B;1"[..]), "{ids:?}");
    }

    #[test]
    fn rejects_bad_input() {
        let dup = [SeedFile::new("a", ""), SeedFile::new("a", "")];
        assert!(matches!(write_iso("cidata", &dup), Err(Error::Iso(_))));
        let clash = [SeedFile::new("a", ""), SeedFile::new("a/b", "")];
        assert!(matches!(write_iso("cidata", &clash), Err(Error::Iso(_))));
        for bad in ["", "..", "a/../b", "a//b", "a\x01"] {
            assert!(
                write_iso("cidata", &[SeedFile::new(bad, "")]).is_err(),
                "{bad:?}"
            );
        }
        assert!(write_iso("", &[]).is_err());
        assert!(write_iso(&"x".repeat(33), &[]).is_err());
        assert!(write_iso("cidata", &[]).is_ok());
    }

    #[test]
    fn isoinfo_agrees() {
        let tmp = tempfile::tempdir().unwrap();
        let img = tmp.path().join("cidata.iso");
        let files = seed();
        write_iso_file(&img, "cidata", &files).unwrap();
        let Some(desc) = isoinfo(&["-d"], &img) else {
            eprintln!("skipping: isoinfo not installed");
            return;
        };
        assert!(desc.contains("Volume id: cidata"), "{desc}");
        assert!(desc.contains("Logical block size is: 2048"), "{desc}");
        assert!(
            desc.contains("Rock Ridge signatures version 1 found"),
            "{desc}"
        );
        assert!(desc.contains("Application id: VMUC"), "{desc}");

        let listing = isoinfo(&["-R", "-l"], &img).unwrap();
        let line = |name: &str| {
            listing
                .lines()
                .find(|l| l.ends_with(&format!(" {name} ")) || l.ends_with(&format!(" {name}")))
                .unwrap_or_else(|| panic!("{name} missing in\n{listing}"))
                .to_owned()
        };
        assert!(line("user-data").starts_with("-rw-r--r--"), "{listing}");
        assert!(line("00-hello.sh").starts_with("-rwxr-xr-x"), "{listing}");
        assert!(line("boot").starts_with("drwxr-xr-x"), "{listing}");
        assert!(line("deep.txt").starts_with("-rw-r--r--"), "{listing}");
        assert!(
            listing.contains("Directory listing of /boot/sub/"),
            "{listing}"
        );
        assert!(line("vmuc-agent").contains(" 5000 "), "{listing}");
        assert!(line("empty").contains(" 0 "), "{listing}");

        let plain = isoinfo(&["-l"], &img).unwrap();
        assert!(plain.contains("USER_DATA.;1"), "{plain}");
        assert!(plain.contains("00_HELLO.SH;1"), "{plain}");

        let paths = isoinfo(&["-R", "-f"], &img).unwrap();
        for p in [
            "/user-data",
            "/meta-data",
            "/boot/00-hello.sh",
            "/boot/sub/deep.txt",
            "/empty",
            "/vmuc-agent",
            "/network-config",
        ] {
            assert!(paths.lines().any(|l| l == p), "{p} not in\n{paths}");
        }

        for f in &files {
            let out = Command::new("isoinfo")
                .args(["-R", "-x", &format!("/{}", f.path), "-i"])
                .arg(&img)
                .output()
                .unwrap();
            assert!(out.status.success());
            assert_eq!(out.stdout, f.data, "{}", f.path);
        }
    }

    #[test]
    fn many_files_span_directory_sectors() {
        let files: Vec<SeedFile> = (0..60)
            .map(|i| SeedFile::new(format!("file-number-{i:02}.txt"), format!("{i}")))
            .collect();
        let tmp = tempfile::tempdir().unwrap();
        let img = tmp.path().join("many.iso");
        write_iso_file(&img, "CIDATA", &files).unwrap();
        let raw = std::fs::read(&img).unwrap();
        let pvd = &raw[16 * SECTOR..];
        let root_size = u32::from_le_bytes(pvd[166..170].try_into().unwrap());
        assert!(root_size > SECTOR as u32, "root dir should span sectors");
        let Some(paths) = isoinfo(&["-R", "-f"], &img) else {
            return;
        };
        assert_eq!(
            paths
                .lines()
                .filter(|l| l.starts_with("/file-number-"))
                .count(),
            60
        );
        let out = Command::new("isoinfo")
            .args(["-R", "-x", "/file-number-59.txt", "-i"])
            .arg(&img)
            .output()
            .unwrap();
        assert_eq!(out.stdout, b"59");
    }
}
