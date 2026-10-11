// SPDX-License-Identifier: Apache-2.0
//! The memory manager's host side in the VMM process: `vmuc stat`
//! numbers, `reclaim_now`, and the host-pressure responder.
//!
//! Free page reporting runs all the time without any of this. The
//! responder only reacts to macOS memory pressure: on *warn* it asks the
//! guest to drop caches and compact (so freed memory can be reported),
//! at most every 30 s; on *critical*, and only with
//! `memoryPolicy.pressureBalloon: true` (off by default), it inflates the
//! balloon by at most 25 % of the guest's available memory, never below
//! `minAvailable`, at most every 60 s, and deflates after 120 s of calm.
//! Those limits exist because looser policies collapsed idle guests and
//! oscillated (feasibility.md).

use std::sync::atomic::Ordering;
use std::sync::{Arc, PoisonError};
use std::time::{Duration, Instant};

use vmuc_proto::{AgentRequest, AgentResponse, Stat};

use super::{Shared, ready};

const POLL: Duration = Duration::from_secs(5);
const RECLAIM_EVERY: Duration = Duration::from_secs(30);
const BALLOON_EVERY: Duration = Duration::from_secs(60);
const CALM_BEFORE_DEFLATE: Duration = Duration::from_secs(120);

/// `kern.memorystatus_vm_pressure_level`: 1 normal, 2 warn, 4 critical.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Pressure {
    Normal,
    Warn,
    Critical,
}

fn host_pressure() -> Pressure {
    match crate::host::sysctl_u32("kern.memorystatus_vm_pressure_level") {
        Some(4) => Pressure::Critical,
        Some(2) => Pressure::Warn,
        _ => Pressure::Normal,
    }
}

/// This process's `phys_footprint`: what macOS bills the VM for.
pub fn host_footprint() -> u64 {
    // SAFETY: a zeroed rusage_info_v4 is a valid out-buffer for flavor V4.
    let mut info: libc::rusage_info_v4 = unsafe { std::mem::zeroed() };
    // SAFETY: valid pid and a buffer of the flavor's type.
    let rc = unsafe {
        libc::proc_pid_rusage(libc::getpid(), libc::RUSAGE_INFO_V4, (&raw mut info).cast())
    };
    if rc == 0 { info.ri_phys_footprint } else { 0 }
}

pub fn stat(shared: &Shared) -> Stat {
    let r = &shared.reclaim;
    let (guest_total, guest_available) = shared
        .balloon
        .stats()
        .map_or((0, 0), |(_, s)| (s.total, s.available));
    Stat {
        host_footprint: host_footprint(),
        guest_total,
        guest_available,
        reported: r.reported.load(Ordering::Relaxed),
        released: r.released.load(Ordering::Relaxed),
        balloon: shared.balloon.actual(),
        release_p50_us: r.latency_quantile_us(500),
        release_p99_us: r.latency_quantile_us(990),
        // Per-vCPU execution time is not collected yet.
        vcpu_exec_nanos: 0,
    }
}

/// Asks the guest to drop its page cache and compact, so freed memory
/// forms whole 2 MiB runs that free page reporting can hand back.
pub fn reclaim_now(shared: &Shared) -> Result<(), String> {
    *shared
        .last_reclaim
        .lock()
        .unwrap_or_else(PoisonError::into_inner) = Some(Instant::now());
    for req in [AgentRequest::DropCaches, AgentRequest::Compact] {
        match ready::agent_call(&shared.vsock, &req, Duration::from_secs(30)) {
            Ok(AgentResponse::Ok) => {}
            Ok(other) => return Err(format!("{req:?}: {other:?}")),
            Err(e) => return Err(format!("{req:?}: {e}")),
        }
    }
    Ok(())
}

/// The balloon size the pressure rule asks for, given what the guest has
/// available: at most a quarter of it, never leaving less than `floor`.
pub fn pressure_target(current: u64, available: u64, floor: u64) -> u64 {
    let step = available / 4;
    let room = available.saturating_sub(floor);
    current + step.min(room)
}

pub fn spawn_responder(shared: Arc<Shared>) {
    let _ = std::thread::Builder::new()
        .name("pressure".into())
        .spawn(move || respond(&shared));
}

fn respond(shared: &Shared) {
    let mut last_balloon: Option<Instant> = None;
    let mut calm_since = Instant::now();
    while !shared.machine.stopping() {
        std::thread::sleep(POLL);
        let level = host_pressure();
        let now = Instant::now();
        if level == Pressure::Normal {
            let deflate = shared.balloon.actual() > 0
                && now.duration_since(calm_since) >= CALM_BEFORE_DEFLATE;
            if deflate {
                tracing::info!(
                    "host pressure calm for {CALM_BEFORE_DEFLATE:?}; deflating the balloon"
                );
                shared.balloon.set_target(0);
            }
            continue;
        }
        calm_since = now;
        let recent = shared
            .last_reclaim
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_some_and(|t| now.duration_since(t) < RECLAIM_EVERY);
        if !recent {
            tracing::info!(
                ?level,
                "host memory pressure; asking the guest to drop caches"
            );
            if let Err(e) = reclaim_now(shared) {
                tracing::warn!("reclaim on pressure: {e}");
            }
        }
        let may_balloon = shared.pressure_balloon
            && level == Pressure::Critical
            && last_balloon.is_none_or(|t| now.duration_since(t) >= BALLOON_EVERY);
        if may_balloon && let Some((_, s)) = shared.balloon.stats() {
            let target =
                pressure_target(shared.balloon.actual(), s.available, shared.min_available);
            if target > shared.balloon.actual() {
                tracing::info!(target, "critical host pressure; inflating the balloon");
                shared.balloon.set_target(target);
                last_balloon = Some(now);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pressure_target_respects_quarter_and_floor() {
        const GIB: u64 = 1 << 30;
        // 4 GiB available, floor 256 MiB: a quarter, 1 GiB.
        assert_eq!(pressure_target(0, 4 * GIB, GIB / 4), GIB);
        // Close to the floor: only down to it.
        assert_eq!(pressure_target(GIB, GIB / 2, GIB / 4), GIB + GIB / 8);
        // At or below the floor: nothing more.
        assert_eq!(pressure_target(GIB, GIB / 4, GIB / 4), GIB);
        assert_eq!(pressure_target(0, 0, GIB / 4), 0);
    }

    #[test]
    fn footprint_of_this_process_is_plausible() {
        let f = host_footprint();
        assert!(f > 1 << 20 && f < 64 << 30, "{f}");
    }
}
