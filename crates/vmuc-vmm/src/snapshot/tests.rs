// SPDX-License-Identifier: Apache-2.0

use std::os::fd::AsRawFd;

use super::*;

const PAGE: usize = HOST_PAGE_SIZE;
const LEN: usize = 128 * PAGE;

/// Guest-RAM-like memory with pages 0, 5, 6 and 100 holding data and
/// page 7 touched but zero.
fn ram() -> *mut u8 {
    let p = vmuc_hv::guest_memory::allocate(LEN).unwrap();
    for (page, byte) in [(0, 1u8), (5, 2), (6, 3), (100, 4)] {
        // SAFETY: inside the allocation.
        unsafe { p.add(page * PAGE + 17).write(byte) };
    }
    // SAFETY: as above.
    unsafe { p.add(7 * PAGE).write(0) };
    p
}

fn bytes(p: *const u8) -> &'static [u8] {
    // SAFETY: a live allocation of LEN bytes (leaked by the tests).
    unsafe { std::slice::from_raw_parts(p, LEN) }
}

/// The first offset where two RAM images differ.
fn first_difference(a: *const u8, b: *const u8) -> Option<usize> {
    bytes(a).iter().zip(bytes(b)).position(|(x, y)| x != y)
}

#[test]
fn only_pages_with_data_are_written_and_both_ways_back_agree() {
    let dir = tempfile::tempdir().unwrap();
    let image = dir.path().join(RAM_FILE);
    let src = ram();
    // SAFETY: our allocation, nothing else writes it.
    let written = unsafe { write_ram(src, LEN, &image, &[]) }.unwrap();
    assert_eq!(written, 4 * PAGE as u64, "pages 0, 5-6 and 100");
    assert_eq!(std::fs::metadata(&image).unwrap().len(), LEN as u64);

    let copy = vmuc_hv::guest_memory::allocate(LEN).unwrap();
    // SAFETY: a fresh allocation of LEN bytes.
    let read = unsafe { copy_ram(&image, copy, LEN) }.unwrap();
    assert!(read >= written, "holes may be coarser than pages");
    assert_eq!(first_difference(copy, src), None);

    let mapped = vmuc_hv::guest_memory::allocate(LEN).unwrap();
    // SAFETY: as above.
    unsafe { map_ram(&image, mapped, LEN) }.unwrap();
    assert_eq!(first_difference(mapped, src), None);
    // SAFETY: inside the mapping, which is private.
    unsafe { mapped.add(100 * PAGE + 17).write(9) };
    // A new snapshot to the same place leaves the mapped image alone.
    let other = vmuc_hv::guest_memory::allocate(LEN).unwrap();
    // SAFETY: a fresh allocation, all zero.
    unsafe { write_ram(other, LEN, &image, &[]) }.unwrap();
    // SAFETY: inside the mapping.
    assert_eq!(unsafe { mapped.add(5 * PAGE + 17).read() }, 2);
    let mut b = [0u8];
    File::open(&image)
        .unwrap()
        .read_exact_at(&mut b, (100 * PAGE + 17) as u64)
        .unwrap();
    assert_eq!(b, [0], "the new, empty snapshot");
}

/// Drops `path`'s pages from the page cache, as a host under pressure or
/// a reboot would.
fn evict(path: &Path) {
    let f = File::open(path).unwrap();
    #[cfg(target_os = "macos")]
    // SAFETY: a shared read-only mapping of our file, invalidated and
    // unmapped at once.
    unsafe {
        let p = libc::mmap(
            std::ptr::null_mut(),
            LEN,
            libc::PROT_READ,
            libc::MAP_SHARED,
            f.as_raw_fd(),
            0,
        );
        assert_ne!(p, libc::MAP_FAILED);
        assert_eq!(libc::msync(p, LEN, libc::MS_INVALIDATE), 0);
        libc::munmap(p, LEN);
    }
    #[cfg(not(target_os = "macos"))]
    // SAFETY: advice on our own descriptor.
    unsafe {
        libc::posix_fadvise(f.as_raw_fd(), 0, 0, libc::POSIX_FADV_DONTNEED);
    }
}

#[test]
fn a_restored_vms_untouched_pages_are_written_again() {
    let dir = tempfile::tempdir().unwrap();
    let (first, second) = (dir.path().join("first.img"), dir.path().join("second.img"));
    let src = ram();
    // SAFETY: our allocation, nothing else writes it.
    unsafe { write_ram(src, LEN, &first, &[]) }.unwrap();
    evict(&first);
    let mapped = vmuc_hv::guest_memory::allocate(LEN).unwrap();
    // SAFETY: a fresh allocation of LEN bytes, used by nothing else.
    let image = unsafe { map_ram(&first, mapped, LEN) }.unwrap();
    assert!(image.iter().any(|r| r.contains(&(100 * PAGE))), "{image:?}");
    // The guest writes one page; the others stay the file's.
    // SAFETY: inside the private mapping.
    unsafe { mapped.add(6 * PAGE + 17).write(8) };
    // SAFETY: the mapping, which nothing else writes.
    let written = unsafe { write_ram(mapped, LEN, &second, &image) }.unwrap();
    assert_eq!(written, 4 * PAGE as u64, "pages 0, 5-6 and 100 again");
    let copy = vmuc_hv::guest_memory::allocate(LEN).unwrap();
    // SAFETY: a fresh allocation of LEN bytes.
    unsafe { copy_ram(&second, copy, LEN) }.unwrap();
    assert_eq!(first_difference(copy, mapped), None);
    // SAFETY: inside the copy.
    assert_eq!(unsafe { copy.add(100 * PAGE + 17).read() }, 4);
}

#[test]
fn state_round_trips_and_other_versions_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    let state = State {
        version: VERSION,
        cpus: 2,
        ram_size: 1 << 30,
        paused_at: 42,
        vcpus: vec![
            Cpu::Running(Regs {
                regs: vec![1; 35],
                simd: vec![[2; 16]; 32],
                sys_regs: vec![(0xc080, 3)],
                icc_regs: vec![(0xc230, 0xf0)],
                vtimer_offset: 5,
                vtimer_masked: false,
            }),
            Cpu::Starting {
                entry: 0x4000_0000,
                context: 7,
            },
        ],
        transports: Vec::new(),
        devices: BTreeMap::from([("pl031".to_owned(), serde_json::json!({"offset": 1}))]),
    };
    state.write(dir.path(), b"gic").unwrap();
    let (back, gic) = State::read(dir.path()).unwrap();
    assert_eq!(gic, b"gic");
    assert_eq!(back.vcpus, state.vcpus);
    assert_eq!(back.devices, state.devices);

    State {
        version: 99,
        ..state
    }
    .write(dir.path(), b"gic")
    .unwrap();
    assert!(State::read(dir.path()).is_err());
}
