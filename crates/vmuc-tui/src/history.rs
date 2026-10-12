// SPDX-License-Identifier: Apache-2.0
//! Recent samples of one instance's `Stat`, and the rates the screen
//! shows: host CPU load and memory handed back per second.

use std::collections::VecDeque;
use std::time::Duration;

use vmuc_proto::Stat;

/// Samples kept per instance (two minutes at one a second).
pub const DEPTH: usize = 120;

#[derive(Debug, Default, Clone)]
pub struct History {
    samples: VecDeque<(Duration, Stat)>,
}

impl History {
    /// Records `stat` taken at `at` (any monotonic clock). A sample from
    /// a new VMM process (CPU time went backwards) starts over.
    pub fn push(&mut self, at: Duration, stat: Stat) {
        if self
            .samples
            .back()
            .is_some_and(|(_, last)| stat.host_cpu_nanos < last.host_cpu_nanos)
        {
            self.samples.clear();
        }
        if self.samples.len() == DEPTH {
            self.samples.pop_front();
        }
        self.samples.push_back((at, stat));
    }

    pub fn latest(&self) -> Option<&Stat> {
        self.samples.back().map(|(_, s)| s)
    }

    /// Rate of `f` between consecutive samples, per second.
    fn rates(&self, f: fn(&Stat) -> u64) -> impl Iterator<Item = f64> + '_ {
        self.samples
            .iter()
            .zip(self.samples.iter().skip(1))
            .map(move |((t0, a), (t1, b))| {
                let dt = t1.saturating_sub(*t0).as_secs_f64();
                if dt > 0.0 {
                    Duration::from_nanos(f(b).saturating_sub(f(a))).as_secs_f64() * 1e9 / dt
                } else {
                    0.0
                }
            })
    }

    /// Host CPU over the last interval, percent of one core (top's way).
    pub fn cpu_percent(&self) -> Option<f64> {
        self.rates(|s| s.host_cpu_nanos).last().map(|ns| ns / 1e7)
    }

    /// Host footprint per sample, MiB, for a sparkline.
    pub fn footprint_mib(&self) -> Vec<u64> {
        self.samples
            .iter()
            .map(|(_, s)| s.host_footprint >> 20)
            .collect()
    }

    /// Memory released to the host per interval, KiB/s, for a sparkline.
    pub fn released_kib_per_s(&self) -> Vec<u64> {
        // Rates are non-negative and far below 2^52: the cast is exact.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        self.rates(|s| s.released)
            .map(|b| (b / 1024.0) as u64)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stat(cpu_ms: u64, released: u64) -> Stat {
        Stat {
            host_cpu_nanos: cpu_ms * 1_000_000,
            released,
            host_footprint: 300 << 20,
            ..Stat::default()
        }
    }

    #[test]
    fn cpu_load_is_the_rate_between_the_last_two_samples() {
        let mut h = History::default();
        assert_eq!(h.cpu_percent(), None);
        h.push(Duration::from_secs(1), stat(100, 0));
        assert_eq!(h.cpu_percent(), None);
        h.push(Duration::from_secs(3), stat(600, 4 << 20));
        assert!((h.cpu_percent().unwrap() - 25.0).abs() < 1e-9);
        assert_eq!(h.released_kib_per_s(), vec![2048]);
        assert_eq!(h.footprint_mib(), vec![300, 300]);
    }

    #[test]
    fn a_restarted_vmm_starts_a_new_history() {
        let mut h = History::default();
        h.push(Duration::from_secs(1), stat(5000, 0));
        h.push(Duration::from_secs(2), stat(10, 0));
        assert_eq!(h.footprint_mib().len(), 1);
        assert_eq!(h.cpu_percent(), None);
    }

    #[test]
    fn depth_is_bounded() {
        let mut h = History::default();
        for i in 0..DEPTH as u64 + 10 {
            h.push(Duration::from_secs(i), stat(i, 0));
        }
        assert_eq!(h.footprint_mib().len(), DEPTH);
    }
}
