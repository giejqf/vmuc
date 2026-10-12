// SPDX-License-Identifier: Apache-2.0
use super::*;

const MIB: u64 = 1 << 20;
const GIB: u64 = 1 << 30;
const POLL: Duration = Duration::from_secs(5);

fn round(now: Instant, level: PressureLevel) -> Round {
    Round {
        now,
        level,
        last_reclaim: None,
        balloon: 0,
        available: Some(3 * GIB),
        stall: None,
    }
}

#[test]
fn pressure_target_respects_quarter_and_floor() {
    // 4 GiB available, floor 256 MiB: a quarter, 1 GiB.
    assert_eq!(pressure_target(0, 4 * GIB, GIB / 4), GIB);
    // Close to the floor: only down to it.
    assert_eq!(pressure_target(GIB, GIB / 2, GIB / 4), GIB + GIB / 8);
    // At or below the floor: nothing more.
    assert_eq!(pressure_target(GIB, GIB / 4, GIB / 4), GIB);
    assert_eq!(pressure_target(0, 0, GIB / 4), 0);
}

#[test]
fn warn_reclaims_at_most_every_30_s_and_never_balloons() {
    let t0 = Instant::now();
    let mut p = Policy::new(true, 256 * MIB, t0);
    let r = round(t0, PressureLevel::Warn);
    assert_eq!(p.step(&r), [Action::Reclaim]);
    let r = Round {
        now: t0 + POLL,
        last_reclaim: Some(t0),
        ..r
    };
    assert_eq!(p.step(&r), []);
    let r = Round {
        now: t0 + RECLAIM_EVERY,
        ..r
    };
    assert_eq!(p.step(&r), [Action::Reclaim]);
}

#[test]
fn critical_balloons_only_when_the_instance_allows_it() {
    let t0 = Instant::now();
    let r = round(t0, PressureLevel::Critical);
    assert_eq!(
        Policy::new(false, 256 * MIB, t0).step(&r),
        [Action::Reclaim]
    );
    assert_eq!(
        Policy::new(true, 256 * MIB, t0).step(&r),
        [Action::Reclaim, Action::Balloon(3 * GIB / 4, Why::Critical)]
    );
}

#[test]
fn calm_deflates_after_120_s_and_pressure_restarts_the_clock() {
    let t0 = Instant::now();
    let mut p = Policy::new(true, 256 * MIB, t0);
    let calm = |now| Round {
        balloon: GIB,
        ..round(now, PressureLevel::Normal)
    };
    p.step(&Round {
        balloon: GIB,
        ..round(t0, PressureLevel::Critical)
    });
    assert_eq!(p.step(&calm(t0 + Duration::from_secs(60))), []);
    // Pressure again at 100 s: the calm clock starts over.
    p.step(&Round {
        balloon: GIB,
        last_reclaim: Some(t0),
        ..round(t0 + Duration::from_secs(100), PressureLevel::Warn)
    });
    assert_eq!(p.step(&calm(t0 + Duration::from_secs(200))), []);
    assert_eq!(
        p.step(&calm(t0 + Duration::from_secs(220))),
        [Action::Balloon(0, Why::Calm)]
    );
}

#[test]
fn a_stalling_guest_gets_its_memory_back_at_any_level() {
    let t0 = Instant::now();
    let mut p = Policy::new(true, 256 * MIB, t0);
    let r = Round {
        balloon: GIB,
        stall: Some(25.0),
        ..round(t0, PressureLevel::Critical)
    };
    assert_eq!(p.step(&r), [Action::Balloon(0, Why::Stalling)]);
    // Below the threshold the balloon stays.
    let r = Round {
        stall: Some(5.0),
        last_reclaim: Some(t0),
        now: t0 + POLL,
        ..r
    };
    assert_eq!(p.step(&r), []);
}

/// ArcBox's incident: a headroom rule drove an idle guest to zero
/// available memory. Thirty minutes of critical pressure on an idle guest
/// (the guest gives the balloon only what it has available) never takes
/// it below the floor.
#[test]
fn half_an_hour_of_critical_pressure_keeps_an_idle_guest_above_its_floor() {
    let floor = 256 * MIB;
    let t0 = Instant::now();
    let mut p = Policy::new(true, floor, t0);
    let (mut balloon, total_free) = (0u64, 3 * GIB);
    let mut t = t0;
    while t < t0 + Duration::from_mins(30) {
        let available = total_free - balloon;
        let r = Round {
            balloon,
            available: Some(available),
            last_reclaim: Some(t),
            ..round(t, PressureLevel::Critical)
        };
        for a in p.step(&r) {
            if let Action::Balloon(target, _) = a {
                balloon = target;
            }
        }
        assert!(
            total_free - balloon >= floor,
            "available fell below the floor"
        );
        t += POLL;
    }
}
