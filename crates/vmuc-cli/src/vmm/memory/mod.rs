// SPDX-License-Identifier: Apache-2.0
//! The memory manager's host side in the VMM process: `vmuc stat`
//! numbers, `reclaim_now`, and the host-pressure responder.
//!
//! Free page reporting runs all the time without any of this. The
//! responder only reacts to macOS memory pressure, by the rules in
//! [`policy`]; the balloon part is off unless the instance sets
//! `memoryPolicy.pressureBalloon`. Its limits exist because looser
//! policies collapsed idle guests and oscillated (feasibility.md).

mod policy;

use std::sync::atomic::Ordering;
use std::sync::{Arc, PoisonError};
use std::time::{Duration, Instant};

use vmuc_proto::{AgentRequest, AgentResponse, PressureLevel, Stat};

use self::policy::{Action, Policy, Round};
use super::{Shared, ready};

const POLL: Duration = Duration::from_secs(5);

/// `kern.memorystatus_vm_pressure_level`: 1 normal, 2 warn, 4 critical.
fn host_pressure() -> PressureLevel {
    match crate::host::sysctl_u32("kern.memorystatus_vm_pressure_level") {
        Some(4) => PressureLevel::Critical,
        Some(2) => PressureLevel::Warn,
        _ => PressureLevel::Normal,
    }
}

/// The level the responder acts on: a simulated one while it lasts, else
/// the host's.
fn pressure(shared: &Shared, now: Instant) -> PressureLevel {
    let simulated = *shared
        .simulated
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    effective(simulated, now, host_pressure)
}

fn effective(
    simulated: Option<(PressureLevel, Instant)>,
    now: Instant,
    host: impl FnOnce() -> PressureLevel,
) -> PressureLevel {
    match simulated {
        Some((level, until)) if now < until => level,
        _ => host(),
    }
}

/// Acts as if the host reported `level` for `span` (`vmuc _pressure`).
pub fn simulate(shared: &Shared, level: PressureLevel, span: Duration) {
    tracing::info!(?level, ?span, "simulated host pressure");
    *shared
        .simulated
        .lock()
        .unwrap_or_else(PoisonError::into_inner) = Some((level, Instant::now() + span));
}

/// This process's `phys_footprint`: what macOS bills the VM for. (0 on
/// other hosts, where the VMM only builds for checks and tests.)
#[cfg(not(target_os = "macos"))]
pub fn host_footprint() -> u64 {
    0
}

/// This process's `phys_footprint`: what macOS bills the VM for.
#[cfg(target_os = "macos")]
pub fn host_footprint() -> u64 {
    // SAFETY: a zeroed rusage_info_v4 is a valid out-buffer for flavor V4.
    let mut info: libc::rusage_info_v4 = unsafe { std::mem::zeroed() };
    // SAFETY: valid pid and a buffer of the flavor's type.
    let rc = unsafe {
        libc::proc_pid_rusage(libc::getpid(), libc::RUSAGE_INFO_V4, (&raw mut info).cast())
    };
    if rc == 0 { info.ri_phys_footprint } else { 0 }
}

/// This process's user + system CPU time, nanoseconds. (getrusage, not
/// proc_pid_rusage, whose times are Mach ticks on Apple silicon.)
fn cpu_time() -> u64 {
    // SAFETY: a zeroed rusage is a valid out-buffer.
    let mut ru: libc::rusage = unsafe { std::mem::zeroed() };
    // SAFETY: valid out-buffer for RUSAGE_SELF.
    if unsafe { libc::getrusage(libc::RUSAGE_SELF, &raw mut ru) } != 0 {
        return 0;
    }
    let ns = |t: libc::timeval| t.tv_sec as u64 * 1_000_000_000 + t.tv_usec as u64 * 1000;
    ns(ru.ru_utime) + ns(ru.ru_stime)
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
        host_cpu_nanos: cpu_time(),
        net_rx_bytes: shared
            .net
            .as_ref()
            .map_or(0, |n| n.rx_bytes.load(Ordering::Relaxed)),
        net_tx_bytes: shared
            .net
            .as_ref()
            .map_or(0, |n| n.tx_bytes.load(Ordering::Relaxed)),
        net_dropped: shared
            .net
            .as_ref()
            .map_or(0, |n| n.dropped.load(Ordering::Relaxed)),
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

pub fn spawn_responder(shared: Arc<Shared>) {
    let _ = std::thread::Builder::new()
        .name("pressure".into())
        .spawn(move || respond(&shared));
}

fn respond(shared: &Shared) {
    let mut policy = Policy::new(
        shared.pressure_balloon,
        shared.min_available,
        Instant::now(),
    );
    while !shared.machine.stopping() {
        std::thread::sleep(POLL);
        let now = Instant::now();
        let balloon = shared.balloon.actual();
        let round = Round {
            now,
            level: pressure(shared, now),
            last_reclaim: *shared
                .last_reclaim
                .lock()
                .unwrap_or_else(PoisonError::into_inner),
            balloon,
            available: shared.balloon.stats().map(|(_, s)| s.available),
            stall: (balloon > 0).then(|| guest_stall(shared)).flatten(),
        };
        for action in policy.step(&round) {
            match action {
                Action::Reclaim => {
                    tracing::info!(level = ?round.level, "host memory pressure; asking the guest to drop caches");
                    if let Err(e) = reclaim_now(shared) {
                        tracing::warn!("reclaim on pressure: {e}");
                    }
                }
                Action::Balloon(target, why) => {
                    tracing::info!(target, ?why, balloon, "balloon target");
                    shared.balloon.set_target(target);
                }
            }
        }
    }
}

/// The guest's memory PSI `some` over the last 10 s, percent.
fn guest_stall(shared: &Shared) -> Option<f32> {
    match ready::agent_call(&shared.vsock, &AgentRequest::Psi, Duration::from_secs(2)) {
        Ok(AgentResponse::Psi(p)) => Some(p.some_avg10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_simulated_level_holds_until_it_expires() {
        let t0 = Instant::now();
        let later = t0 + Duration::from_secs(10);
        let sim = Some((PressureLevel::Critical, later));
        let host = || PressureLevel::Warn;
        assert_eq!(effective(sim, t0, host), PressureLevel::Critical);
        assert_eq!(effective(sim, later, host), PressureLevel::Warn);
        assert_eq!(effective(None, t0, host), PressureLevel::Warn);
        // Simulating `normal` hides real pressure while it lasts.
        let calm = Some((PressureLevel::Normal, later));
        assert_eq!(effective(calm, t0, host), PressureLevel::Normal);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn footprint_of_this_process_is_plausible() {
        let f = host_footprint();
        assert!(f > 1 << 20 && f < 64 << 30, "{f}");
    }
}
