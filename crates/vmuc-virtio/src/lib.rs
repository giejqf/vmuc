// SPDX-License-Identifier: Apache-2.0
//! Split virtqueues, virtio-mmio transport and device models (console,
//! blk, net, vsock, rng, balloon, fs), plus the primitives every device
//! uses: a view of guest memory, an interrupt line, and the MMIO device
//! trait.
//!
//! Device models take a [`GuestMemory`] and their queues, never a VM
//! handle, so they are unit-tested against byte buffers on any host.

pub mod bus;
pub mod device;
pub mod devices;
pub mod irq;
pub mod mem;
pub mod mmio;
pub mod queue;

pub use bus::BusDevice;
pub use device::{Activation, Doorbell, IrqStatus, VirtioDevice};
pub use irq::{Interrupt, TestIrq};
pub use mem::{Gpa, GuestMemory, MemError, MemResult, Pod};
pub use mmio::MmioTransport;
pub use queue::{Desc, DescChain, Queue, QueueError};
