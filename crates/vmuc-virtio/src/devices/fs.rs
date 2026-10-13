// SPDX-License-Identifier: Apache-2.0
//! virtio-fs: FUSE requests on virtqueues, answered by `vmuc-fs`.
//!
//! One device per shared directory, identified to the guest by its tag
//! (`mount -t virtiofs TAG DIR`). Queue 0 is the high-priority queue
//! (FORGET, INTERRUPT), handled on the device's worker thread; queue 1 is
//! the one request queue. Each chain's readable part is a whole FUSE
//! request and its writable part takes the reply.
//!
//! Requests run on a small pool so one host call that blocks (a macOS
//! privacy prompt for ~/Desktop, an iCloud file being downloaded, a FIFO)
//! stalls only its own thread: the worker hands each request to an idle
//! thread and completes chains in whatever order replies come back. Pool
//! threads never touch guest memory, so they need not be joined before
//! it is unmapped, and one stuck in the host never holds up shutdown.

use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, OnceLock};

use vmuc_fs::server::Server;

use crate::bus::put_le;
use crate::device::{
    Activation, Doorbell, HOST_EVENT, ParkDoorbell, QueueHandler, ShmRegion, VirtioDevice, Worker,
    WorkerCtx, device_type, features,
};
use crate::devices::Segments;

/// Longest tag the config space holds.
pub const TAG_LEN: usize = 36;
/// Request threads per share.
const POOL: usize = 4;
const HIPRIO: usize = 0;
const REQUEST: usize = 1;

struct Job {
    epoch: u64,
    head: u16,
    req: Vec<u8>,
}

struct Done {
    thread: usize,
    epoch: u64,
    head: u16,
    reply: Option<Vec<u8>>,
}

struct Handler {
    server: Arc<Server>,
    jobs: Vec<Sender<Job>>,
    busy: [u32; POOL],
    done: Receiver<Done>,
    /// Reply buffers of the chains a pool thread is working on.
    inflight: HashMap<u16, Segments>,
    /// Bumped on reset, so replies to a previous driver are dropped.
    epoch: u64,
    req: Vec<u8>,
}

impl Handler {
    /// Starts the pool; its threads ring `wake` (the worker's doorbell,
    /// set once the worker exists) with [`HOST_EVENT`] after each reply.
    fn new(server: Server, wake: &Arc<OnceLock<Arc<ParkDoorbell>>>) -> std::io::Result<Self> {
        let server = Arc::new(server);
        let (done_tx, done) = mpsc::channel();
        let mut jobs = Vec::with_capacity(POOL);
        for thread in 0..POOL {
            let (tx, rx) = mpsc::channel::<Job>();
            let (server, done_tx, wake) = (Arc::clone(&server), done_tx.clone(), Arc::clone(wake));
            std::thread::Builder::new()
                .name(format!("virtio-fs-{thread}"))
                .spawn(move || {
                    for job in rx {
                        let reply = server.handle(&job.req);
                        let done = Done {
                            thread,
                            epoch: job.epoch,
                            head: job.head,
                            reply,
                        };
                        if done_tx.send(done).is_err() {
                            return;
                        }
                        if let Some(db) = wake.get() {
                            db.ring_bits(HOST_EVENT);
                        }
                    }
                })?;
            jobs.push(tx);
        }
        Ok(Self {
            server,
            jobs,
            busy: [0; POOL],
            done,
            inflight: HashMap::new(),
            epoch: 0,
            req: Vec::new(),
        })
    }

    /// High-priority requests are quick and answered in place.
    fn serve_hiprio(&mut self, ctx: &mut WorkerCtx) {
        let (mem, q) = (&ctx.mem, &mut ctx.queues[HIPRIO]);
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

    /// Hands every new request to the least busy pool thread.
    fn dispatch(&mut self, ctx: &mut WorkerCtx) {
        let (mem, q) = (&ctx.mem, &mut ctx.queues[REQUEST]);
        loop {
            while let Ok(Some(chain)) = q.pop(mem) {
                let src = Segments::readable(&chain);
                let mut req = vec![0u8; src.len() as usize];
                let n = src.read_into(mem, &mut req).unwrap_or(0);
                req.truncate(n);
                let t = (0..POOL).min_by_key(|&t| self.busy[t]).unwrap_or(0);
                let job = Job {
                    epoch: self.epoch,
                    head: chain.head,
                    req,
                };
                if self.jobs[t].send(job).is_ok() {
                    self.busy[t] += 1;
                    self.inflight.insert(chain.head, Segments::writable(&chain));
                } else {
                    let _ = q.add_used(mem, chain.head, 0);
                }
            }
            if !q.enable_notification(mem).unwrap_or(false) {
                break;
            }
        }
    }

    /// Writes back the replies that have come in.
    fn complete(&mut self, ctx: &mut WorkerCtx) {
        let (mem, q) = (&ctx.mem, &mut ctx.queues[REQUEST]);
        let mut any = false;
        while let Ok(d) = self.done.try_recv() {
            self.busy[d.thread] = self.busy[d.thread].saturating_sub(1);
            if d.epoch != self.epoch {
                continue;
            }
            let Some(out) = self.inflight.remove(&d.head) else {
                continue;
            };
            let written = d.reply.map_or(0, |r| out.write_from(mem, &r).unwrap_or(0));
            let _ = q.add_used(mem, d.head, written as u32);
            any = true;
        }
        if any {
            let _ = q.publish_used(mem);
            if q.needs_interrupt(mem).unwrap_or(true) {
                ctx.irq.signal_used();
            }
        }
    }
}

impl QueueHandler for Handler {
    fn process(&mut self, ctx: &mut WorkerCtx, bits: u32) {
        if bits & (1 << HIPRIO) != 0 {
            self.serve_hiprio(ctx);
        }
        if ctx.queues.len() > REQUEST {
            if bits & (1 << REQUEST) != 0 {
                self.dispatch(ctx);
            }
            self.complete(ctx);
        }
    }

    fn reset(&mut self) {
        self.epoch += 1;
        self.inflight.clear();
    }
}

pub struct Fs {
    config: [u8; TAG_LEN + 4],
    worker: Worker,
    /// The DAX window (shared memory region 0), when there is one.
    window: Option<ShmRegion>,
}

impl Fs {
    /// `tag` must fit in 36 bytes (longer ones are cut).
    pub fn new(tag: &str, server: Server) -> std::io::Result<Self> {
        let mut config = [0u8; TAG_LEN + 4];
        let n = tag.len().min(TAG_LEN);
        config[..n].copy_from_slice(&tag.as_bytes()[..n]);
        config[TAG_LEN..].copy_from_slice(&1u32.to_le_bytes()); // num_request_queues
        let wake = Arc::new(OnceLock::new());
        let worker = Worker::spawn("virtio-fs", Handler::new(server, &wake)?)?;
        let _ = wake.set(worker.doorbell());
        Ok(Self {
            config,
            worker,
            window: None,
        })
    }

    /// Offers a DAX window at guest physical `base` (shared memory region
    /// 0, `VIRTIO_FS_SHMCAP_ID_CACHE`); the server maps into it.
    #[must_use]
    pub fn with_window(mut self, base: u64, len: u64) -> Self {
        self.window = Some(ShmRegion { id: 0, base, len });
        self
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
    fn shm_regions(&self) -> Vec<ShmRegion> {
        self.window.into_iter().collect()
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
    use crate::{Gpa, GuestMemory};

    struct Harness {
        fs: Fs,
        rq: Driver,
        dir: tempfile::TempDir,
        unique: u64,
    }

    fn harness(read_only: bool) -> Harness {
        let dir = tempfile::tempdir().unwrap();
        let server = Server::new(Passthrough::new(dir.path(), read_only).unwrap());
        let mut fs = Fs::new("vmuc-test", server).unwrap();
        let mut qs = Driver::many(2, 16, true);
        let rq = qs.pop().unwrap();
        let hq = qs.pop().unwrap();
        fs.activate(Activation {
            mem: rq.mem.clone(),
            queues: vec![hq.queue.clone(), rq.queue.clone()],
            irq: Arc::new(IrqStatus::new(Arc::new(TestIrq::default()))),
            features: features::RING_EVENT_IDX,
        })
        .unwrap();
        Harness {
            fs,
            rq,
            dir,
            unique: 0,
        }
    }

    impl Harness {
        /// Posts one request; returns (head, reply buffer).
        fn send(&mut self, op: u32, node: u64, body: &[u8]) -> (u16, Gpa) {
            self.unique += 1;
            let len = (40 + body.len()) as u32;
            let h = InHeader {
                len,
                opcode: op,
                unique: self.unique,
                nodeid: node,
                ..InHeader::default()
            };
            let mut req = h.encode().to_vec();
            req.extend_from_slice(body);
            let r = self.rq.buffer(&req, len);
            let out = self.rq.buffer(&[], 256);
            let head = self.rq.post(&[(r, len, false), (out, 256, true)]);
            self.fs.doorbell().ring(REQUEST as u16);
            (head, out)
        }

        /// Waits for the next completion: (head, reply bytes).
        fn next(&mut self, mem: &GuestMemory, out_of: &[(u16, Gpa)]) -> (u16, Vec<u8>) {
            let t = Instant::now();
            loop {
                if let Some(&(head, len)) = self.rq.used().first() {
                    let (_, buf) = out_of.iter().find(|(h, _)| u32::from(*h) == head).unwrap();
                    let mut reply = vec![0u8; len as usize];
                    mem.read(*buf, &mut reply).unwrap();
                    return (head as u16, reply);
                }
                assert!(t.elapsed() < Duration::from_secs(5), "no reply");
                std::thread::sleep(Duration::from_millis(1));
            }
        }
    }

    fn u64_at(b: &[u8], o: usize) -> u64 {
        u64::from_le_bytes(b[o..o + 8].try_into().unwrap())
    }

    #[test]
    fn a_lookup_round_trips_through_the_request_queue() {
        let mut h = harness(true);
        std::fs::write(h.dir.path().join("file"), b"12345").unwrap();
        let mut tag = [0u8; 9];
        h.fs.read_config(0, &mut tag);
        assert_eq!(&tag, b"vmuc-test");
        let sent = h.send(opcode::LOOKUP, ROOT_ID, b"file\0");
        let mem = h.rq.mem.clone();
        let (head, reply) = h.next(&mem, &[sent]);
        assert_eq!(head, sent.0);
        assert_eq!(reply.len(), 16 + 128, "out header + entry_out");
        assert_eq!(i32::from_le_bytes(reply[4..8].try_into().unwrap()), 0);
        assert_eq!(u64_at(&reply, 16 + 48), 5, "size");
    }

    #[test]
    fn a_request_blocked_in_the_host_does_not_hold_up_the_next() {
        let mut h = harness(false);
        let fifo = h.dir.path().join("fifo");
        let c = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
        // SAFETY: valid C string.
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
        std::fs::write(h.dir.path().join("file"), b"").unwrap();
        let mem = h.rq.mem.clone();
        let sent = h.send(opcode::LOOKUP, ROOT_ID, b"fifo\0");
        let node = u64_at(&h.next(&mem, &[sent]).1, 16);
        // Opening a FIFO for reading blocks until a writer comes along.
        let open = h.send(opcode::OPEN, node, &[0; 8]);
        std::thread::sleep(Duration::from_millis(20));
        let lookup = h.send(opcode::LOOKUP, ROOT_ID, b"file\0");
        assert_eq!(
            h.next(&mem, &[open, lookup]).0,
            lookup.0,
            "answered while OPEN waits"
        );
        let writer = std::fs::OpenOptions::new().write(true).open(&fifo).unwrap();
        let (head, reply) = h.next(&mem, &[open, lookup]);
        assert_eq!(head, open.0);
        assert_eq!(i32::from_le_bytes(reply[4..8].try_into().unwrap()), 0);
        drop(writer);
    }
}
