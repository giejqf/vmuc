// SPDX-License-Identifier: Apache-2.0
use std::time::{Duration, Instant};

use super::*;
use crate::device::IrqStatus;
use crate::irq::TestIrq;
use crate::queue::testing::Driver;

#[derive(Default)]
struct Recorder {
    ranges: Mutex<Vec<(u64, u64)>>,
    fail: bool,
}

impl Reclaim for Recorder {
    fn release(&self, gpa: u64, len: u64) -> Result<u64, String> {
        self.ranges.lock().unwrap().push((gpa, len));
        if self.fail {
            Err("remap failed".into())
        } else {
            Ok(len)
        }
    }
}

struct Rig {
    qs: Vec<Driver>,
    balloon: Balloon,
    rec: Arc<Recorder>,
}

fn rig(fail: bool) -> Rig {
    let qs = Driver::many(4, 16, true);
    let rec = Arc::new(Recorder {
        fail,
        ..Recorder::default()
    });
    let mut balloon = Balloon::new(rec.clone()).unwrap();
    let irq = Arc::new(IrqStatus::new(Arc::new(TestIrq::default())));
    balloon
        .activate(Activation {
            mem: qs[0].mem.clone(),
            queues: qs.iter().map(|d| d.queue.clone()).collect(),
            irq,
            features: features::RING_EVENT_IDX | F_STATS_VQ | F_REPORTING,
        })
        .unwrap();
    Rig { qs, balloon, rec }
}

fn wait_until(what: &str, f: impl Fn() -> bool) {
    let t = Instant::now();
    while !f() {
        assert!(t.elapsed() < Duration::from_secs(5), "timed out: {what}");
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn queue_numbering_follows_negotiated_features() {
    assert_eq!(
        optional_queues(F_STATS_VQ | F_REPORTING),
        (Some(2), Some(3))
    );
    assert_eq!(optional_queues(F_REPORTING), (None, Some(2)));
    assert_eq!(optional_queues(F_STATS_VQ), (Some(2), None));
    assert_eq!(optional_queues(0), (None, None));
}

#[test]
fn frames_coalesce_into_runs() {
    assert_eq!(
        runs(vec![12, 10, 11, 20, 11]),
        vec![(10 * PAGE, 3 * PAGE), (20 * PAGE, PAGE)]
    );
    assert_eq!(runs(Vec::new()), Vec::<(u64, u64)>::new());
}

#[test]
fn reported_ranges_are_released_before_the_chain_returns() {
    let mut r = rig(false);
    let head = r.qs[3].post(&[(0x4020_0000, 2 << 20, true), (0x4060_0000, 2 << 20, true)]);
    r.balloon.doorbell().ring(3);
    wait_until("report acked", || {
        r.qs[3]
            .mem
            .load_u16_acquire(r.qs[3].queue.used + 2)
            .unwrap()
            == 1
    });
    assert_eq!(
        *r.rec.ranges.lock().unwrap(),
        [(0x4020_0000, 2 << 20), (0x4060_0000, 2 << 20)]
    );
    assert_eq!(r.qs[3].used(), vec![(u32::from(head), 0)]);
}

#[test]
fn a_failed_release_is_never_acknowledged() {
    let mut r = rig(true);
    r.qs[3].post(&[(0x4020_0000, 2 << 20, true)]);
    r.balloon.doorbell().ring(3);
    wait_until("release attempted", || {
        !r.rec.ranges.lock().unwrap().is_empty()
    });
    std::thread::sleep(Duration::from_millis(20));
    assert!(r.qs[3].used().is_empty(), "a hole must not be acknowledged");
    assert!(r.balloon.handle().lock().failed);
}

#[test]
fn inflated_frames_release_contiguous_runs() {
    let mut r = rig(false);
    let pfns: Vec<u8> = [0x40010u32, 0x40011, 0x40012, 0x40013, 0x40100]
        .iter()
        .flat_map(|p| p.to_le_bytes())
        .collect();
    let buf = r.qs[0].buffer(&pfns, pfns.len() as u32);
    r.qs[0].post(&[(buf, pfns.len() as u32, false)]);
    r.balloon.doorbell().ring(0);
    let q0 = r.qs[0].queue.clone();
    wait_until("inflate acked", || {
        r.qs[0].mem.load_u16_acquire(q0.used + 2).unwrap() == 1
    });
    assert_eq!(
        *r.rec.ranges.lock().unwrap(),
        [(0x4001_0000, 4 * PAGE), (0x4010_0000, PAGE)]
    );
}

#[test]
fn stats_arrive_and_the_buffer_goes_back_on_request() {
    let mut r = rig(false);
    let mut raw = Vec::new();
    for (tag, v) in [(4u16, 1u64 << 30), (5, 2 << 30), (6, 3 << 29)] {
        raw.extend(tag.to_le_bytes());
        raw.extend(v.to_le_bytes());
    }
    let buf = r.qs[2].buffer(&raw, raw.len() as u32);
    let head = r.qs[2].post(&[(buf, raw.len() as u32, false)]);
    r.balloon.doorbell().ring(2);
    let h = r.balloon.handle();
    wait_until("stats read", || h.lock().stats.is_some());
    let (_, s) = h.stats().unwrap();
    assert_eq!((s.free, s.total, s.available), (1 << 30, 2 << 30, 3 << 29));
    wait_until("buffer returned", || {
        r.qs[2]
            .mem
            .load_u16_acquire(r.qs[2].queue.used + 2)
            .unwrap()
            == 1
    });
    assert_eq!(r.qs[2].used(), vec![(u32::from(head), 0)]);
}

#[test]
fn never_asks_the_guest_to_inflate() {
    let r = rig(false);
    let mut b = [0xffu8; 4];
    r.balloon.read_config(0, &mut b);
    assert_eq!(u32::from_le_bytes(b), 0, "num_pages");
    let mut balloon = r.balloon;
    balloon.write_config(4, &16u32.to_le_bytes());
    balloon.read_config(4, &mut b);
    assert_eq!(u32::from_le_bytes(b), 16, "actual reads back");
    assert_eq!(balloon.features() & (1 << 2), 0, "no DEFLATE_ON_OOM");
}
