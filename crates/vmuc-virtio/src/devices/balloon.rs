// SPDX-License-Identifier: Apache-2.0
//! virtio-balloon with free page reporting: the guest half of returning
//! memory to macOS.
//!
//! Free page reporting is the bulk path: the guest posts runs of free
//! pages (2 MiB on a 4 KiB-page guest) on the reporting queue, the
//! device releases each through [`Reclaim`] and only then returns the
//! chain; the guest does not touch reported pages until it is back.
//! Inflation hands over single 4 KiB frames, so only contiguous runs of
//! them can be released. The stats queue carries the guest's memory
//! counters; the device keeps the guest's buffer and returns it to ask
//! for fresh numbers. `num_pages` is always 0: vmuc never asks the guest
//! to inflate (ADR 0004), so the inflate queue only matters to a driver
//! that inflates on its own.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;

use crate::bus::put_le;
use crate::device::{
    Activation, Doorbell, HOST_EVENT, ParkDoorbell, QueueHandler, VirtioDevice, Worker, WorkerCtx,
    device_type, features,
};
use crate::devices::Segments;
use crate::mem::GuestMemory;
use crate::queue::Queue;

pub const F_STATS_VQ: u64 = 1 << 1;
pub const F_REPORTING: u64 = 1 << 5;

const INFLATE: usize = 0;
const DEFLATE: usize = 1;
const PAGE: u64 = 4096;

/// Where released ranges go: the VMM's reclaimer.
pub trait Reclaim: Send + Sync {
    /// Gives `[gpa, gpa + len)` back to the host; `Err` means the VM can
    /// no longer run (the range is not acknowledged and the device stops).
    fn release(&self, gpa: u64, len: u64) -> Result<u64, String>;
}

/// The guest's memory counters (`VIRTIO_BALLOON_S_*`), bytes or counts.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GuestStats {
    pub swap_in: u64,
    pub swap_out: u64,
    pub major_faults: u64,
    pub minor_faults: u64,
    pub free: u64,
    pub total: u64,
    pub available: u64,
    pub caches: u64,
}

impl GuestStats {
    fn set(&mut self, tag: u16, value: u64) {
        match tag {
            0 => self.swap_in = value,
            1 => self.swap_out = value,
            2 => self.major_faults = value,
            3 => self.minor_faults = value,
            4 => self.free = value,
            5 => self.total = value,
            6 => self.available = value,
            7 => self.caches = value,
            _ => {}
        }
    }
}

#[derive(Debug, Default)]
struct State {
    /// What the guest says the balloon holds (`actual`).
    actual: u32,
    stats: Option<(Instant, GuestStats)>,
    /// The device stopped after a fatal reclaim error.
    failed: bool,
}

/// Host-side control of the balloon; cheap to clone, any thread.
#[derive(Clone)]
pub struct BalloonHandle {
    state: Arc<Mutex<State>>,
    doorbell: Arc<ParkDoorbell>,
}

impl std::fmt::Debug for BalloonHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("BalloonHandle")
    }
}

impl BalloonHandle {
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The latest guest counters and when they arrived; asks for fresh ones.
    pub fn stats(&self) -> Option<(Instant, GuestStats)> {
        let s = self.lock().stats;
        self.doorbell.ring_bits(HOST_EVENT);
        s
    }
}

struct Handler {
    state: Arc<Mutex<State>>,
    reclaim: Arc<dyn Reclaim>,
    /// The guest's stats buffer, held until we want new numbers.
    stats_head: Option<u16>,
}

/// Queue indices after the inflate/deflate pair: Linux numbers only the
/// queues whose features were negotiated.
fn optional_queues(features: u64) -> (Option<usize>, Option<usize>) {
    let mut next = 2;
    let mut take = |on: bool| {
        on.then(|| {
            next += 1;
            next - 1
        })
    };
    let stats = take(features & F_STATS_VQ != 0);
    let reporting = take(features & F_REPORTING != 0);
    (stats, reporting)
}

/// Coalesces 4 KiB frame numbers into contiguous guest-physical runs.
fn runs(mut pfns: Vec<u32>) -> Vec<(u64, u64)> {
    pfns.sort_unstable();
    pfns.dedup();
    let mut out: Vec<(u64, u64)> = Vec::new();
    for pfn in pfns {
        let gpa = u64::from(pfn) * PAGE;
        match out.last_mut() {
            Some((start, len)) if *start + *len == gpa => *len += PAGE,
            _ => out.push((gpa, PAGE)),
        }
    }
    out
}

impl Handler {
    fn fail(&self, why: &str) {
        tracing::error!("virtio-balloon stops: {why}");
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .failed = true;
    }

    fn failed(&self) -> bool {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .failed
    }

    /// Reporting: release every range of a chain, then return it.
    fn report(&mut self, mem: &GuestMemory, q: &mut Queue) -> bool {
        let mut any = false;
        while let Ok(Some(chain)) = q.pop(mem) {
            for d in &chain.descs {
                if let Err(e) = self.reclaim.release(d.addr, u64::from(d.len)) {
                    // Never acknowledge a range that may not be mapped back.
                    self.fail(&e);
                    return any;
                }
            }
            let _ = q.add_used(mem, chain.head, 0);
            any = true;
        }
        any
    }

    /// Inflate (release contiguous runs) or deflate (nothing to do: the
    /// guest's next touch faults fresh pages in).
    fn frames(&mut self, mem: &GuestMemory, q: &mut Queue, inflate: bool) -> bool {
        let mut any = false;
        while let Ok(Some(chain)) = q.pop(mem) {
            if inflate {
                let src = Segments::readable(&chain);
                let mut raw = vec![0u8; src.len() as usize];
                let n = src.read_into(mem, &mut raw).unwrap_or(0);
                let pfns = raw[..n]
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .map(|b| u32::from_le_bytes(*b))
                    .collect();
                for (gpa, len) in runs(pfns) {
                    if let Err(e) = self.reclaim.release(gpa, len) {
                        self.fail(&e);
                        return any;
                    }
                }
            }
            let _ = q.add_used(mem, chain.head, 0);
            any = true;
        }
        any
    }

    /// Stats: read the buffer the guest posted and keep it.
    fn take_stats(&mut self, mem: &GuestMemory, q: &mut Queue) {
        while let Ok(Some(chain)) = q.pop(mem) {
            let src = Segments::readable(&chain);
            let mut raw = vec![0u8; src.len().min(4096) as usize];
            let n = src.read_into(mem, &mut raw).unwrap_or(0);
            let mut stats = GuestStats::default();
            for entry in raw[..n].as_chunks::<10>().0 {
                let tag = u16::from_le_bytes([entry[0], entry[1]]);
                let value = u64::from_le_bytes(entry[2..10].try_into().unwrap_or_default());
                stats.set(tag, value);
            }
            self.state
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .stats = Some((Instant::now(), stats));
            self.stats_head = Some(chain.head);
        }
    }

    fn finish(ctx: &mut WorkerCtx, qi: usize, any: bool) {
        let (mem, q) = (&ctx.mem, &mut ctx.queues[qi]);
        if any {
            let _ = q.publish_used(mem);
        }
        let _ = q.enable_notification(mem);
        if any && q.needs_interrupt(mem).unwrap_or(true) {
            ctx.irq.signal_used();
        }
    }
}

impl QueueHandler for Handler {
    fn process(&mut self, ctx: &mut WorkerCtx, bits: u32) {
        if self.failed() {
            return;
        }
        let mem = ctx.mem.clone();
        let (stats_q, report_q) = optional_queues(ctx.features);
        if let Some(r) = report_q
            && bits & (1 << r) != 0
        {
            let any = self.report(&mem, &mut ctx.queues[r]);
            Self::finish(ctx, r, any);
        }
        for (qi, inflate) in [(INFLATE, true), (DEFLATE, false)] {
            if bits & (1 << qi) != 0 {
                let any = self.frames(&mem, &mut ctx.queues[qi], inflate);
                Self::finish(ctx, qi, any);
            }
        }
        if let Some(s) = stats_q {
            if bits & (1 << s) != 0 {
                self.take_stats(&mem, &mut ctx.queues[s]);
            }
            // A host request for fresh numbers: hand the buffer back.
            if bits & HOST_EVENT != 0
                && let Some(head) = self.stats_head.take()
            {
                let _ = ctx.queues[s].add_used(&mem, head, 0);
                Self::finish(ctx, s, true);
            }
        }
    }

    fn reset(&mut self) {
        self.stats_head = None;
        let mut s = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        s.actual = 0;
        s.stats = None;
    }
}

pub struct Balloon {
    worker: Worker,
    handle: BalloonHandle,
}

impl Balloon {
    pub fn new(reclaim: Arc<dyn Reclaim>) -> std::io::Result<Self> {
        let state = Arc::new(Mutex::new(State::default()));
        let handler = Handler {
            state: Arc::clone(&state),
            reclaim,
            stats_head: None,
        };
        let worker = Worker::spawn("virtio-balloon", handler)?;
        let handle = BalloonHandle {
            state,
            doorbell: worker.doorbell(),
        };
        Ok(Self { worker, handle })
    }

    pub fn handle(&self) -> BalloonHandle {
        self.handle.clone()
    }
}

impl VirtioDevice for Balloon {
    fn device_type(&self) -> u32 {
        device_type::BALLOON
    }
    fn features(&self) -> u64 {
        features::RING_EVENT_IDX | F_STATS_VQ | F_REPORTING
    }
    fn queue_max_sizes(&self) -> Vec<u16> {
        // inflate, deflate, stats, reporting
        vec![128, 128, 4, 32]
    }
    fn read_config(&self, offset: u64, data: &mut [u8]) {
        let s = self.handle.lock();
        // num_pages (offset 0) is always 0.
        let v = if offset == 4 { s.actual } else { 0 };
        put_le(data, u64::from(v));
    }
    fn write_config(&mut self, offset: u64, data: &[u8]) {
        if offset == 4 && data.len() == 4 {
            self.handle.lock().actual = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
        }
    }
    fn activate(&mut self, act: Activation) -> Result<(), String> {
        self.handle.lock().failed = false;
        self.worker.activate(act)
    }
    fn reset(&mut self) {
        self.worker.reset();
    }
    fn doorbell(&self) -> Arc<dyn Doorbell> {
        self.worker.doorbell()
    }
}

#[cfg(test)]
mod tests;
