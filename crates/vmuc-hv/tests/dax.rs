// SPDX-License-Identifier: Apache-2.0
//! virtiofs DAX on Hypervisor.framework (experiment E2): host files
//! mapped straight into the guest's physical address space.
//!
//! With DAX the guest reads and writes a shared file's pages through a
//! window in its physical address space instead of copying them into its
//! page cache; the VMM maps each file range it is asked for with
//! `hv_vm_map`. This probe checks, with a real vCPU, what that rests on:
//! that a `MAP_SHARED` file mapping works as guest memory in both
//! directions, what it bills, which ways of reusing a window slot the
//! framework accepts (`hv_vm_map` refuses overlapping maps), what a guest
//! load past a host truncation does, and how much a map/unmap pair costs.
//!
//! `cargo test -p vmuc-hv --test dax -- --nocapture`

#![cfg(all(target_os = "macos", target_arch = "aarch64"))]
#![allow(clippy::cast_precision_loss, clippy::cast_ptr_alignment)]

use std::fs::File;
use std::io::Write;
use std::os::fd::AsRawFd;
use std::os::unix::fs::FileExt;
use std::thread;
use std::time::Instant;

use vmuc_hv::{HvError, HvVcpu, HvVm, MemoryPermission, reg, sys_reg};

const CODE_IPA: u64 = 0x4000_0000;
const CODE_SIZE: usize = 0x4000;
/// The DAX window, far from anything else.
const WIN: u64 = 0x8000_0000;
const CHUNK: usize = 2 << 20;
const MIB: f64 = 1024.0 * 1024.0;

/// x0 = start, x1 = end, x2 = stride: load every page, then HVC.
const LOAD_LOOP: [u32; 5] = [
    0xf940_0003,
    0x8b02_0000,
    0xeb01_001f,
    0x54ff_ffa3,
    0xd400_0002,
];
/// Store x2 to every page from x0 to x1 (stride x2), then HVC.
const STORE_LOOP: [u32; 5] = [
    0xf900_0002,
    0x8b02_0000,
    0xeb01_001f,
    0x54ff_ffa3,
    0xd400_0002,
];
/// `ldr x3, [x0]; hvc #0`: one load, result in x3.
const LOAD_ONE: [u32; 2] = [0xf940_0003, 0xd400_0002];
const STORE_AT: usize = 0x100;
const ONE_AT: usize = 0x200;

fn footprint() -> u64 {
    // SAFETY: a zeroed rusage_info_v4 is a valid out-buffer for flavor V4.
    let mut info: libc::rusage_info_v4 = unsafe { std::mem::zeroed() };
    // SAFETY: valid pid and buffer of the flavor's type.
    let rc = unsafe {
        libc::proc_pid_rusage(libc::getpid(), libc::RUSAGE_INFO_V4, (&raw mut info).cast())
    };
    assert_eq!(rc, 0);
    info.ri_phys_footprint
}

/// A scratch file of `len` bytes, each 4 KiB page starting with
/// `tag + its index`.
fn scratch(name: &str, len: usize, tag: u64) -> (File, std::path::PathBuf) {
    let path = std::env::temp_dir().join(format!("vmuc-dax-{}-{name}", std::process::id()));
    let mut f = File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(&path)
        .unwrap();
    let mut buf = vec![0u8; len];
    for (i, page) in buf.chunks_mut(4096).enumerate() {
        page[..8].copy_from_slice(&(tag + i as u64).to_le_bytes());
    }
    f.write_all(&buf).unwrap();
    f.sync_all().unwrap();
    (f, path)
}

/// `MAP_SHARED` read/write mapping of all of `f`.
fn map_shared(f: &File, len: usize) -> *mut u8 {
    // SAFETY: a fresh shared mapping of a file we own, checked below.
    let p = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            len,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_SHARED,
            f.as_raw_fd(),
            0,
        )
    };
    assert_ne!(p, libc::MAP_FAILED);
    p.cast()
}

/// Runs code at `pc` with x0..x2 on a fresh vCPU; returns x3.
fn run_at(pc: usize, x0: u64, x1: u64, x2: u64) -> Result<u64, HvError> {
    thread::spawn(move || -> Result<u64, HvError> {
        let vcpu = HvVcpu::new()?;
        vcpu.set_sys_reg(sys_reg::HV_SYS_REG_MPIDR_EL1, 0)?;
        vcpu.set_reg(reg::HV_REG_PC, CODE_IPA + pc as u64)?;
        vcpu.set_reg(reg::HV_REG_CPSR, 0x3c5)?;
        vcpu.set_reg(reg::HV_REG_X0, x0)?;
        vcpu.set_reg(reg::HV_REG_X1, x1)?;
        vcpu.set_reg(reg::HV_REG_X2, x2)?;
        let exit = vcpu.run()?;
        assert!(format!("{exit:?}").contains("HypercallHvc"), "{exit:?}");
        vcpu.get_reg(reg::HV_REG_X3)
    })
    .join()
    .expect("vCPU thread panicked")
}

fn load_one(ipa: u64) -> Result<u64, HvError> {
    run_at(ONE_AT, ipa, 0, 0)
}

fn map(host: *mut u8, ipa: u64, len: usize) -> Result<(), HvError> {
    // SAFETY: `host..host+len` is a live mapping of ours for the call.
    unsafe { HvVm::remap(host, ipa, len, MemoryPermission::ALL) }
}

/// A scratch file mapped `MAP_SHARED`, removed on drop.
struct Scratch {
    file: File,
    path: std::path::PathBuf,
    ptr: *mut u8,
    len: usize,
}

impl Scratch {
    fn new(name: &str, len: usize, tag: u64) -> Self {
        let (file, path) = scratch(name, len, tag);
        let ptr = map_shared(&file, len);
        Self {
            file,
            path,
            ptr,
            len,
        }
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        // SAFETY: our own mapping, no longer mapped into the VM.
        unsafe { libc::munmap(self.ptr.cast(), self.len) };
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Guest loads and stores through a 64 MiB file mapping: MiB billed for
/// each, and coherence both ways.
fn coherence_and_billing() -> (f64, f64) {
    let len = 64 << 20;
    let file = Scratch::new("a", len, 0);
    map(file.ptr, WIN, len).unwrap();
    let base = footprint();
    run_at(0, WIN, WIN + len as u64, 4096).unwrap();
    let read_billed = (footprint() as f64 - base as f64) / MIB;
    assert_eq!(
        load_one(WIN + 4096 * 77).unwrap(),
        77,
        "guest sees the file"
    );
    file.file
        .write_all_at(&0xfeed_u64.to_le_bytes(), 4096 * 5)
        .unwrap();
    assert_eq!(
        load_one(WIN + 4096 * 5).unwrap(),
        0xfeed,
        "guest sees a host write at once"
    );
    let base = footprint();
    run_at(STORE_AT, WIN, WIN + len as u64, 4096).unwrap();
    let write_billed = (footprint() as f64 - base as f64) / MIB;
    let mut word = [0u8; 8];
    file.file.read_exact_at(&mut word, 4096 * 9).unwrap();
    assert_eq!(
        u64::from_le_bytes(word),
        4096,
        "host sees a guest write at once"
    );
    HvVm::unmap(WIN, len).unwrap();
    (read_billed, write_billed)
}

/// Ways of putting a different host range where another was mapped.
fn slot_reuse() {
    let first = Scratch::new("first", 2 * CHUNK, 0);
    let second = Scratch::new("second", 2 * CHUNK, 1_000_000);
    let try_map = |host, ipa, n| map(host, ipa, n).map_err(|e| format!("{e:?}"));
    // Map, unmap, map another file there.
    map(first.ptr, WIN, CHUNK).unwrap();
    HvVm::unmap(WIN, CHUNK).unwrap();
    let reuse = try_map(second.ptr, WIN, CHUNK);
    let reuse_reads = reuse.is_ok().then(|| load_one(WIN).unwrap());
    HvVm::unmap(WIN, CHUNK).ok();
    // Map 4 MiB of one file, unmap half, map another file into the hole.
    map(first.ptr, WIN, 2 * CHUNK).unwrap();
    HvVm::unmap(WIN, CHUNK).unwrap();
    let hole = try_map(second.ptr, WIN, CHUNK);
    let hole_reads = hole.is_ok().then(|| load_one(WIN).unwrap());
    HvVm::unmap(WIN, 2 * CHUNK).ok();
    // The same with anonymous memory, which AGENTS.md said took only the
    // same host address back.
    let anon = |n: usize| {
        // SAFETY: a fresh anonymous mapping, checked below.
        let p = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                n,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_ANON | libc::MAP_PRIVATE,
                -1,
                0,
            )
        };
        assert_ne!(p, libc::MAP_FAILED);
        p.cast::<u8>()
    };
    let (big, small) = (anon(2 * CHUNK), anon(CHUNK));
    // SAFETY: in bounds of `small`.
    unsafe { small.cast::<u64>().write(42) };
    map(big, WIN, 2 * CHUNK).unwrap();
    HvVm::unmap(WIN, CHUNK).unwrap();
    let anon_hole = try_map(small, WIN, CHUNK);
    let anon_reads = anon_hole.is_ok().then(|| load_one(WIN).unwrap());
    HvVm::unmap(WIN, 2 * CHUNK).ok();
    // SAFETY: our own mappings, no longer mapped into the VM.
    unsafe {
        libc::munmap(big.cast(), 2 * CHUNK);
        libc::munmap(small.cast(), CHUNK);
    }
    // Map over a live mapping.
    map(first.ptr, WIN, CHUNK).unwrap();
    let over = try_map(second.ptr, WIN, CHUNK);
    HvVm::unmap(WIN, CHUNK).ok();
    eprintln!("map, unmap, map another file:          {reuse:?}, reads {reuse_reads:?}");
    eprintln!("unmap half of a mapping, map another:  {hole:?}, reads {hole_reads:?}");
    eprintln!("anonymous: unmap half, map another:    {anon_hole:?}, reads {anon_reads:?}");
    eprintln!("map another file over a live mapping:  {over:?}");
    assert_eq!(
        reuse_reads,
        Some(1_000_000),
        "a freed slot takes another file"
    );
    assert!(over.is_err(), "hv_vm_map refuses a map over a live mapping");
}

/// What a guest load past a host truncation of a mapped file does.
fn past_eof() -> String {
    let file = Scratch::new("trunc", CHUNK, 0);
    file.file.set_len(0).unwrap();
    map(file.ptr, WIN, CHUNK).unwrap();
    let exit = thread::spawn(|| {
        let vcpu = HvVcpu::new().unwrap();
        vcpu.set_sys_reg(sys_reg::HV_SYS_REG_MPIDR_EL1, 0).unwrap();
        vcpu.set_reg(reg::HV_REG_PC, CODE_IPA + ONE_AT as u64)
            .unwrap();
        vcpu.set_reg(reg::HV_REG_CPSR, 0x3c5).unwrap();
        vcpu.set_reg(reg::HV_REG_X0, WIN + 4096).unwrap();
        format!("{:?}", vcpu.run().unwrap())
    })
    .join()
    .unwrap();
    HvVm::unmap(WIN, CHUNK).unwrap();
    exit
}

/// Average cost of mapping and unmapping 2 MiB of a file.
fn map_cost() -> std::time::Duration {
    let file = Scratch::new("cost", 32 * CHUNK, 0);
    let n = 2000;
    let t = Instant::now();
    for i in 0..n {
        let ipa = WIN + ((i % 16) * CHUNK) as u64;
        // SAFETY: inside the file mapping.
        map(unsafe { file.ptr.add((i % 32) * CHUNK) }, ipa, CHUNK).unwrap();
        HvVm::unmap(ipa, CHUNK).unwrap();
    }
    t.elapsed() / n as u32
}

#[test]
fn files_map_into_the_guest() {
    let vm = match HvVm::new() {
        Err(HvError::Unsupported | HvError::Denied) => {
            eprintln!("Hypervisor.framework unavailable here; skipped");
            return;
        }
        other => other.unwrap(),
    };
    // SAFETY: an anonymous code page holding the three programs.
    let code = unsafe {
        let p = libc::mmap(
            std::ptr::null_mut(),
            CODE_SIZE,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_ANON | libc::MAP_PRIVATE,
            -1,
            0,
        )
        .cast::<u8>();
        for (at, prog) in [
            (0, &LOAD_LOOP[..]),
            (STORE_AT, &STORE_LOOP[..]),
            (ONE_AT, &LOAD_ONE[..]),
        ] {
            for (i, insn) in prog.iter().enumerate() {
                p.add(at).cast::<u32>().add(i).write(*insn);
            }
        }
        vm.map_memory(p, CODE_IPA, CODE_SIZE, MemoryPermission::ALL)
            .unwrap();
        p
    };
    let (read_billed, write_billed) = coherence_and_billing();
    eprintln!(
        "64 MiB file in the guest: loads billed {read_billed:.1} MiB, stores {write_billed:.1} MiB more"
    );
    assert!(
        read_billed + write_billed < 8.0,
        "file pages are billed to the VMM"
    );
    slot_reuse();
    let exit = past_eof();
    eprintln!("guest load past a host truncation: {exit}");
    assert!(
        exit.contains("DataAbort"),
        "a load past EOF should be a data abort"
    );
    eprintln!("2 MiB map + unmap: {:?} per pair", map_cost());
    drop(vm);
    // SAFETY: the VM is gone; nothing maps the code page any more.
    unsafe { libc::munmap(code.cast(), CODE_SIZE) };
}
