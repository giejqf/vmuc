// SPDX-License-Identifier: Apache-2.0
//! What a virtio device model implements, and the worker thread most of
//! them run their queues on.
//!
//! The transport (vCPU threads) owns configuration; a device's worker
//! thread owns its queues between activation and reset. A queue kick is
//! a lock-free [`Doorbell`] ring; activation and reset are messages to
//! the worker, and reset returns only after the worker has let go of the
//! queues. Interrupts go through [`IrqStatus`], which keeps the
//! InterruptStatus bits and the SPI level consistent.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::thread::{self, JoinHandle, Thread};

use crate::irq::Interrupt;
use crate::mem::GuestMemory;
use crate::queue::Queue;

/// Device type IDs (virtio 1.2 §5).
pub mod device_type {
    pub const NET: u32 = 1;
    pub const BLOCK: u32 = 2;
    pub const CONSOLE: u32 = 3;
    pub const RNG: u32 = 4;
    pub const BALLOON: u32 = 5;
    pub const VSOCK: u32 = 19;
    pub const FS: u32 = 26;
}

/// Transport-level feature bits.
pub mod features {
    pub const RING_INDIRECT_DESC: u64 = 1 << 28;
    pub const RING_EVENT_IDX: u64 = 1 << 29;
    pub const VERSION_1: u64 = 1 << 32;
}

/// InterruptStatus bits.
pub const INT_VRING: u32 = 1;
pub const INT_CONFIG: u32 = 2;

/// The InterruptStatus register and the SPI behind it. Workers raise
/// bits, the driver acknowledges them; the line is asserted exactly
/// while some bit is set. One small lock keeps bits and level in step
/// (lock-free set/ack races can leave the line asserted with no status,
/// an interrupt storm); it is taken once per batch, not per buffer.
pub struct IrqStatus {
    status: Mutex<u32>,
    line: Arc<dyn Interrupt>,
}

impl std::fmt::Debug for IrqStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "IrqStatus({:#x})", self.status())
    }
}

impl IrqStatus {
    pub fn new(line: Arc<dyn Interrupt>) -> Self {
        Self {
            status: Mutex::new(0),
            line,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, u32> {
        self.status.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Used buffers are ready.
    pub fn signal_used(&self) {
        self.raise(INT_VRING);
    }

    /// The configuration space changed.
    pub fn signal_config(&self) {
        self.raise(INT_CONFIG);
    }

    fn raise(&self, bits: u32) {
        let mut s = self.lock();
        let was = *s;
        *s |= bits;
        if was == 0 {
            self.line.set_level(true);
        }
    }

    pub fn ack(&self, bits: u32) {
        let mut s = self.lock();
        let was = *s;
        *s &= !bits;
        if was != 0 && *s == 0 {
            self.line.set_level(false);
        }
    }

    pub fn status(&self) -> u32 {
        *self.lock()
    }
}

/// What the transport hands a device when the driver sets DRIVER_OK.
pub struct Activation {
    pub mem: GuestMemory,
    /// Every queue the device declared; unused ones have `ready == false`.
    pub queues: Vec<Queue>,
    pub irq: Arc<IrqStatus>,
    /// The features the driver accepted.
    pub features: u64,
}

/// A queue kick from the driver (a `QueueNotify` write). Called on a
/// vCPU thread: must not block.
pub trait Doorbell: Send + Sync {
    fn ring(&self, queue: u16);
}

/// A virtio device model behind the MMIO transport.
pub trait VirtioDevice: Send {
    fn device_type(&self) -> u32;
    /// Device feature bits; the transport adds `VIRTIO_F_VERSION_1`.
    fn features(&self) -> u64;
    fn queue_max_sizes(&self) -> Vec<u16>;
    fn read_config(&self, offset: u64, data: &mut [u8]);
    fn write_config(&mut self, _offset: u64, _data: &[u8]) {}
    fn activate(&mut self, act: Activation) -> Result<(), String>;
    /// Driver reset: when this returns the device no longer touches the
    /// queues or guest memory.
    fn reset(&mut self);
    /// The doorbell for queue kicks; fixed for the device's lifetime.
    fn doorbell(&self) -> Arc<dyn Doorbell>;
}

/// Doorbell bits: queues 0..=29, then two for the device's own use.
pub const HOST_EVENT: u32 = 1 << 30;
const CONTROL: u32 = 1 << 31;

/// A doorbell that wakes one long-lived thread: an atomic bit set plus
/// `unpark`. No locks on the kick path.
#[derive(Debug, Default)]
pub struct ParkDoorbell {
    pending: AtomicU32,
    thread: OnceLock<Thread>,
}

impl ParkDoorbell {
    /// Sets `bits` and wakes the worker.
    pub fn ring_bits(&self, bits: u32) {
        self.pending.fetch_or(bits, Ordering::SeqCst);
        if let Some(t) = self.thread.get() {
            t.unpark();
        }
    }

    /// Blocks the bound thread until some bit is set; returns and clears them.
    fn wait(&self) -> u32 {
        loop {
            let bits = self.pending.swap(0, Ordering::SeqCst);
            if bits != 0 {
                return bits;
            }
            thread::park();
        }
    }
}

impl Doorbell for ParkDoorbell {
    fn ring(&self, queue: u16) {
        if queue < 30 {
            self.ring_bits(1 << queue);
        }
    }
}

/// A doorbell for devices that run a `mio` event loop (vsock, net): the
/// bits are collected atomically and the loop's poll is woken, so queue
/// kicks and socket readiness share one wait.
#[derive(Debug)]
pub struct WakerDoorbell {
    pending: Arc<AtomicU32>,
    waker: mio::Waker,
    queues: u16,
}

impl WakerDoorbell {
    pub fn new(waker: mio::Waker, queues: u16) -> Self {
        Self {
            pending: Arc::new(AtomicU32::new(0)),
            waker,
            queues,
        }
    }

    /// The bits rung since the last call.
    pub fn take(&self) -> u32 {
        self.pending.swap(0, Ordering::SeqCst)
    }

    pub fn ring_bits(&self, bits: u32) {
        self.pending.fetch_or(bits, Ordering::SeqCst);
        if let Err(e) = self.waker.wake() {
            tracing::warn!("device waker: {e}");
        }
    }
}

impl Doorbell for WakerDoorbell {
    fn ring(&self, queue: u16) {
        if queue < self.queues {
            self.ring_bits(1 << queue);
        }
    }
}

/// The per-device logic a [`Worker`] drives.
pub trait QueueHandler: Send + 'static {
    /// Handles kicks for the queues in `bits` (bit n = queue n) and
    /// [`HOST_EVENT`]. Signal the driver through `ctx.irq`.
    fn process(&mut self, ctx: &mut WorkerCtx, bits: u32);
    /// Called when the driver resets the device.
    fn reset(&mut self) {}
}

/// What a handler works on between activation and reset.
pub struct WorkerCtx {
    pub mem: GuestMemory,
    pub queues: Vec<Queue>,
    pub irq: Arc<IrqStatus>,
    pub features: u64,
}

enum Ctrl {
    Activate(WorkerCtx),
    Reset(Sender<()>),
    Shutdown,
}

/// One thread per device for the device's whole life, so the doorbell's
/// thread handle never changes.
pub struct Worker {
    doorbell: Arc<ParkDoorbell>,
    ctrl: Sender<Ctrl>,
    thread: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for Worker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Worker").finish_non_exhaustive()
    }
}

impl Worker {
    pub fn spawn(name: &str, mut handler: impl QueueHandler) -> std::io::Result<Self> {
        let doorbell = Arc::new(ParkDoorbell::default());
        let (ctrl, rx) = mpsc::channel::<Ctrl>();
        let db = Arc::clone(&doorbell);
        let thread = thread::Builder::new().name(name.into()).spawn(move || {
            let _ = db.thread.set(thread::current());
            run(&db, &rx, &mut handler);
        })?;
        // Wait for the thread to bind so early rings are not lost.
        while doorbell.thread.get().is_none() {
            thread::yield_now();
        }
        Ok(Self {
            doorbell,
            ctrl,
            thread: Some(thread),
        })
    }

    pub fn doorbell(&self) -> Arc<ParkDoorbell> {
        Arc::clone(&self.doorbell)
    }

    pub fn activate(&self, act: Activation) -> Result<(), String> {
        let ctx = WorkerCtx {
            mem: act.mem,
            queues: act.queues,
            irq: act.irq,
            features: act.features,
        };
        self.ctrl
            .send(Ctrl::Activate(ctx))
            .map_err(|_| "device worker is gone".to_owned())?;
        // Queues may already hold buffers: process them as if kicked.
        self.doorbell.ring_bits(CONTROL | 0x3fff_ffff);
        Ok(())
    }

    /// Returns once the worker has dropped the queues and guest memory.
    pub fn reset(&self) {
        let (tx, rx) = mpsc::channel();
        if self.ctrl.send(Ctrl::Reset(tx)).is_ok() {
            self.doorbell.ring_bits(CONTROL);
            let _ = rx.recv();
        }
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.ctrl.send(Ctrl::Shutdown);
        self.doorbell.ring_bits(CONTROL);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn run(db: &ParkDoorbell, rx: &Receiver<Ctrl>, handler: &mut impl QueueHandler) {
    let mut ctx: Option<WorkerCtx> = None;
    loop {
        let bits = db.wait();
        if bits & CONTROL != 0 {
            while let Ok(msg) = rx.try_recv() {
                match msg {
                    Ctrl::Activate(c) => ctx = Some(c),
                    Ctrl::Reset(ack) => {
                        ctx = None;
                        handler.reset();
                        let _ = ack.send(());
                    }
                    Ctrl::Shutdown => return,
                }
            }
        }
        if let Some(c) = ctx.as_mut() {
            let live = (0..c.queues.len().min(30))
                .filter(|&q| c.queues[q].ready)
                .fold(HOST_EVENT, |m, q| m | (1 << q));
            if bits & live != 0 {
                handler.process(c, bits & live);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;
    use crate::irq::TestIrq;

    #[test]
    fn irq_status_drives_the_line_by_level() {
        let line = Arc::new(TestIrq::default());
        let irq = IrqStatus::new(line.clone());
        irq.signal_used();
        irq.signal_config();
        assert_eq!(irq.status(), INT_VRING | INT_CONFIG);
        irq.ack(INT_VRING);
        assert!(line.level());
        irq.ack(INT_CONFIG);
        assert!(!line.level());
        irq.ack(INT_CONFIG);
        assert_eq!(
            line.history(),
            vec![true, false],
            "no redundant line changes"
        );
    }

    struct Count(Arc<AtomicU32>);
    impl QueueHandler for Count {
        fn process(&mut self, _ctx: &mut WorkerCtx, bits: u32) {
            self.0.fetch_or(bits, Ordering::SeqCst);
        }
    }

    fn wait_until(f: impl Fn() -> bool) {
        let t = Instant::now();
        while !f() {
            assert!(t.elapsed() < Duration::from_secs(5), "timed out");
            thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn worker_sees_kicks_only_for_ready_queues_while_active() {
        let seen = Arc::new(AtomicU32::new(0));
        let w = Worker::spawn("test-worker", Count(seen.clone())).unwrap();
        w.doorbell().ring(0);
        thread::sleep(Duration::from_millis(20));
        assert_eq!(seen.load(Ordering::SeqCst), 0, "not active yet");

        let mut q0 = Queue::new(8);
        q0.ready = true;
        w.activate(Activation {
            mem: GuestMemory::from_vec(0, vec![0; 64]),
            queues: vec![q0, Queue::new(8)],
            irq: Arc::new(IrqStatus::new(Arc::new(TestIrq::default()))),
            features: 0,
        })
        .unwrap();
        wait_until(|| seen.load(Ordering::SeqCst) & 1 != 0);
        assert_eq!(seen.load(Ordering::SeqCst) & 2, 0, "queue 1 is not ready");

        w.reset();
        seen.store(0, Ordering::SeqCst);
        w.doorbell().ring(0);
        thread::sleep(Duration::from_millis(20));
        assert_eq!(seen.load(Ordering::SeqCst), 0, "reset drops the queues");
    }
}
