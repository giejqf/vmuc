// SPDX-License-Identifier: Apache-2.0
//! The host-pressure policy as a state machine: the responder thread
//! gathers one [`Round`] of inputs every few seconds and applies the
//! [`Action`]s that come back. Nothing here touches the VM, so every rule
//! is unit-tested, including the failure modes it must never reproduce.
//!
//! The rules (plan.md, "Memory manager"):
//! - warn or critical: at most every [`RECLAIM_EVERY`], ask the guest to
//!   reclaim a share of its reclaimable memory (a quarter on warn, half
//!   on critical) through its own LRU, coldest first, and compact;
//! - critical, with `pressureBalloon`: inflate by at most a quarter of the
//!   guest's available memory, never leaving less than `minAvailable`, at
//!   most every [`BALLOON_EVERY`];
//! - deflate fully once the host has been calm for
//!   [`CALM_BEFORE_DEFLATE`], or at once when the guest stalls on memory
//!   (PSI `some` over [`STALL_PERCENT`] in the last 10 s); after a stall,
//!   inflate again only once the host has been calm, or the balloon and
//!   the guest's stalls take turns (E1, the oscillation ArcBox saw).

use std::time::{Duration, Instant};

use vmuc_proto::PressureLevel;

pub const RECLAIM_EVERY: Duration = Duration::from_secs(30);
pub const BALLOON_EVERY: Duration = Duration::from_secs(60);
pub const CALM_BEFORE_DEFLATE: Duration = Duration::from_secs(120);
/// Guest memory PSI `some` (avg10, percent) above which the balloon goes.
pub const STALL_PERCENT: f32 = 10.0;
/// Smaller trims are not worth a round trip.
const MIN_TRIM: u64 = 16 << 20;

/// What the responder saw this round.
#[derive(Debug, Clone, Copy)]
pub struct Round {
    pub now: Instant,
    pub level: PressureLevel,
    /// When the guest was last asked to drop caches (by anyone).
    pub last_reclaim: Option<Instant>,
    /// Bytes the balloon holds.
    pub balloon: u64,
    /// The guest's available memory, when its stats have arrived.
    pub available: Option<u64>,
    /// What of it the guest could reclaim (available minus free).
    pub reclaimable: Option<u64>,
    /// Guest memory PSI `some` avg10; only fetched while the balloon
    /// holds memory.
    pub stall: Option<f32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Reclaim this many bytes of the guest's coldest memory, and compact.
    Trim(u64),
    /// Set the balloon target, in bytes.
    Balloon(u64, Why),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Why {
    Critical,
    Calm,
    Stalling,
}

#[derive(Debug)]
pub struct Policy {
    pressure_balloon: bool,
    min_available: u64,
    last_balloon: Option<Instant>,
    calm_since: Instant,
    /// The guest stalled under the balloon during this bout of pressure.
    stalled: bool,
}

impl Policy {
    pub fn new(pressure_balloon: bool, min_available: u64, now: Instant) -> Self {
        Self {
            pressure_balloon,
            min_available,
            last_balloon: None,
            calm_since: now,
            stalled: false,
        }
    }

    pub fn step(&mut self, r: &Round) -> Vec<Action> {
        let mut out = Vec::new();
        if r.balloon > 0 && r.stall.is_some_and(|s| s > STALL_PERCENT) {
            self.last_balloon = Some(r.now);
            self.stalled = true;
            out.push(Action::Balloon(0, Why::Stalling));
            return out;
        }
        if r.level == PressureLevel::Normal {
            self.stalled = false;
            if r.balloon > 0 && r.now.duration_since(self.calm_since) >= CALM_BEFORE_DEFLATE {
                out.push(Action::Balloon(0, Why::Calm));
            }
            return out;
        }
        self.calm_since = r.now;
        let share = if r.level == PressureLevel::Critical {
            2
        } else {
            4
        };
        let trim = r.reclaimable.unwrap_or(0) / share;
        let due = r
            .last_reclaim
            .is_none_or(|t| r.now.duration_since(t) >= RECLAIM_EVERY);
        if due && trim >= MIN_TRIM {
            out.push(Action::Trim(trim));
        }
        let may_balloon = self.pressure_balloon
            && !self.stalled
            && r.level == PressureLevel::Critical
            && self
                .last_balloon
                .is_none_or(|t| r.now.duration_since(t) >= BALLOON_EVERY);
        if may_balloon && let Some(available) = r.available {
            let target = pressure_target(r.balloon, available, self.min_available);
            if target > r.balloon {
                self.last_balloon = Some(r.now);
                out.push(Action::Balloon(target, Why::Critical));
            }
        }
        out
    }
}

/// The balloon size the pressure rule asks for, given what the guest has
/// available: at most a quarter of it, never leaving less than `floor`.
pub fn pressure_target(current: u64, available: u64, floor: u64) -> u64 {
    let step = available / 4;
    let room = available.saturating_sub(floor);
    current + step.min(room)
}

#[cfg(test)]
mod tests;
