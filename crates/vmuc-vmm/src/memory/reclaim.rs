// SPDX-License-Identifier: Apache-2.0
//! Returning guest memory to macOS.
//!
//! The only sequence that gives back pages a vCPU has dirtied is
//! `hv_vm_unmap` → fresh memory over the same host address → `hv_vm_map`
//! (`docs/experiments/2026-10-11-stage2-release-matrix.md`). Guest RAM is
//! ledger-tagged granules, which are billed honestly but freed only whole,
//! so the fresh memory is new granules and ranges are aligned inward to
//! [`RELEASE_GRANULE`] (`docs/experiments/2026-10-13-guest-ram-accounting.md`).
//! Ranges with nothing resident are skipped, and a remap that fails twice
//! leaves a hole in guest RAM: the VM is stopped and the range never
//! acknowledged.
//! Reclaim runs on the balloon's worker thread, never on a vCPU.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, PoisonError, RwLock};
use std::time::Instant;

use vmuc_hv::guest_memory::{self, RELEASE_GRANULE};
use vmuc_hv::{HOST_PAGE_SIZE, HvVm, MemoryPermission, align_inward_to};

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ReclaimError {
    #[error("range {gpa:#x}+{len:#x} is outside guest RAM")]
    OutOfRange { gpa: u64, len: u64 },
    #[error("the VM is shutting down")]
    Stopped,
    #[error("remapping {gpa:#x}+{len:#x} failed twice ({why}); guest RAM has a hole")]
    Hole { gpa: u64, len: u64, why: String },
}

/// Release latency buckets, microseconds (upper bounds).
const BUCKETS_US: [u64; 10] = [10, 25, 50, 100, 250, 500, 1000, 2500, 5000, u64::MAX];

/// Counters behind `vmuc stat`.
#[derive(Debug, Default)]
pub struct ReclaimStats {
    /// Bytes the guest reported or ballooned.
    pub reported: AtomicU64,
    /// Bytes given back to macOS.
    pub released: AtomicU64,
    /// Bytes skipped because nothing in them was resident.
    pub not_resident: AtomicU64,
    latency: [AtomicU64; BUCKETS_US.len()],
}

impl ReclaimStats {
    fn record(&self, us: u64) {
        let i = BUCKETS_US
            .iter()
            .position(|&b| us <= b)
            .unwrap_or(BUCKETS_US.len() - 1);
        self.latency[i].fetch_add(1, Ordering::Relaxed);
    }

    /// The bucket bound below which `per_mille`/1000 of releases
    /// finished, in microseconds (500 = p50, 990 = p99).
    pub fn latency_quantile_us(&self, per_mille: u64) -> u32 {
        let counts: Vec<u64> = self
            .latency
            .iter()
            .map(|c| c.load(Ordering::Relaxed))
            .collect();
        let total: u64 = counts.iter().sum();
        if total == 0 {
            return 0;
        }
        let want = (total * per_mille).div_ceil(1000);
        let mut seen = 0;
        for (i, c) in counts.iter().enumerate() {
            seen += c;
            if seen >= want {
                return BUCKETS_US[i].min(u64::from(u32::MAX)) as u32;
            }
        }
        u32::MAX
    }
}

/// What a release has to do to the host and the VM; the real one calls
/// mmap and Hypervisor.framework, tests record the calls.
///
/// # Safety
/// For every method taking `host`, `[host, host + len)` must be part of
/// the guest RAM mapping that backs the IPA range in question.
pub unsafe trait Stage2: Send + Sync {
    /// # Safety
    /// See the trait.
    unsafe fn resident(&self, host: *mut u8, len: usize) -> bool;
    fn unmap(&self, gpa: u64, len: usize) -> Result<(), String>;
    /// Replaces `[host, host+len)` with fresh zero pages.
    ///
    /// # Safety
    /// See the trait; the range must also be unmapped from the VM.
    unsafe fn refresh(&self, host: *mut u8, len: usize) -> Result<(), String>;
    /// # Safety
    /// See the trait.
    unsafe fn map(&self, host: *mut u8, gpa: u64, len: usize) -> Result<(), String>;
}

/// Hypervisor.framework and tagged guest memory.
#[derive(Debug, Default)]
pub struct HvStage2;

// SAFETY: every method passes `host` straight to mincore/mmap/hv_vm_map
// under the trait's contract.
unsafe impl Stage2 for HvStage2 {
    unsafe fn resident(&self, host: *mut u8, len: usize) -> bool {
        let mut vec = vec![0u8; len.div_ceil(HOST_PAGE_SIZE)];
        // SAFETY: `host..host+len` is our guest RAM mapping; `vec` has one
        // byte per host page.
        let rc = unsafe { libc::mincore(host.cast(), len, vec.as_mut_ptr().cast()) };
        rc != 0 || vec.iter().any(|&b| b & 1 != 0)
    }

    fn unmap(&self, gpa: u64, len: usize) -> Result<(), String> {
        HvVm::unmap(gpa, len).map_err(|e| e.to_string())
    }

    unsafe fn refresh(&self, host: *mut u8, len: usize) -> Result<(), String> {
        // SAFETY: whole granules of our own guest RAM; the range is
        // unmapped from the VM, so no vCPU can touch it.
        unsafe { guest_memory::refresh(host, len) }.map_err(|e| e.to_string())
    }

    unsafe fn map(&self, host: *mut u8, gpa: u64, len: usize) -> Result<(), String> {
        // SAFETY: the same host address that backed this IPA range before,
        // which stays mapped for the VM's lifetime.
        unsafe { HvVm::remap(host, gpa, len, MemoryPermission::ALL) }.map_err(|e| e.to_string())
    }
}

pub struct Reclaimer {
    host: *mut u8,
    base: u64,
    len: u64,
    /// `true` while the VM exists; the VM takes the write lock before
    /// `hv_vm_destroy`, so no release can be half done when it goes.
    gate: Arc<RwLock<bool>>,
    stats: Arc<ReclaimStats>,
    stage2: Box<dyn Stage2>,
}

// SAFETY: `host` points into guest RAM, which outlives the reclaimer (the
// VM closes the gate before it unmaps RAM); every access goes through
// mmap/mincore/hv calls that are safe from any thread.
unsafe impl Send for Reclaimer {}
// SAFETY: as above; `&self` methods only read the fields.
unsafe impl Sync for Reclaimer {}

impl std::fmt::Debug for Reclaimer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Reclaimer")
            .field("base", &format_args!("{:#x}", self.base))
            .field("len", &format_args!("{:#x}", self.len))
            .finish_non_exhaustive()
    }
}

impl Reclaimer {
    /// # Safety
    /// `[host, host + len)` must be the host mapping behind guest RAM at
    /// `base`, alive until `gate` is closed.
    pub unsafe fn new(
        host: *mut u8,
        base: u64,
        len: u64,
        gate: Arc<RwLock<bool>>,
        stats: Arc<ReclaimStats>,
        stage2: Box<dyn Stage2>,
    ) -> Self {
        Self {
            host,
            base,
            len,
            gate,
            stats,
            stage2,
        }
    }

    pub fn stats(&self) -> &Arc<ReclaimStats> {
        &self.stats
    }

    /// Gives `[gpa, gpa + len)` back to macOS; returns the bytes released
    /// (0 when no whole granule fits or nothing was resident).
    pub fn release(&self, gpa: u64, len: u64) -> Result<u64, ReclaimError> {
        self.stats.reported.fetch_add(len, Ordering::Relaxed);
        let in_ram = gpa >= self.base
            && gpa
                .checked_add(len)
                .is_some_and(|end| end <= self.base + self.len);
        if !in_ram {
            return Err(ReclaimError::OutOfRange { gpa, len });
        }
        let Some((gpa, len)) = align_inward_to(gpa, len, RELEASE_GRANULE as u64) else {
            return Ok(0);
        };
        let open = self.gate.read().unwrap_or_else(PoisonError::into_inner);
        if !*open {
            return Err(ReclaimError::Stopped);
        }
        // SAFETY: in bounds (checked above).
        let host = unsafe { self.host.add((gpa - self.base) as usize) };
        let size = len as usize;
        // SAFETY (all stage2 calls below): `host..host+size` is the part of
        // guest RAM's mapping behind `gpa..gpa+size`, and it is unmapped
        // from the VM before the refresh.
        if !unsafe { self.stage2.resident(host, size) } {
            self.stats.not_resident.fetch_add(len, Ordering::Relaxed);
            return Ok(0);
        }
        let started = Instant::now();
        if let Err(e) = self.stage2.unmap(gpa, size) {
            // Nothing changed; the pages stay billed but the guest is fine.
            tracing::warn!(gpa = format_args!("{gpa:#x}"), len, "hv_vm_unmap: {e}");
            return Ok(0);
        }
        let refreshed = unsafe { self.stage2.refresh(host, size) }
            .or_else(|_| unsafe { self.stage2.refresh(host, size) });
        let mapped = unsafe { self.stage2.map(host, gpa, size) }
            .or_else(|_| unsafe { self.stage2.map(host, gpa, size) });
        if let Err(why) = mapped {
            return Err(ReclaimError::Hole { gpa, len, why });
        }
        if let Err(e) = refreshed {
            // Mapped back with the old pages: correct, just not released.
            tracing::warn!(gpa = format_args!("{gpa:#x}"), len, "refresh: {e}");
            return Ok(0);
        }
        self.stats.record(started.elapsed().as_micros() as u64);
        self.stats.released.fetch_add(len, Ordering::Relaxed);
        Ok(len)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    #[derive(Default)]
    struct Fake {
        calls: Mutex<Vec<String>>,
        resident: bool,
        fail_map: u32,
    }

    // SAFETY: the fake never dereferences `host`.
    unsafe impl Stage2 for Arc<Fake> {
        unsafe fn resident(&self, _host: *mut u8, _len: usize) -> bool {
            self.resident
        }
        fn unmap(&self, gpa: u64, len: usize) -> Result<(), String> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("unmap {gpa:#x}+{len:#x}"));
            Ok(())
        }
        unsafe fn refresh(&self, _host: *mut u8, len: usize) -> Result<(), String> {
            self.calls.lock().unwrap().push(format!("refresh {len:#x}"));
            Ok(())
        }
        unsafe fn map(&self, _host: *mut u8, gpa: u64, len: usize) -> Result<(), String> {
            let mut c = self.calls.lock().unwrap();
            c.push(format!("map {gpa:#x}+{len:#x}"));
            let failures = c.iter().filter(|s| s.starts_with("map")).count() as u32;
            if failures <= self.fail_map {
                Err("no".into())
            } else {
                Ok(())
            }
        }
    }

    fn reclaimer(fake: &Arc<Fake>, gate: bool) -> Reclaimer {
        // A fake host pointer: the fake Stage2 never dereferences it.
        let host = std::ptr::without_provenance_mut(0x1_0000_0000);
        // SAFETY: the fake never touches host memory.
        unsafe {
            Reclaimer::new(
                host,
                0x4000_0000,
                1 << 30,
                Arc::new(RwLock::new(gate)),
                Arc::default(),
                Box::new(Arc::clone(fake)),
            )
        }
    }

    #[test]
    fn releases_inward_aligned_ranges_in_order() {
        let fake = Arc::new(Fake {
            resident: true,
            ..Fake::default()
        });
        let r = reclaimer(&fake, true);
        // 2 MiB report starting 4 KiB into a granule: one granule trimmed.
        assert_eq!(
            r.release(0x4000_1000, 2 << 20).unwrap(),
            (2 << 20) - 0x2_0000
        );
        assert_eq!(
            *fake.calls.lock().unwrap(),
            [
                "unmap 0x40020000+0x1e0000",
                "refresh 0x1e0000",
                "map 0x40020000+0x1e0000"
            ]
        );
        assert_eq!(
            r.release(0x4000_0000, 0x1_c000).unwrap(),
            0,
            "no whole granule"
        );
        assert_eq!(r.stats().released.load(Ordering::Relaxed), 0x1e_0000);
        assert_eq!(
            r.stats().reported.load(Ordering::Relaxed),
            (2 << 20) + 0x1_c000
        );
        assert!(r.stats().latency_quantile_us(500) > 0);
    }

    #[test]
    fn skips_what_is_not_resident_and_refuses_outside_ram() {
        let fake = Arc::new(Fake::default());
        let r = reclaimer(&fake, true);
        assert_eq!(r.release(0x4000_0000, 1 << 20).unwrap(), 0);
        assert!(fake.calls.lock().unwrap().is_empty());
        assert_eq!(r.stats().not_resident.load(Ordering::Relaxed), 1 << 20);
        assert!(matches!(
            r.release(0x3fff_0000, 1 << 20),
            Err(ReclaimError::OutOfRange { .. })
        ));
        assert!(matches!(
            r.release(0x7fff_0000, 1 << 20),
            Err(ReclaimError::OutOfRange { .. })
        ));
    }

    #[test]
    fn a_closed_gate_stops_releases() {
        let fake = Arc::new(Fake {
            resident: true,
            ..Fake::default()
        });
        let r = reclaimer(&fake, false);
        assert_eq!(r.release(0x4000_0000, 1 << 20), Err(ReclaimError::Stopped));
        assert!(fake.calls.lock().unwrap().is_empty());
    }

    #[test]
    fn map_is_retried_once_then_reported_as_a_hole() {
        let fake = Arc::new(Fake {
            resident: true,
            fail_map: 1,
            ..Fake::default()
        });
        let r = reclaimer(&fake, true);
        assert_eq!(r.release(0x4000_0000, 1 << 20).unwrap(), 1 << 20);
        let fake = Arc::new(Fake {
            resident: true,
            fail_map: 2,
            ..Fake::default()
        });
        let r = reclaimer(&fake, true);
        assert!(matches!(
            r.release(0x4000_0000, 1 << 20),
            Err(ReclaimError::Hole { .. })
        ));
    }

    #[test]
    fn latency_quantiles_come_from_buckets() {
        let s = ReclaimStats::default();
        for us in [5, 20, 30, 40, 2000] {
            s.record(us);
        }
        assert_eq!(s.latency_quantile_us(500), 50);
        assert_eq!(s.latency_quantile_us(990), 2500);
    }
}
