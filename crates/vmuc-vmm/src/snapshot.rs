// SPDX-License-Identifier: Apache-2.0
//! Snapshots: a paused VM written to a directory, and the files a VM is
//! rebuilt from (`Vm::snapshot`, `VmBuilder::restore`).
//!
//! ```text
//! state.json   format version, machine shape, each vCPU (its registers,
//!              or that it is off or about to start), each virtio
//!              transport, each platform device
//! gic.bin      the framework's GIC state, opaque and versioned by macOS
//! ram.img      guest RAM at its offsets, sparse: only the pages the guest
//!              holds that are not zero are written
//! ```
//!
//! Guest RAM comes back either copied into fresh granules (billed as the
//! guest touches nothing more than it had) or mapped copy-on-write from
//! the file (docs/experiments/2026-10-13-e5-snapshot-restore.md).

use std::collections::BTreeMap;
use std::fs::File;
use std::io;
use std::ops::Range;
use std::os::fd::AsRawFd;
use std::os::unix::fs::FileExt;
use std::path::Path;

use serde::{Deserialize, Serialize};
use vmuc_hv::{HOST_PAGE_SIZE, VcpuState};
use vmuc_image::sparse::{SparseFileExt, add_hole};
use vmuc_virtio::TransportState;

use crate::machine::CpuSnapshot;

pub const VERSION: u32 = 1;
pub const STATE_FILE: &str = "state.json";
pub const GIC_FILE: &str = "gic.bin";
pub const RAM_FILE: &str = "ram.img";

/// What `state.json` holds.
#[derive(Debug, Serialize, Deserialize)]
pub struct State {
    pub version: u32,
    pub cpus: u32,
    pub ram_size: u64,
    /// `vmuc_hv::state::host_counter()` when the vCPUs stopped: a restore
    /// shifts the vtimer offsets by the time since, so the guest's clock
    /// carries on from here.
    pub paused_at: u64,
    pub vcpus: Vec<Cpu>,
    pub transports: Vec<TransportState>,
    pub devices: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "power", rename_all = "snake_case")]
pub enum Cpu {
    Off,
    Starting { entry: u64, context: u64 },
    Running(Regs),
}

/// [`VcpuState`] as stored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Regs {
    pub regs: Vec<u64>,
    pub simd: Vec<[u8; 16]>,
    pub sys_regs: Vec<(u16, u64)>,
    pub icc_regs: Vec<(u16, u64)>,
    pub vtimer_offset: u64,
    pub vtimer_masked: bool,
}

impl From<CpuSnapshot> for Cpu {
    fn from(c: CpuSnapshot) -> Self {
        match c {
            CpuSnapshot::Off => Cpu::Off,
            CpuSnapshot::Starting { entry, context } => Cpu::Starting { entry, context },
            CpuSnapshot::Running(s) => Cpu::Running(Regs {
                regs: s.regs,
                simd: s.simd,
                sys_regs: s.sys_regs,
                icc_regs: s.icc_regs,
                vtimer_offset: s.vtimer_offset,
                vtimer_masked: s.vtimer_masked,
            }),
        }
    }
}

impl From<Regs> for VcpuState {
    fn from(r: Regs) -> Self {
        VcpuState {
            regs: r.regs,
            simd: r.simd,
            sys_regs: r.sys_regs,
            icc_regs: r.icc_regs,
            vtimer_offset: r.vtimer_offset,
            vtimer_masked: r.vtimer_masked,
        }
    }
}

/// Writes `path` by renaming a finished temporary file over it, so a VM
/// restored with its RAM mapped from an older file keeps that file whole.
fn replace(path: &Path, write: impl FnOnce(&Path) -> io::Result<()>) -> io::Result<()> {
    let tmp = path.with_extension("tmp");
    write(&tmp)?;
    std::fs::rename(tmp, path)
}

impl State {
    /// Writes the state last: a directory with a state file is complete.
    pub fn write(&self, dir: &Path, gic: &[u8]) -> io::Result<()> {
        replace(&dir.join(GIC_FILE), |p| std::fs::write(p, gic))?;
        let json = serde_json::to_vec(self).map_err(io::Error::other)?;
        replace(&dir.join(STATE_FILE), |p| std::fs::write(p, json))
    }

    /// The state and the GIC data.
    pub fn read(dir: &Path) -> io::Result<(Self, Vec<u8>)> {
        let state: Self = serde_json::from_slice(&std::fs::read(dir.join(STATE_FILE))?)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        if state.version != VERSION {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "snapshot format {} (this vmuc reads {VERSION})",
                    state.version
                ),
            ));
        }
        Ok((state, std::fs::read(dir.join(GIC_FILE))?))
    }
}

/// How guest RAM comes back from `ram.img`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RamRestore {
    /// Read the written pages into guest RAM's fresh granules.
    Copy,
    /// Map the file copy-on-write over guest RAM: pages come in as the
    /// guest touches them and stay the file cache's until it writes them.
    Map,
}

/// Pages of guest RAM looked at per `mincore`.
const SCAN: usize = 256;

/// Writes the host pages of `[host, host + len)` that hold data to `path`
/// at their offsets, leaving the rest a hole; returns the bytes written.
/// A page holds data if it is not zero and is in memory, compressed or
/// paged out, or lies in `image`, the data of the file guest RAM was
/// mapped from (see [`map_ram`]), which a page never touched since still
/// reads from. Nothing may write guest RAM meanwhile. A file already
/// there is replaced, not rewritten: a VM may have it mapped.
///
/// # Safety
/// `[host, host + len)` is mapped and readable.
pub unsafe fn write_ram(
    host: *const u8,
    len: usize,
    path: &Path,
    image: &[Range<usize>],
) -> io::Result<u64> {
    let mut written = 0;
    replace(path, |tmp| {
        // SAFETY: the caller's guarantee.
        written = unsafe { write_ram_to(host, len, tmp, image) }?;
        Ok(())
    })?;
    Ok(written)
}

/// Whether a host page may hold data, by `mincore`'s flags for its system
/// pages and whether the image RAM was mapped from has data there.
fn may_hold_data(flags: &[u8], in_image: bool) -> bool {
    #[cfg(target_os = "macos")]
    {
        // MINCORE_ANONYMOUS alone: anonymous memory never touched (or a
        // granule reclaim refreshed). No flag at all: a page of the image,
        // not in memory. Anything else (in memory, paged out or
        // compressed, copied): maybe data.
        const ANONYMOUS: u8 = 0x80;
        flags.iter().any(|&f| f & !ANONYMOUS != 0) || (in_image && flags.iter().all(|&f| f == 0))
    }
    #[cfg(not(target_os = "macos"))]
    {
        flags.iter().any(|&f| f & 1 != 0) || in_image
    }
}

/// # Safety
/// As [`write_ram`].
unsafe fn write_ram_to(
    host: *const u8,
    len: usize,
    path: &Path,
    image: &[Range<usize>],
) -> io::Result<u64> {
    let file = File::create(path)?;
    file.set_len(len as u64)?;
    let mut written = 0u64;
    let mut run: Option<usize> = None;
    // What is not written, for APFS to leave unallocated (it fills short
    // holes in when it flushes; see vmuc_image::sparse).
    let mut holes = Vec::new();
    let mut data_end = 0u64;
    let mut flush = |start: usize, end: usize, written: &mut u64| -> io::Result<()> {
        // SAFETY: inside the caller's readable range.
        let bytes = unsafe { std::slice::from_raw_parts(host.add(start), end - start) };
        file.write_all_at(bytes, start as u64)?;
        add_hole(&mut holes, data_end..start as u64);
        data_end = end as u64;
        *written += (end - start) as u64;
        Ok(())
    };
    // mincore answers per system page: 16 KiB on Apple silicon, smaller
    // where the tests also run.
    // SAFETY: no arguments.
    let sys = usize::try_from(unsafe { libc::sysconf(libc::_SC_PAGESIZE) })
        .unwrap_or(HOST_PAGE_SIZE)
        .clamp(1, HOST_PAGE_SIZE);
    let per = HOST_PAGE_SIZE / sys;
    let mut vec = vec![0u8; SCAN * per];
    // Sorted, as pages are visited.
    let mut extents = image.iter().peekable();
    for chunk in (0..len).step_by(SCAN * HOST_PAGE_SIZE) {
        let n = (len - chunk).min(SCAN * HOST_PAGE_SIZE);
        // SAFETY: a mapped range and a vector of a byte per system page.
        let resident = unsafe {
            libc::mincore(
                host.add(chunk).cast_mut().cast(),
                n,
                vec.as_mut_ptr().cast(),
            )
        };
        for (i, page) in (chunk..chunk + n).step_by(HOST_PAGE_SIZE).enumerate() {
            while extents.next_if(|r| r.end <= page).is_some() {}
            let in_image = extents.peek().is_some_and(|r| r.start <= page);
            let data = may_hold_data(&vec[i * per..(i + 1) * per], in_image);
            let keep = (resident != 0 || data) && {
                // SAFETY: a host page inside the range (read only if it
                // may hold data, so untouched memory is not faulted in).
                let bytes = unsafe { std::slice::from_raw_parts(host.add(page), HOST_PAGE_SIZE) };
                // SAFETY: any bit pattern is a u64.
                let (head, words, tail) = unsafe { bytes.align_to::<u64>() };
                words.iter().any(|&w| w != 0) || head.iter().chain(tail).any(|&b| b != 0)
            };
            match (keep, run) {
                (true, None) => run = Some(page),
                (false, Some(start)) => {
                    flush(start, page, &mut written)?;
                    run = None;
                }
                _ => {}
            }
        }
    }
    if let Some(start) = run {
        flush(start, len, &mut written)?;
    }
    add_hole(&mut holes, data_end..len as u64);
    file.settle_holes(&holes)?;
    Ok(written)
}

/// Reads the data in `path` (not its holes) into `[host, host + len)`;
/// returns the bytes read.
///
/// # Safety
/// `[host, host + len)` is mapped, writable, and nothing else uses it.
pub unsafe fn copy_ram(path: &Path, host: *mut u8, len: usize) -> io::Result<u64> {
    let file = File::open(path)?;
    if file.metadata()?.len() != len as u64 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "ram.img is not the size of guest RAM",
        ));
    }
    let fd = file.as_raw_fd();
    let mut read = 0u64;
    let mut off = 0i64;
    loop {
        // SAFETY: plain lseek on our descriptor.
        let data = unsafe { libc::lseek(fd, off, libc::SEEK_DATA) };
        if data < 0 {
            let e = io::Error::last_os_error();
            return if e.raw_os_error() == Some(libc::ENXIO) {
                Ok(read)
            } else {
                Err(e)
            };
        }
        // SAFETY: as above.
        let hole = unsafe { libc::lseek(fd, data, libc::SEEK_HOLE) };
        let end = if hole < 0 { len as i64 } else { hole };
        // SAFETY: `[data, end)` lies inside guest RAM (the file is its size).
        let dst = unsafe {
            std::slice::from_raw_parts_mut(host.add(data as usize), (end - data) as usize)
        };
        file.read_exact_at(dst, data as u64)?;
        read += (end - data) as u64;
        off = end;
    }
}

/// Maps `path` copy-on-write over `[host, host + len)`; returns where the
/// file has data, for the next [`write_ram`].
///
/// # Safety
/// The range is guest RAM's host mapping, unmapped from the VM until the
/// caller maps it back, and nothing else uses it meanwhile.
pub unsafe fn map_ram(path: &Path, host: *mut u8, len: usize) -> io::Result<Vec<Range<usize>>> {
    let file = File::open(path)?;
    if file.metadata()?.len() != len as u64 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "ram.img is not the size of guest RAM",
        ));
    }
    // SAFETY: replaces the caller's own range (MAP_FIXED); the mapping
    // keeps the file open.
    let p = unsafe {
        libc::mmap(
            host.cast(),
            len,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE | libc::MAP_FIXED,
            file.as_raw_fd(),
            0,
        )
    };
    if p == libc::MAP_FAILED {
        return Err(io::Error::last_os_error());
    }
    data_extents(&file, len)
}

/// The ranges of `file` (of `len` bytes) that hold data.
fn data_extents(file: &File, len: usize) -> io::Result<Vec<Range<usize>>> {
    let fd = file.as_raw_fd();
    let mut out = Vec::new();
    let mut off = 0i64;
    loop {
        // SAFETY: plain lseek on our descriptor.
        let data = unsafe { libc::lseek(fd, off, libc::SEEK_DATA) };
        if data < 0 {
            let e = io::Error::last_os_error();
            return if e.raw_os_error() == Some(libc::ENXIO) {
                Ok(out)
            } else {
                Err(e)
            };
        }
        // SAFETY: as above.
        let hole = unsafe { libc::lseek(fd, data, libc::SEEK_HOLE) };
        let end = if hole < 0 { len as i64 } else { hole };
        out.push(data as usize..end as usize);
        off = end;
    }
}

#[cfg(test)]
mod tests;
