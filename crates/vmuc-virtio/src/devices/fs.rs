// SPDX-License-Identifier: Apache-2.0
//! virtio-fs: FUSE requests on virtqueues, answered by `vmuc-fs`.
//!
//! One device per shared directory, identified to the guest by its tag
//! (`mount -t virtiofs TAG DIR`). Queue 0 is the high-priority queue
//! (FORGET, INTERRUPT), queue 1 the one request queue. Each chain's
//! readable part is a whole FUSE request and its writable part takes the
//! reply; requests are handled in order on the device's worker thread.

use std::sync::Arc;

use vmuc_fs::server::Server;

use crate::bus::put_le;
use crate::device::{
    Activation, Doorbell, QueueHandler, VirtioDevice, Worker, WorkerCtx, device_type, features,
};
use crate::devices::Segments;

/// Longest tag the config space holds.
pub const TAG_LEN: usize = 36;

struct Handler {
    server: Arc<Server>,
    req: Vec<u8>,
}

impl Handler {
    fn serve(&mut self, ctx: &mut WorkerCtx, qi: usize) {
        let (mem, q) = (&ctx.mem, &mut ctx.queues[qi]);
        loop {
            while let Ok(Some(chain)) = q.pop(mem) {
                let src = Segments::readable(&chain);
                self.req.resize(src.len() as usize, 0);
                let n = src.read_into(mem, &mut self.req).unwrap_or(0);
                let written = match self.server.handle(&self.req[..n]) {
                    Some(reply) => Segments::writable(&chain)
                        .write_from(mem, &reply)
                        .unwrap_or(0),
                    None => 0,
                };
                let _ = q.add_used(mem, chain.head, written as u32);
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

impl QueueHandler for Handler {
    fn process(&mut self, ctx: &mut WorkerCtx, bits: u32) {
        for qi in 0..ctx.queues.len().min(2) {
            if bits & (1 << qi) != 0 {
                self.serve(ctx, qi);
            }
        }
    }
}

pub struct Fs {
    config: [u8; TAG_LEN + 4],
    worker: Worker,
}

impl Fs {
    /// `tag` must fit in 36 bytes (longer ones are cut).
    pub fn new(tag: &str, server: Server) -> std::io::Result<Self> {
        let mut config = [0u8; TAG_LEN + 4];
        let n = tag.len().min(TAG_LEN);
        config[..n].copy_from_slice(&tag.as_bytes()[..n]);
        config[TAG_LEN..].copy_from_slice(&1u32.to_le_bytes()); // num_request_queues
        let handler = Handler {
            server: Arc::new(server),
            req: Vec::new(),
        };
        Ok(Self {
            config,
            worker: Worker::spawn("virtio-fs", handler)?,
        })
    }
}

impl VirtioDevice for Fs {
    fn device_type(&self) -> u32 {
        device_type::FS
    }
    fn features(&self) -> u64 {
        features::RING_EVENT_IDX | features::RING_INDIRECT_DESC
    }
    fn queue_max_sizes(&self) -> Vec<u16> {
        vec![128, 1024]
    }
    fn read_config(&self, offset: u64, data: &mut [u8]) {
        let off = offset as usize;
        if off + data.len() <= self.config.len() {
            data.copy_from_slice(&self.config[off..off + data.len()]);
        } else {
            put_le(data, 0);
        }
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

    use vmuc_fs::abi::{InHeader, ROOT_ID, opcode};
    use vmuc_fs::passthrough::Passthrough;

    use super::*;
    use crate::device::IrqStatus;
    use crate::irq::TestIrq;
    use crate::queue::testing::Driver;

    #[test]
    fn a_lookup_round_trips_through_the_request_queue() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("file"), b"12345").unwrap();
        let server = Server::new(Passthrough::new(dir.path(), true).unwrap());
        let mut fs = Fs::new("vmuc-test", server).unwrap();
        let mut tag = [0u8; 9];
        fs.read_config(0, &mut tag);
        assert_eq!(&tag, b"vmuc-test");

        let mut qs = Driver::many(2, 16, true);
        let mut rq = qs.pop().unwrap();
        let hq = qs.pop().unwrap();
        fs.activate(Activation {
            mem: rq.mem.clone(),
            queues: vec![hq.queue.clone(), rq.queue.clone()],
            irq: Arc::new(IrqStatus::new(Arc::new(TestIrq::default()))),
            features: features::RING_EVENT_IDX,
        })
        .unwrap();
        let mut req = InHeader {
            len: 45,
            opcode: opcode::LOOKUP,
            unique: 5,
            nodeid: ROOT_ID,
            ..InHeader::default()
        }
        .encode()
        .to_vec();
        req.extend_from_slice(b"file\0");
        let r = rq.buffer(&req, req.len() as u32);
        let out = rq.buffer(&[], 256);
        rq.post(&[(r, req.len() as u32, false), (out, 256, true)]);
        fs.doorbell().ring(1);
        let t = Instant::now();
        while rq.mem.load_u16_acquire(rq.queue.used + 2).unwrap() == 0 {
            assert!(t.elapsed() < Duration::from_secs(5));
            std::thread::sleep(Duration::from_millis(1));
        }
        let (_, len) = rq.used()[0];
        assert_eq!(len, 16 + 128, "out header + entry_out");
        let mut reply = vec![0u8; len as usize];
        rq.mem.read(out, &mut reply).unwrap();
        assert_eq!(i32::from_le_bytes(reply[4..8].try_into().unwrap()), 0);
        assert_eq!(
            u64::from_le_bytes(reply[16 + 48..16 + 56].try_into().unwrap()),
            5,
            "size"
        );
    }
}
