// SPDX-License-Identifier: Apache-2.0
//! virtio-rng (entropy source): fills every buffer the driver posts from
//! the host's `/dev/urandom`, so the guest's pool never waits.

use std::fs::File;
use std::io::Read;
use std::sync::Arc;

use crate::bus::put_le;
use crate::device::{
    Activation, Doorbell, QueueHandler, VirtioDevice, Worker, WorkerCtx, device_type, features,
};
use crate::devices::Segments;

struct Handler {
    urandom: File,
    buf: Vec<u8>,
}

impl QueueHandler for Handler {
    fn process(&mut self, ctx: &mut WorkerCtx, _bits: u32) {
        let (mem, q) = (&ctx.mem, &mut ctx.queues[0]);
        loop {
            while let Ok(Some(chain)) = q.pop(mem) {
                let out = Segments::writable(&chain);
                let want = out.len().min(1 << 16) as usize;
                self.buf.resize(want, 0);
                let n = match self.urandom.read_exact(&mut self.buf) {
                    Ok(()) => out.write_from(mem, &self.buf).unwrap_or(0),
                    Err(e) => {
                        tracing::warn!("virtio-rng: /dev/urandom: {e}");
                        0
                    }
                };
                let _ = q.add_used(mem, chain.head, n as u32);
            }
            let _ = q.publish_used(mem);
            if !q.enable_notification(mem).unwrap_or(false) {
                break;
            }
        }
        if q.needs_interrupt(mem).unwrap_or(true) {
            ctx.irq.signal_used();
        }
    }
}

pub struct Rng {
    worker: Worker,
}

impl Rng {
    pub fn new() -> std::io::Result<Self> {
        let handler = Handler {
            urandom: File::open("/dev/urandom")?,
            buf: Vec::new(),
        };
        Ok(Self {
            worker: Worker::spawn("virtio-rng", handler)?,
        })
    }
}

impl VirtioDevice for Rng {
    fn device_type(&self) -> u32 {
        device_type::RNG
    }
    fn features(&self) -> u64 {
        features::RING_EVENT_IDX | features::RING_INDIRECT_DESC
    }
    fn queue_max_sizes(&self) -> Vec<u16> {
        vec![64]
    }
    fn read_config(&self, _offset: u64, data: &mut [u8]) {
        put_le(data, 0);
    }
    fn activate(&mut self, act: Activation) -> Result<(), String> {
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
mod tests {
    use std::time::{Duration, Instant};

    use super::*;
    use crate::device::IrqStatus;
    use crate::irq::TestIrq;
    use crate::queue::testing::Driver;

    #[test]
    fn fills_posted_buffers_and_interrupts() {
        let mut d = Driver::new(8, true);
        let buf = d.buffer(&[], 64);
        let head = d.post(&[(buf, 64, true)]);
        let line = Arc::new(TestIrq::default());
        let mut rng = Rng::new().unwrap();
        rng.activate(Activation {
            mem: d.mem.clone(),
            queues: vec![d.queue.clone()],
            irq: Arc::new(IrqStatus::new(line.clone())),
            features: features::RING_EVENT_IDX,
        })
        .unwrap();
        let t = Instant::now();
        let used = loop {
            let u = d.used();
            if !u.is_empty() {
                break u;
            }
            assert!(t.elapsed() < Duration::from_secs(5));
            std::thread::sleep(Duration::from_millis(1));
        };
        assert_eq!(used, vec![(u32::from(head), 64)]);
        let mut got = [0u8; 64];
        d.mem.read(buf, &mut got).unwrap();
        assert_ne!(got, [0u8; 64]);
        assert!(line.level());
        rng.reset();
    }
}
