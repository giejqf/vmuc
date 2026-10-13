// SPDX-License-Identifier: Apache-2.0
//! The virtio-mmio transport, version 2 (virtio 1.2 §4.2.2).
//!
//! Register accesses arrive on vCPU threads. Everything except
//! `QueueNotify` goes through one lock (configuration, not the data
//! path); `QueueNotify` rings the device's doorbell without it.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use tracing::{debug, warn};

use crate::bus::{BusDevice, le_value, put_le};
use crate::device::{Activation, Doorbell, IrqStatus, ShmRegion, VirtioDevice, features};
use crate::mem::GuestMemory;
use crate::queue::Queue;

const MAGIC: u64 = 0x000;
const VERSION: u64 = 0x004;
const DEVICE_ID: u64 = 0x008;
const VENDOR_ID: u64 = 0x00c;
const DEVICE_FEATURES: u64 = 0x010;
const DEVICE_FEATURES_SEL: u64 = 0x014;
const DRIVER_FEATURES: u64 = 0x020;
const DRIVER_FEATURES_SEL: u64 = 0x024;
const QUEUE_SEL: u64 = 0x030;
const QUEUE_NUM_MAX: u64 = 0x034;
const QUEUE_NUM: u64 = 0x038;
const QUEUE_READY: u64 = 0x044;
const QUEUE_NOTIFY: u64 = 0x050;
const INTERRUPT_STATUS: u64 = 0x060;
const INTERRUPT_ACK: u64 = 0x064;
const STATUS: u64 = 0x070;
const QUEUE_DESC_LOW: u64 = 0x080;
const QUEUE_DESC_HIGH: u64 = 0x084;
const QUEUE_DRIVER_LOW: u64 = 0x090;
const QUEUE_DRIVER_HIGH: u64 = 0x094;
const QUEUE_DEVICE_LOW: u64 = 0x0a0;
const QUEUE_DEVICE_HIGH: u64 = 0x0a4;
const SHM_SEL: u64 = 0x0ac;
const SHM_LEN_LOW: u64 = 0x0b0;
const SHM_LEN_HIGH: u64 = 0x0b4;
const SHM_BASE_LOW: u64 = 0x0b8;
const SHM_BASE_HIGH: u64 = 0x0bc;
const CONFIG_GENERATION: u64 = 0x0fc;
const CONFIG: u64 = 0x100;

const MAGIC_VALUE: u32 = 0x7472_6976; // "virt"
/// "VMUC", little-endian.
const VENDOR: u32 = 0x4355_4d56;

pub mod status {
    pub const ACKNOWLEDGE: u32 = 1;
    pub const DRIVER: u32 = 2;
    pub const DRIVER_OK: u32 = 4;
    pub const FEATURES_OK: u32 = 8;
    pub const DEVICE_NEEDS_RESET: u32 = 64;
    pub const FAILED: u32 = 128;
}

/// What the driver set up in a transport, for snapshots, and the
/// device's own state.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TransportState {
    pub device_type: u32,
    pub status: u32,
    pub driver_features: u64,
    pub device_features_sel: u32,
    pub driver_features_sel: u32,
    pub queue_sel: u32,
    pub queues: Vec<QueueSetup>,
    pub config_generation: u32,
    pub shm_sel: u32,
    pub interrupt_status: u32,
    /// [`VirtioDevice::save_state`].
    #[serde(default)]
    pub device: serde_json::Value,
}

/// One queue as the driver configured it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct QueueSetup {
    pub size: u16,
    pub ready: bool,
    pub desc: u64,
    pub avail: u64,
    pub used: u64,
}

struct State {
    device: Box<dyn VirtioDevice>,
    status: u32,
    device_features: u64,
    device_features_sel: u32,
    driver_features: u64,
    driver_features_sel: u32,
    queue_sel: u32,
    queues: Vec<Queue>,
    config_generation: u32,
    active: bool,
    shm: Vec<ShmRegion>,
    shm_sel: u32,
}

pub struct MmioTransport {
    state: Mutex<State>,
    doorbell: Arc<dyn Doorbell>,
    irq: Arc<IrqStatus>,
    mem: GuestMemory,
}

impl std::fmt::Debug for MmioTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = self.lock();
        f.debug_struct("MmioTransport")
            .field("device_type", &s.device.device_type())
            .field("status", &s.status)
            .finish_non_exhaustive()
    }
}

impl MmioTransport {
    pub fn new(device: Box<dyn VirtioDevice>, mem: GuestMemory, irq: Arc<IrqStatus>) -> Self {
        let queues = device
            .queue_max_sizes()
            .into_iter()
            .map(Queue::new)
            .collect();
        let device_features = device.features() | features::VERSION_1;
        let shm = device.shm_regions();
        Self {
            doorbell: device.doorbell(),
            state: Mutex::new(State {
                device,
                status: 0,
                device_features,
                device_features_sel: 0,
                driver_features: 0,
                driver_features_sel: 0,
                queue_sel: 0,
                queues,
                config_generation: 0,
                active: false,
                shm,
                shm_sel: 0,
            }),
            irq,
            mem,
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The registers, queue setup and device state, for a snapshot of a
    /// paused VM whose devices are quiesced.
    pub fn save(&self) -> TransportState {
        let s = self.lock();
        TransportState {
            device_type: s.device.device_type(),
            status: s.status,
            driver_features: s.driver_features,
            device_features_sel: s.device_features_sel,
            driver_features_sel: s.driver_features_sel,
            queue_sel: s.queue_sel,
            queues: s
                .queues
                .iter()
                .map(|q| QueueSetup {
                    size: q.size,
                    ready: q.ready,
                    desc: q.desc,
                    avail: q.avail,
                    used: q.used,
                })
                .collect(),
            config_generation: s.config_generation,
            shm_sel: s.shm_sel,
            interrupt_status: self.irq.status(),
            device: s.device.save_state(),
        }
    }

    /// Puts a saved setup back, before any vCPU runs. A device the driver
    /// had finished setting up is activated with each queue picking up
    /// where its used ring in (restored) guest memory left off.
    pub fn restore(&self, st: &TransportState) -> Result<(), String> {
        let mut s = self.lock();
        if st.device_type != s.device.device_type() || st.queues.len() != s.queues.len() {
            return Err(format!(
                "snapshot has device type {} with {} queues here, not {} with {}",
                st.device_type,
                st.queues.len(),
                s.device.device_type(),
                s.queues.len()
            ));
        }
        s.status = st.status;
        s.driver_features = st.driver_features;
        s.device_features_sel = st.device_features_sel;
        s.driver_features_sel = st.driver_features_sel;
        s.queue_sel = st.queue_sel;
        s.config_generation = st.config_generation;
        s.shm_sel = st.shm_sel;
        for (q, c) in s.queues.iter_mut().zip(&st.queues) {
            q.size = c.size;
            q.ready = c.ready;
            q.desc = c.desc;
            q.avail = c.avail;
            q.used = c.used;
        }
        s.device.restore_state(st.device.clone())?;
        self.reactivate_locked(&mut s)?;
        self.irq.restore(st.interrupt_status);
        Ok(())
    }

    /// Before a snapshot, with the vCPUs paused: see
    /// [`VirtioDevice::quiesce`]. The setup stays for `save`.
    pub fn quiesce(&self, timeout: std::time::Duration) -> Result<(), String> {
        let mut s = self.lock();
        if s.active {
            s.device.quiesce(timeout)?;
            s.active = false;
        }
        Ok(())
    }

    /// Activates a quiesced device again (a snapshot that did not go
    /// ahead), its queues picking up at their used rings.
    pub fn reactivate(&self) -> Result<(), String> {
        let mut s = self.lock();
        if s.active {
            return Ok(());
        }
        self.reactivate_locked(&mut s)
    }

    fn reactivate_locked(&self, s: &mut State) -> Result<(), String> {
        for q in &mut s.queues {
            if q.ready && q.is_valid() {
                q.resume(&self.mem).map_err(|e| e.to_string())?;
            }
        }
        if s.status & status::DRIVER_OK != 0 && s.status & status::FEATURES_OK != 0 {
            self.activate(s);
        }
        Ok(())
    }

    /// Stops the device for good (VM teardown): its workers stop touching
    /// guest memory before this returns.
    pub fn shutdown(&self) {
        let mut s = self.lock();
        if s.active {
            s.device.reset();
            s.active = false;
        }
    }

    fn reset(&self, s: &mut State) {
        if s.active {
            s.device.reset();
        }
        s.active = false;
        s.status = 0;
        s.driver_features = 0;
        s.device_features_sel = 0;
        s.driver_features_sel = 0;
        s.queue_sel = 0;
        s.queues.iter_mut().for_each(Queue::reset);
        self.irq.ack(u32::MAX);
    }

    fn set_status(&self, s: &mut State, v: u32) {
        if v == 0 {
            debug!(device = s.device.device_type(), "driver reset");
            return self.reset(s);
        }
        let mut v = v;
        if v & status::FEATURES_OK != 0
            && s.status & status::FEATURES_OK == 0
            && (s.driver_features & !s.device_features != 0
                || s.driver_features & features::VERSION_1 == 0)
        {
            warn!(
                device = s.device.device_type(),
                driver = format_args!("{:#x}", s.driver_features),
                "driver accepted features the device does not offer"
            );
            v &= !status::FEATURES_OK;
        }
        s.status = v;
        if v & status::DRIVER_OK != 0 && v & status::FEATURES_OK != 0 && !s.active {
            self.activate(s);
        }
    }

    fn activate(&self, s: &mut State) {
        let event_idx = s.driver_features & features::RING_EVENT_IDX != 0;
        for q in &mut s.queues {
            q.event_idx = event_idx;
        }
        let bad = s.queues.iter().position(|q| q.ready && !q.is_valid());
        let result = match bad {
            Some(i) => Err(format!("queue {i} is misconfigured")),
            None => s.device.activate(Activation {
                mem: self.mem.clone(),
                queues: s.queues.clone(),
                irq: Arc::clone(&self.irq),
                features: s.driver_features,
            }),
        };
        match result {
            Ok(()) => s.active = true,
            Err(e) => {
                warn!(device = s.device.device_type(), "activation failed: {e}");
                s.status |= status::DEVICE_NEEDS_RESET;
                self.irq.signal_config();
            }
        }
    }

    fn queue(s: &mut State) -> Option<&mut Queue> {
        let sel = s.queue_sel as usize;
        s.queues.get_mut(sel)
    }
}

fn set_low(v: &mut u64, low: u32) {
    *v = (*v & !0xffff_ffff) | u64::from(low);
}

fn set_high(v: &mut u64, high: u32) {
    *v = (*v & 0xffff_ffff) | (u64::from(high) << 32);
}

impl BusDevice for MmioTransport {
    fn read(&self, offset: u64, data: &mut [u8]) {
        let s = self.lock();
        if offset >= CONFIG {
            return s.device.read_config(offset - CONFIG, data);
        }
        let sel_queue = s.queues.get(s.queue_sel as usize);
        let v: u32 = match offset {
            MAGIC => MAGIC_VALUE,
            VERSION => 2,
            DEVICE_ID => s.device.device_type(),
            VENDOR_ID => VENDOR,
            DEVICE_FEATURES => match s.device_features_sel {
                0 => s.device_features as u32,
                1 => (s.device_features >> 32) as u32,
                _ => 0,
            },
            QUEUE_NUM_MAX => sel_queue.map_or(0, |q| u32::from(q.max_size)),
            QUEUE_READY => sel_queue.map_or(0, |q| u32::from(q.ready)),
            INTERRUPT_STATUS => self.irq.status(),
            STATUS => s.status,
            // Length all ones means the selected region is absent.
            SHM_LEN_LOW | SHM_LEN_HIGH | SHM_BASE_LOW | SHM_BASE_HIGH => {
                let r = s.shm.iter().find(|r| u32::from(r.id) == s.shm_sel);
                let v = match offset {
                    SHM_LEN_LOW | SHM_LEN_HIGH => r.map_or(u64::MAX, |r| r.len),
                    _ => r.map_or(u64::MAX, |r| r.base),
                };
                if offset & 4 == 0 {
                    v as u32
                } else {
                    (v >> 32) as u32
                }
            }
            CONFIG_GENERATION => s.config_generation,
            _ => 0,
        };
        put_le(data, u64::from(v));
    }

    fn write(&self, offset: u64, data: &[u8]) {
        let v = le_value(data) as u32;
        if offset == QUEUE_NOTIFY {
            return self.doorbell.ring(v as u16);
        }
        let mut s = self.lock();
        if offset >= CONFIG {
            return s.device.write_config(offset - CONFIG, data);
        }
        let configurable = s.status & status::DRIVER_OK == 0;
        match offset {
            DEVICE_FEATURES_SEL => s.device_features_sel = v,
            DRIVER_FEATURES_SEL => s.driver_features_sel = v,
            DRIVER_FEATURES => match s.driver_features_sel {
                0 => set_low(&mut s.driver_features, v),
                1 => set_high(&mut s.driver_features, v),
                _ => {}
            },
            QUEUE_SEL => s.queue_sel = v,
            QUEUE_NUM | QUEUE_READY | QUEUE_DESC_LOW | QUEUE_DESC_HIGH | QUEUE_DRIVER_LOW
            | QUEUE_DRIVER_HIGH | QUEUE_DEVICE_LOW | QUEUE_DEVICE_HIGH
                if configurable =>
            {
                if let Some(q) = Self::queue(&mut s) {
                    match offset {
                        QUEUE_NUM => q.size = v as u16,
                        QUEUE_READY => q.ready = v == 1,
                        QUEUE_DESC_LOW => set_low(&mut q.desc, v),
                        QUEUE_DESC_HIGH => set_high(&mut q.desc, v),
                        QUEUE_DRIVER_LOW => set_low(&mut q.avail, v),
                        QUEUE_DRIVER_HIGH => set_high(&mut q.avail, v),
                        QUEUE_DEVICE_LOW => set_low(&mut q.used, v),
                        _ => set_high(&mut q.used, v),
                    }
                }
            }
            INTERRUPT_ACK => self.irq.ack(v),
            STATUS => self.set_status(&mut s, v),
            SHM_SEL => s.shm_sel = v,
            _ => debug!(offset, v, "virtio-mmio: ignored write"),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::*;
    use crate::device::ParkDoorbell;
    use crate::irq::TestIrq;

    #[derive(Default)]
    struct Probe {
        activated: Arc<AtomicU32>,
        resets: Arc<AtomicU32>,
        doorbell: Arc<ParkDoorbell>,
        shm: Vec<ShmRegion>,
        queues: Arc<Mutex<Vec<Queue>>>,
        /// Refuses to quiesce.
        busy: bool,
        restored: Arc<Mutex<Option<serde_json::Value>>>,
    }

    impl VirtioDevice for Probe {
        fn device_type(&self) -> u32 {
            4
        }
        fn features(&self) -> u64 {
            features::RING_EVENT_IDX
        }
        fn queue_max_sizes(&self) -> Vec<u16> {
            vec![256]
        }
        fn read_config(&self, offset: u64, data: &mut [u8]) {
            put_le(data, offset + 0x40);
        }
        fn activate(&mut self, act: Activation) -> Result<(), String> {
            assert!(act.queues[0].event_idx);
            self.activated.fetch_add(1, Ordering::SeqCst);
            *self.queues.lock().unwrap() = act.queues;
            Ok(())
        }
        fn reset(&mut self) {
            self.resets.fetch_add(1, Ordering::SeqCst);
        }
        fn quiesce(&mut self, _timeout: std::time::Duration) -> Result<(), String> {
            if self.busy {
                return Err("busy".into());
            }
            self.reset();
            Ok(())
        }
        fn save_state(&self) -> serde_json::Value {
            serde_json::json!({ "probe": 7 })
        }
        fn restore_state(&mut self, state: serde_json::Value) -> Result<(), String> {
            *self.restored.lock().unwrap() = Some(state);
            Ok(())
        }
        fn doorbell(&self) -> Arc<dyn Doorbell> {
            self.doorbell.clone()
        }
        fn shm_regions(&self) -> Vec<ShmRegion> {
            self.shm.clone()
        }
    }

    fn rd(t: &MmioTransport, off: u64) -> u32 {
        let mut d = [0u8; 4];
        t.read(off, &mut d);
        u32::from_le_bytes(d)
    }

    fn wr(t: &MmioTransport, off: u64, v: u32) {
        t.write(off, &v.to_le_bytes());
    }

    fn transport() -> (MmioTransport, Arc<AtomicU32>, Arc<AtomicU32>) {
        let p = Probe::default();
        let (a, r) = (p.activated.clone(), p.resets.clone());
        let irq = Arc::new(IrqStatus::new(Arc::new(TestIrq::default())));
        let t = MmioTransport::new(Box::new(p), GuestMemory::from_vec(0, vec![0; 4096]), irq);
        (t, a, r)
    }

    /// The Linux virtio-mmio probe sequence, condensed.
    fn bring_up(t: &MmioTransport, driver_features: u64) {
        wr(t, STATUS, 0);
        wr(t, STATUS, status::ACKNOWLEDGE | status::DRIVER);
        wr(t, DRIVER_FEATURES_SEL, 0);
        wr(t, DRIVER_FEATURES, driver_features as u32);
        wr(t, DRIVER_FEATURES_SEL, 1);
        wr(t, DRIVER_FEATURES, (driver_features >> 32) as u32);
        wr(
            t,
            STATUS,
            status::ACKNOWLEDGE | status::DRIVER | status::FEATURES_OK,
        );
        wr(t, QUEUE_SEL, 0);
        wr(t, QUEUE_NUM, 128);
        wr(t, QUEUE_DESC_LOW, 0x1000);
        wr(t, QUEUE_DRIVER_LOW, 0x1800);
        wr(t, QUEUE_DEVICE_LOW, 0x1a00);
        wr(t, QUEUE_READY, 1);
        wr(
            t,
            STATUS,
            status::ACKNOWLEDGE | status::DRIVER | status::FEATURES_OK | status::DRIVER_OK,
        );
    }

    #[test]
    fn a_restored_transport_takes_up_the_used_ring_where_it_was() {
        let mem = GuestMemory::from_vec(0, vec![0; 0x2000]);
        let probe = || {
            let p = Probe::default();
            let (a, q) = (p.activated.clone(), p.queues.clone());
            let irq = Arc::new(IrqStatus::new(Arc::new(TestIrq::default())));
            (MmioTransport::new(Box::new(p), mem.clone(), irq), a, q)
        };
        let (t, _, _) = probe();
        bring_up(&t, features::VERSION_1 | features::RING_EVENT_IDX);
        // Five chains returned; the sixth (head 9) taken and not returned.
        mem.store_u16_release(0x1a02, 5).unwrap();
        mem.store_u16_release(0x1802, 6).unwrap();
        mem.write_obj(0x1800 + 4 + 2 * 5, 9u16).unwrap();
        mem.write_obj(0x1000 + 16 * 9, 0x100u64).unwrap();
        mem.write_obj(0x1000 + 16 * 9 + 8, 4u32).unwrap();
        t.irq.signal_used();
        let saved = t.save();

        assert_eq!(saved.device, serde_json::json!({ "probe": 7 }));
        let (fresh, activated, queues) = probe();
        fresh.restore(&saved).unwrap();
        assert_eq!(activated.load(Ordering::SeqCst), 1);
        assert_eq!(fresh.save(), saved);
        assert_eq!(rd(&fresh, INTERRUPT_STATUS), saved.interrupt_status);
        let chain = queues.lock().unwrap()[0].pop(&mem).unwrap().unwrap();
        assert_eq!(chain.head, 9, "the taken chain is taken again");

        let (other, _, _) = transport();
        let mut wrong = saved.clone();
        wrong.device_type = 2;
        assert!(other.restore(&wrong).is_err());
    }

    /// A transport with a chain (head 9) taken and not returned.
    fn holding_a_chain(busy: bool) -> (MmioTransport, GuestMemory, Probe) {
        let mem = GuestMemory::from_vec(0, vec![0; 0x2000]);
        let p = Probe {
            busy,
            ..Probe::default()
        };
        let handles = Probe {
            activated: p.activated.clone(),
            resets: p.resets.clone(),
            queues: p.queues.clone(),
            restored: p.restored.clone(),
            ..Probe::default()
        };
        let irq = Arc::new(IrqStatus::new(Arc::new(TestIrq::default())));
        let t = MmioTransport::new(Box::new(p), mem.clone(), irq);
        bring_up(&t, features::VERSION_1 | features::RING_EVENT_IDX);
        mem.store_u16_release(0x1802, 1).unwrap();
        mem.write_obj(0x1800 + 4, 9u16).unwrap();
        mem.write_obj(0x1000 + 16 * 9, 0x100u64).unwrap();
        mem.write_obj(0x1000 + 16 * 9 + 8, 4u32).unwrap();
        let chain = handles.queues.lock().unwrap()[0].pop(&mem).unwrap();
        assert_eq!(chain.unwrap().head, 9);
        (t, mem, handles)
    }

    #[test]
    fn a_quiesced_device_activates_again_at_its_used_ring() {
        let (t, mem, p) = holding_a_chain(false);
        t.quiesce(std::time::Duration::from_secs(1)).unwrap();
        assert_eq!(p.resets.load(Ordering::SeqCst), 1);
        let saved = t.save();
        assert_ne!(saved.status & status::DRIVER_OK, 0, "the setup stays");
        t.reactivate().unwrap();
        assert_eq!(p.activated.load(Ordering::SeqCst), 2);
        let chain = p.queues.lock().unwrap()[0].pop(&mem).unwrap().unwrap();
        assert_eq!(chain.head, 9, "the unfinished chain is taken again");
        t.reactivate().unwrap();
        assert_eq!(p.activated.load(Ordering::SeqCst), 2, "already active");
        assert!(p.restored.lock().unwrap().is_none(), "not a restore");
    }

    #[test]
    fn a_device_that_cannot_quiesce_stays_active() {
        let (t, _, p) = holding_a_chain(true);
        assert_eq!(
            t.quiesce(std::time::Duration::from_secs(1)),
            Err("busy".into())
        );
        t.reactivate().unwrap();
        assert_eq!(p.activated.load(Ordering::SeqCst), 1, "never stopped");
        assert_eq!(p.resets.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn identifies_and_offers_features() {
        let (t, _, _) = transport();
        assert_eq!(rd(&t, MAGIC), MAGIC_VALUE);
        assert_eq!(rd(&t, VERSION), 2);
        assert_eq!(rd(&t, DEVICE_ID), 4);
        wr(&t, DEVICE_FEATURES_SEL, 1);
        assert_eq!(rd(&t, DEVICE_FEATURES), 1, "VERSION_1 is bit 32");
        wr(&t, DEVICE_FEATURES_SEL, 0);
        assert_eq!(rd(&t, DEVICE_FEATURES), 1 << 29);
        assert_eq!(rd(&t, QUEUE_NUM_MAX), 256);
        assert_eq!(rd(&t, CONFIG + 4), 0x44);
        assert_eq!(rd(&t, SHM_LEN_LOW), u32::MAX);
    }

    #[test]
    fn shared_memory_regions_are_found_by_id() {
        let p = Probe {
            shm: vec![ShmRegion {
                id: 0,
                base: 0x1_4000_0000,
                len: 0x8000_0000,
            }],
            ..Probe::default()
        };
        let irq = Arc::new(IrqStatus::new(Arc::new(TestIrq::default())));
        let t = MmioTransport::new(Box::new(p), GuestMemory::from_vec(0, vec![0; 4096]), irq);
        wr(&t, SHM_SEL, 0);
        assert_eq!(
            [
                rd(&t, SHM_LEN_LOW),
                rd(&t, SHM_LEN_HIGH),
                rd(&t, SHM_BASE_LOW),
                rd(&t, SHM_BASE_HIGH)
            ],
            [0x8000_0000, 0, 0x4000_0000, 1]
        );
        wr(&t, SHM_SEL, 1);
        assert_eq!(
            [rd(&t, SHM_LEN_LOW), rd(&t, SHM_LEN_HIGH)],
            [u32::MAX; 2],
            "absent"
        );
    }

    #[test]
    fn driver_ok_activates_and_reset_deactivates() {
        let (t, activated, resets) = transport();
        bring_up(&t, features::VERSION_1 | features::RING_EVENT_IDX);
        assert_eq!(activated.load(Ordering::SeqCst), 1);
        assert_eq!(rd(&t, QUEUE_READY), 1);
        // Queue registers are frozen once DRIVER_OK is set.
        wr(&t, QUEUE_NUM, 64);
        assert_eq!(t.lock().queues[0].size, 128);
        wr(&t, STATUS, 0);
        assert_eq!(resets.load(Ordering::SeqCst), 1);
        assert_eq!(rd(&t, STATUS), 0);
        assert_eq!(rd(&t, QUEUE_READY), 0);
    }

    #[test]
    fn unknown_features_or_missing_version_1_refuse_features_ok() {
        let (t, activated, _) = transport();
        bring_up(&t, features::RING_EVENT_IDX);
        assert_eq!(rd(&t, STATUS) & status::FEATURES_OK, 0);
        assert_eq!(activated.load(Ordering::SeqCst), 0);
        bring_up(&t, features::VERSION_1 | (1 << 40));
        assert_eq!(rd(&t, STATUS) & status::FEATURES_OK, 0);
    }

    #[test]
    fn misconfigured_queue_sets_needs_reset_and_raises_config_irq() {
        let (t, activated, _) = transport();
        wr(&t, STATUS, status::ACKNOWLEDGE | status::DRIVER);
        wr(&t, DRIVER_FEATURES_SEL, 1);
        wr(&t, DRIVER_FEATURES, 1);
        wr(
            &t,
            STATUS,
            status::ACKNOWLEDGE | status::DRIVER | status::FEATURES_OK,
        );
        wr(&t, QUEUE_NUM, 100); // not a power of two
        wr(&t, QUEUE_READY, 1);
        wr(
            &t,
            STATUS,
            status::ACKNOWLEDGE | status::DRIVER | status::FEATURES_OK | status::DRIVER_OK,
        );
        assert_eq!(activated.load(Ordering::SeqCst), 0);
        assert_ne!(rd(&t, STATUS) & status::DEVICE_NEEDS_RESET, 0);
        assert_eq!(rd(&t, INTERRUPT_STATUS), crate::device::INT_CONFIG);
        wr(&t, INTERRUPT_ACK, crate::device::INT_CONFIG);
        assert_eq!(rd(&t, INTERRUPT_STATUS), 0);
    }
}
