// SPDX-License-Identifier: Apache-2.0
//! State the vCPU threads and the controlling thread share: each vCPU's
//! PSCI power state, the framework handles `hv_vcpus_exit` needs, and
//! the one reason the VM stops. Every power-state transition happens
//! here, so the PSCI answers are testable without a hypervisor.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Condvar, Mutex, MutexGuard, PoisonError};

use tracing::{debug, warn};
use vmuc_hv::HvVcpu;

use crate::bus::MmioBus;
use crate::layout::mpidr;
use crate::psci::{AffinityState, ret};

/// Why the VM stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VmExit {
    /// The guest called PSCI SYSTEM_OFF.
    PowerOff,
    /// The guest called PSCI SYSTEM_RESET.
    Reset,
    /// The host asked it to stop.
    Stopped,
    /// A vCPU hit something the VMM cannot handle.
    Crashed(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Power {
    Off,
    Starting { entry: u64, context: u64 },
    On,
}

struct CpuSlot {
    power: Mutex<Power>,
    wake: Condvar,
}

/// Exit counters, for `vmuc stat` and for finding what keeps an idle
/// guest busy.
#[derive(Debug, Default)]
pub struct ExitCounts {
    pub mmio: AtomicU64,
    pub sysreg: AtomicU64,
    pub hvc: AtomicU64,
    pub canceled: AtomicU64,
    pub other: AtomicU64,
}

/// State every vCPU thread and the controlling thread share.
pub struct Machine {
    bus: MmioBus,
    cpus: Vec<CpuSlot>,
    /// Framework handles, for `hv_vcpus_exit`; filled as vCPUs are created.
    ids: Mutex<Vec<u64>>,
    stopping: AtomicBool,
    exit: Mutex<Option<VmExit>>,
    exited: Condvar,
    pub counts: ExitCounts,
}

impl std::fmt::Debug for Machine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Machine")
            .field("cpus", &self.cpus.len())
            .field("bus", &self.bus)
            .finish_non_exhaustive()
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Machine {
    pub fn new(cpus: u32, bus: MmioBus) -> Self {
        Self {
            bus,
            cpus: (0..cpus)
                .map(|_| CpuSlot {
                    power: Mutex::new(Power::Off),
                    wake: Condvar::new(),
                })
                .collect(),
            ids: Mutex::new(Vec::new()),
            stopping: AtomicBool::new(false),
            exit: Mutex::new(None),
            exited: Condvar::new(),
            counts: ExitCounts::default(),
        }
    }

    pub fn bus(&self) -> &MmioBus {
        &self.bus
    }

    /// Records a created vCPU's framework handle for `hv_vcpus_exit`.
    pub fn register_vcpu(&self, id: u64) {
        lock(&self.ids).push(id);
    }

    /// Powers on vCPU `index` at `entry` with X0 = `context` (boot).
    pub fn power_on(&self, index: usize, entry: u64, context: u64) {
        let slot = &self.cpus[index];
        *lock(&slot.power) = Power::Starting { entry, context };
        slot.wake.notify_one();
    }

    /// Parks vCPU `index` until it is powered on, then marks it on and
    /// returns `(entry, context)`; `None` once the VM is stopping.
    pub fn wait_power_on(&self, index: usize) -> Option<(u64, u64)> {
        let slot = &self.cpus[index];
        let mut power = lock(&slot.power);
        loop {
            if self.stopping() {
                return None;
            }
            if let Power::Starting { entry, context } = *power {
                *power = Power::On;
                return Some((entry, context));
            }
            power = slot
                .wake
                .wait(power)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }

    /// PSCI CPU_OFF on vCPU `index`.
    pub fn power_off(&self, index: usize) {
        *lock(&self.cpus[index].power) = Power::Off;
    }

    /// PSCI CPU_ON: the X0 the caller gets back.
    pub fn cpu_on(&self, target_mpidr: u64, entry: u64, context: u64) -> i64 {
        let Some(target) = self.index_of(target_mpidr) else {
            return ret::INVALID_PARAMETERS;
        };
        let slot = &self.cpus[target];
        let mut power = lock(&slot.power);
        match *power {
            Power::On => ret::ALREADY_ON,
            Power::Starting { .. } => ret::ON_PENDING,
            Power::Off => {
                debug!(target, entry = format_args!("{entry:#x}"), "CPU_ON");
                *power = Power::Starting { entry, context };
                slot.wake.notify_one();
                ret::SUCCESS
            }
        }
    }

    /// PSCI AFFINITY_INFO: the X0 the caller gets back.
    pub fn affinity_info(&self, target_mpidr: u64, lowest_level: u64) -> i64 {
        match self.index_of(target_mpidr) {
            Some(t) if lowest_level == 0 => match *lock(&self.cpus[t].power) {
                Power::On => AffinityState::On as i64,
                Power::Off => AffinityState::Off as i64,
                Power::Starting { .. } => AffinityState::OnPending as i64,
            },
            _ => ret::INVALID_PARAMETERS,
        }
    }

    /// Records why the VM stops (first caller wins) and gets every vCPU
    /// out of the guest: parked ones through their condvar, running ones
    /// through `hv_vcpus_exit`.
    pub fn request_exit(&self, why: VmExit) {
        {
            let mut exit = lock(&self.exit);
            if exit.is_none() {
                debug!(?why, "VM exit requested");
                *exit = Some(why);
            }
        }
        self.stopping.store(true, Ordering::SeqCst);
        for slot in &self.cpus {
            let _guard = lock(&slot.power);
            slot.wake.notify_one();
        }
        if let Err(e) = HvVcpu::force_exit(&lock(&self.ids)) {
            warn!("hv_vcpus_exit: {e}");
        }
        self.exited.notify_all();
    }

    pub fn stopping(&self) -> bool {
        self.stopping.load(Ordering::SeqCst)
    }

    /// Blocks until some thread has called [`Machine::request_exit`].
    pub fn wait_exit(&self) -> VmExit {
        let mut exit = lock(&self.exit);
        loop {
            if let Some(why) = exit.clone() {
                return why;
            }
            exit = self
                .exited
                .wait(exit)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }

    /// The vCPU index whose MPIDR is `target_mpidr`, if it exists.
    pub fn index_of(&self, target_mpidr: u64) -> Option<usize> {
        let index = ((target_mpidr >> 8) & 0xff) * 16 + (target_mpidr & 0xff);
        let index = usize::try_from(index).ok()?;
        (index < self.cpus.len() && mpidr(index as u32) == target_mpidr & 0xff_ffff)
            .then_some(index)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    #[test]
    fn mpidr_lookup_is_exact() {
        let m = Machine::new(20, MmioBus::default());
        assert_eq!(m.index_of(0), Some(0));
        assert_eq!(m.index_of(15), Some(15));
        assert_eq!(m.index_of(0x100), Some(16));
        assert_eq!(m.index_of(0x103), Some(19));
        assert_eq!(m.index_of(0x104), None);
        assert_eq!(m.index_of(16), None, "Aff0 above 15 is not a vCPU");
    }

    #[test]
    fn cpu_on_and_affinity_follow_psci_states() {
        let m = Arc::new(Machine::new(2, MmioBus::default()));
        assert_eq!(m.affinity_info(1, 0), AffinityState::Off as i64);
        assert_eq!(m.cpu_on(1, 0x4000_1000, 7), ret::SUCCESS);
        assert_eq!(m.affinity_info(1, 0), AffinityState::OnPending as i64);
        assert_eq!(m.cpu_on(1, 0x4000_1000, 7), ret::ON_PENDING);
        let parked = {
            let m = m.clone();
            std::thread::spawn(move || m.wait_power_on(1))
        };
        assert_eq!(parked.join().unwrap(), Some((0x4000_1000, 7)));
        assert_eq!(m.cpu_on(1, 0, 0), ret::ALREADY_ON);
        assert_eq!(m.affinity_info(1, 0), AffinityState::On as i64);
        assert_eq!(m.affinity_info(1, 1), ret::INVALID_PARAMETERS);
        assert_eq!(m.cpu_on(5, 0, 0), ret::INVALID_PARAMETERS);
        m.power_off(1);
        assert_eq!(m.affinity_info(1, 0), AffinityState::Off as i64);
    }

    #[test]
    fn stop_releases_parked_vcpus_and_first_reason_wins() {
        let m = Arc::new(Machine::new(2, MmioBus::default()));
        let parked = {
            let m = m.clone();
            std::thread::spawn(move || m.wait_power_on(1))
        };
        let waiter = {
            let m = m.clone();
            std::thread::spawn(move || m.wait_exit())
        };
        m.request_exit(VmExit::PowerOff);
        m.request_exit(VmExit::Stopped);
        assert_eq!(parked.join().unwrap(), None);
        assert_eq!(waiter.join().unwrap(), VmExit::PowerOff);
        assert!(m.stopping());
    }
}
