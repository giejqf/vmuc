// SPDX-License-Identifier: Apache-2.0
//! virtio-vsock: host-initiated streams to guest ports.
//!
//! A host process asks the VMM (over `vmm.sock`) to connect to a guest
//! port; the VMM hands the client's unix socket to [`VsockConnector`],
//! the muxer sends the guest a REQUEST, and once the guest answers the
//! client is told (`on_connect`) and the socket carries raw bytes. The
//! agent's port 7700, `vmuc shell`'s ssh tunnel and port forwards all
//! ride on this. Guest-initiated connections are refused for now.

pub mod conn;
mod muxer;
pub mod packet;

use std::os::unix::net::UnixStream;
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::{self, JoinHandle};

use mio::{Poll, Waker};

pub use conn::OnConnect;
use muxer::{CONTROL, Ctrl, Muxer, WAKER};

use crate::device::{
    Activation, Doorbell, VirtioDevice, WakerDoorbell, WorkerCtx, device_type, features,
};

/// The guest's CID: the first one available to guests.
pub const GUEST_CID: u64 = 3;

/// Opens host-initiated streams; cheap to clone, any thread.
#[derive(Clone)]
pub struct VsockConnector {
    ctrl: Arc<Mutex<Sender<Ctrl>>>,
    doorbell: Arc<WakerDoorbell>,
}

impl std::fmt::Debug for VsockConnector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("VsockConnector")
    }
}

impl VsockConnector {
    /// Connects `stream` to `guest_port`. `on_connect` runs on the muxer
    /// thread with the outcome before any byte moves; on failure the
    /// stream is closed after it returns.
    pub fn connect(
        &self,
        stream: UnixStream,
        guest_port: u32,
        on_connect: OnConnect,
    ) -> std::io::Result<()> {
        stream.set_nonblocking(true)?;
        self.ctrl
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .send(Ctrl::Connect(stream, guest_port, on_connect))
            .map_err(|_| std::io::Error::other("vsock device is gone"))?;
        self.doorbell.ring_bits(CONTROL);
        Ok(())
    }
}

pub struct Vsock {
    guest_cid: u64,
    connector: VsockConnector,
    thread: Option<JoinHandle<()>>,
}

impl Vsock {
    pub fn new(guest_cid: u64) -> std::io::Result<Self> {
        let poll = Poll::new()?;
        let doorbell = Arc::new(WakerDoorbell::new(Waker::new(poll.registry(), WAKER)?, 3));
        let (tx, rx) = mpsc::channel();
        let muxer = Muxer::new(poll, guest_cid, rx, Arc::clone(&doorbell));
        let thread = thread::Builder::new()
            .name("virtio-vsock".into())
            .spawn(move || muxer.run())?;
        Ok(Self {
            guest_cid,
            connector: VsockConnector {
                ctrl: Arc::new(Mutex::new(tx)),
                doorbell,
            },
            thread: Some(thread),
        })
    }

    pub fn connector(&self) -> VsockConnector {
        self.connector.clone()
    }

    fn send(&self, msg: Ctrl) -> bool {
        let ok = self
            .connector
            .ctrl
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .send(msg)
            .is_ok();
        self.connector.doorbell.ring_bits(CONTROL);
        ok
    }
}

impl Drop for Vsock {
    fn drop(&mut self) {
        self.send(Ctrl::Shutdown);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl VirtioDevice for Vsock {
    fn device_type(&self) -> u32 {
        device_type::VSOCK
    }
    fn features(&self) -> u64 {
        features::RING_EVENT_IDX | features::RING_INDIRECT_DESC
    }
    fn queue_max_sizes(&self) -> Vec<u16> {
        // rx, tx, event
        vec![256, 256, 16]
    }
    fn read_config(&self, offset: u64, data: &mut [u8]) {
        let cfg = self.guest_cid.to_le_bytes();
        let off = offset as usize;
        for (i, b) in data.iter_mut().enumerate() {
            *b = cfg.get(off + i).copied().unwrap_or(0);
        }
    }
    fn activate(&mut self, act: Activation) -> Result<(), String> {
        let ctx = WorkerCtx {
            mem: act.mem,
            queues: act.queues,
            irq: act.irq,
            features: act.features,
        };
        if !self.send(Ctrl::Activate(ctx)) {
            return Err("vsock muxer is gone".into());
        }
        self.connector.doorbell.ring_bits(0b11);
        Ok(())
    }
    fn reset(&mut self) {
        let (tx, rx) = mpsc::channel();
        if self.send(Ctrl::Reset(tx)) {
            let _ = rx.recv();
        }
    }
    fn doorbell(&self) -> Arc<dyn Doorbell> {
        self.connector.doorbell.clone()
    }
}

#[cfg(test)]
mod tests;
