// SPDX-License-Identifier: Apache-2.0
//! virtio-console, single port (`hvc0`): the guest's console once the
//! kernel is up. Guest output goes to a [`ConsoleSink`]; host input
//! (keyboard, `console.sock` clients) waits in a queue until the guest
//! posts receive buffers.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, PoisonError};

use crate::bus::put_le;
use crate::device::{
    Activation, Doorbell, HOST_EVENT, ParkDoorbell, QueueHandler, VirtioDevice, Worker, WorkerCtx,
    device_type, features,
};
use crate::devices::Segments;

const RX: usize = 0;
const TX: usize = 1;
/// Input held for the guest before new bytes are dropped.
const INPUT_LIMIT: usize = 1 << 20;

/// Where guest console output goes.
pub trait ConsoleSink: Send + Sync {
    fn write(&self, bytes: &[u8]);
}

/// Feeds host input to the guest; cheap to clone, usable from any thread.
#[derive(Clone)]
pub struct ConsoleInput {
    queue: Arc<Mutex<VecDeque<u8>>>,
    doorbell: Arc<ParkDoorbell>,
}

impl std::fmt::Debug for ConsoleInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ConsoleInput")
    }
}

impl ConsoleInput {
    pub fn send(&self, bytes: &[u8]) {
        {
            let mut q = self.queue.lock().unwrap_or_else(PoisonError::into_inner);
            let room = INPUT_LIMIT.saturating_sub(q.len());
            q.extend(&bytes[..bytes.len().min(room)]);
        }
        self.doorbell.ring_bits(HOST_EVENT);
    }
}

struct Handler {
    sink: Arc<dyn ConsoleSink>,
    input: Arc<Mutex<VecDeque<u8>>>,
    buf: Vec<u8>,
}

impl Handler {
    fn transmit(&mut self, ctx: &mut WorkerCtx) {
        let (mem, q) = (&ctx.mem, &mut ctx.queues[TX]);
        loop {
            while let Ok(Some(chain)) = q.pop(mem) {
                let src = Segments::readable(&chain);
                self.buf.resize(src.len().min(1 << 20) as usize, 0);
                let n = src.read_into(mem, &mut self.buf).unwrap_or(0);
                self.sink.write(&self.buf[..n]);
                let _ = q.add_used(mem, chain.head, 0);
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

    fn receive(&mut self, ctx: &mut WorkerCtx) {
        let (mem, q) = (&ctx.mem, &mut ctx.queues[RX]);
        let mut input = self.input.lock().unwrap_or_else(PoisonError::into_inner);
        while !input.is_empty() {
            let Ok(Some(chain)) = q.pop(mem) else {
                // No buffers: keep the input; the guest's next kick on
                // the receive queue brings us back.
                break;
            };
            let dst = Segments::writable(&chain);
            let n = (dst.len() as usize).min(input.len());
            let bytes: Vec<u8> = input.drain(..n).collect();
            let written = dst.write_from(mem, &bytes).unwrap_or(0);
            let _ = q.add_used(mem, chain.head, written as u32);
        }
        drop(input);
        let _ = q.publish_used(mem);
        let _ = q.enable_notification(mem);
        if q.needs_interrupt(mem).unwrap_or(true) {
            ctx.irq.signal_used();
        }
    }
}

impl QueueHandler for Handler {
    fn process(&mut self, ctx: &mut WorkerCtx, bits: u32) {
        if bits & (1 << TX) != 0 {
            self.transmit(ctx);
        }
        if bits & ((1 << RX) | HOST_EVENT) != 0 {
            self.receive(ctx);
        }
    }

    fn reset(&mut self) {
        self.input
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
    }
}

pub struct Console {
    worker: Worker,
    input: ConsoleInput,
}

impl Console {
    pub fn new(sink: Arc<dyn ConsoleSink>) -> std::io::Result<Self> {
        let queue = Arc::new(Mutex::new(VecDeque::new()));
        let handler = Handler {
            sink,
            input: Arc::clone(&queue),
            buf: Vec::new(),
        };
        let worker = Worker::spawn("virtio-console", handler)?;
        let input = ConsoleInput {
            queue,
            doorbell: worker.doorbell(),
        };
        Ok(Self { worker, input })
    }

    pub fn input(&self) -> ConsoleInput {
        self.input.clone()
    }
}

impl VirtioDevice for Console {
    fn device_type(&self) -> u32 {
        device_type::CONSOLE
    }
    fn features(&self) -> u64 {
        features::RING_EVENT_IDX | features::RING_INDIRECT_DESC
    }
    fn queue_max_sizes(&self) -> Vec<u16> {
        vec![128, 128]
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

    #[derive(Default)]
    struct Collect(Mutex<Vec<u8>>);
    impl ConsoleSink for Collect {
        fn write(&self, bytes: &[u8]) {
            self.0.lock().unwrap().extend_from_slice(bytes);
        }
    }

    fn wait_until(f: impl Fn() -> bool) {
        let t = Instant::now();
        while !f() {
            assert!(t.elapsed() < Duration::from_secs(5), "timed out");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn output_reaches_the_sink_and_input_waits_for_buffers() {
        let sink = Arc::new(Collect::default());
        let mut con = Console::new(sink.clone()).unwrap();
        let mut rx = Driver::new(8, true);
        // Both queues live in one guest memory: carve TX from RX's heap.
        let mem = rx.mem.clone();
        let mut txq = rx.queue.clone();
        txq.desc = 0x4_0000;
        txq.avail = 0x4_1000;
        txq.used = 0x4_2000;
        con.input().send(b"early ");
        con.activate(Activation {
            mem: mem.clone(),
            queues: vec![rx.queue.clone(), txq.clone()],
            irq: Arc::new(IrqStatus::new(Arc::new(TestIrq::default()))),
            features: features::RING_EVENT_IDX,
        })
        .unwrap();

        // Guest posts one receive buffer: the early input lands in it.
        let buf = rx.buffer(&[], 64);
        let head = rx.post(&[(buf, 64, true)]);
        con.doorbell().ring(0);
        wait_until(|| {
            let mut b = [0u8; 6];
            mem.read(buf, &mut b).unwrap();
            &b == b"early "
        });
        wait_until(|| mem.load_u16_acquire(rx.queue.used + 2).unwrap() == 1);
        assert_eq!(rx.used(), vec![(u32::from(head), 6)]);

        // Guest output on the transmit queue (posted by hand into TX).
        let text = rx.buffer(b"hello hvc0", 10);
        mem.write_obj(txq.desc, text).unwrap();
        mem.write_obj(txq.desc + 8, 10u32).unwrap();
        mem.write_obj(txq.avail + 4, 0u16).unwrap();
        mem.store_u16_release(txq.avail + 2, 1).unwrap();
        con.doorbell().ring(1);
        wait_until(|| sink.0.lock().unwrap().as_slice() == b"hello hvc0");
        con.reset();
    }
}
