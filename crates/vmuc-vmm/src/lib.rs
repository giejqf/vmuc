// SPDX-License-Identifier: Apache-2.0
//! The virtual machine monitor: guest RAM, vCPU threads and exit
//! dispatch, PSCI, the in-kernel GIC, the device tree, the kernel loader
//! and the PrimeCell devices (PL011, PL031). Virtio devices live in
//! `vmuc-virtio` and plug into the MMIO bus here.

pub mod bus;
pub mod devices;
pub mod fdt;
pub mod layout;
pub mod loader;
pub mod machine;
pub mod memory;
pub mod psci;
pub mod vcpu;
pub mod vm;

pub use devices::console::ConsoleOut;
pub use machine::VmExit;
pub use vm::{Vm, VmBuilder, VmConfig, VmError};
