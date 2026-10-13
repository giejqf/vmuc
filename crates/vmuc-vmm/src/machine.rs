// SPDX-License-Identifier: Apache-2.0
//! State the vCPU threads and the controlling thread share: each vCPU's
//! PSCI power state, the framework handles `hv_vcpus_exit` needs, the
//! one reason the VM stops, and pausing for a snapshot. Every
//! power-state transition happens here, so the PSCI answers are testable
//! without a hypervisor.

use std::ops::Range;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Condvar, Mutex, MutexGuard, PoisonError};

use tracing::{debug, warn};
use vmuc_hv::{HvVcpu, VcpuState};

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
    Starting {
        entry: u64,
        context: u64,
    },
    /// Restored from a snapshot; the state is in the slot's `resume`.
    Resuming,
    On,
}

struct CpuSlot {
    power: Mutex<Power>,
    wake: Condvar,
    /// A snapshot's vCPU state and how far to shift its vtimer offset.
    resume: Mutex<Option<(VcpuState, u64)>>,
}

/// What a parked vCPU is woken to do.
#[derive(Debug)]
pub enum PowerOn {
    /// Reset and enter at `entry` with X0 = `context` (boot, CPU_ON).
    Start { entry: u64, context: u64 },
    /// Take a snapshot's state, with the vtimer offset shifted by
    /// `offset_shift`, and carry on.
    Resume {
        state: Box<VcpuState>,
        offset_shift: u64,
    },
}

/// A vCPU in a snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CpuSnapshot {
    Off,
    /// Powered on by CPU_ON but not yet run.
    Starting {
        entry: u64,
        context: u64,
    },
    Running(Box<VcpuState>),
}

#[derive(Default)]
struct Pause {
    requested: bool,
    captured: Vec<Option<CpuSnapshot>>,
}

/// Exit counters, for `vmuc stat` and for finding what keeps an idle
/// guest busy.
#[derive(Debug, Default)]
pub struct ExitCounts {
    pub mmio: AtomicU64,
    pub sysreg: AtomicU64,
    pub hvc: AtomicU64,
    pub canceled: AtomicU64,
    /// Guest touched RAM mid-release (unmapped for microseconds): retried.
    pub reclaim_faults: AtomicU64,
    pub other: AtomicU64,
}

/// State every vCPU thread and the controlling thread share.
pub struct Machine {
    bus: MmioBus,
    ram: Range<u64>,
    cpus: Vec<CpuSlot>,
    /// Framework handles by vCPU index, for `hv_vcpus_exit`; filled as
    /// vCPUs are created.
    ids: Mutex<Vec<Option<u64>>>,
    stopping: AtomicBool,
    exit: Mutex<Option<VmExit>>,
    exited: Condvar,
    pausing: AtomicBool,
    pause: Mutex<Pause>,
    paused: Condvar,
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
            ram: 0..0,
            cpus: (0..cpus)
                .map(|_| CpuSlot {
                    power: Mutex::new(Power::Off),
                    wake: Condvar::new(),
                    resume: Mutex::new(None),
                })
                .collect(),
            ids: Mutex::new(vec![None; cpus as usize]),
            stopping: AtomicBool::new(false),
            exit: Mutex::new(None),
            exited: Condvar::new(),
            pausing: AtomicBool::new(false),
            pause: Mutex::new(Pause::default()),
            paused: Condvar::new(),
            counts: ExitCounts::default(),
        }
    }

    /// Guest RAM's IPA range: aborts there are reclaim races, not MMIO.
    #[must_use]
    pub fn with_ram(mut self, ram: Range<u64>) -> Self {
        self.ram = ram;
        self
    }

    pub fn is_ram(&self, addr: u64) -> bool {
        self.ram.contains(&addr)
    }

    pub fn bus(&self) -> &MmioBus {
        &self.bus
    }

    /// Records vCPU `index`'s framework handle for `hv_vcpus_exit`.
    pub fn register_vcpu(&self, index: usize, id: u64) {
        lock(&self.ids)[index] = Some(id);
    }

    /// The handles of the vCPUs `pick` selects by index.
    fn handles(&self, pick: impl Fn(usize) -> bool) -> Vec<u64> {
        lock(&self.ids)
            .iter()
            .enumerate()
            .filter_map(|(i, id)| id.filter(|_| pick(i)))
            .collect()
    }

    /// Powers on vCPU `index` at `entry` with X0 = `context` (boot).
    pub fn power_on(&self, index: usize, entry: u64, context: u64) {
        let slot = &self.cpus[index];
        *lock(&slot.power) = Power::Starting { entry, context };
        slot.wake.notify_one();
    }

    /// Hands vCPU `index` a snapshot's state to resume with, before it
    /// first runs.
    pub fn resume_cpu(&self, index: usize, state: VcpuState, offset_shift: u64) {
        let slot = &self.cpus[index];
        *lock(&slot.resume) = Some((state, offset_shift));
        *lock(&slot.power) = Power::Resuming;
        slot.wake.notify_one();
    }

    /// Parks vCPU `index` until it is powered on (or resumed), then marks
    /// it on; `None` once the VM is stopping. A parked vCPU reports itself
    /// to a pause and waits it out.
    pub fn wait_power_on(&self, index: usize) -> Option<PowerOn> {
        let slot = &self.cpus[index];
        loop {
            let mut power = lock(&slot.power);
            let parked_as = loop {
                if self.stopping() {
                    return None;
                }
                match *power {
                    Power::Starting { entry, context } if !self.pause_pending(index) => {
                        *power = Power::On;
                        return Some(PowerOn::Start { entry, context });
                    }
                    Power::Resuming => {
                        *power = Power::On;
                        let (state, offset_shift) = lock(&slot.resume).take()?;
                        return Some(PowerOn::Resume {
                            state: Box::new(state),
                            offset_shift,
                        });
                    }
                    Power::Starting { entry, context } => {
                        break CpuSnapshot::Starting { entry, context };
                    }
                    Power::Off | Power::On if self.pause_pending(index) => break CpuSnapshot::Off,
                    _ => {}
                }
                power = slot
                    .wake
                    .wait(power)
                    .unwrap_or_else(PoisonError::into_inner);
            };
            drop(power);
            if !self.report_paused(index, parked_as) {
                return None;
            }
        }
    }

    pub fn pause_requested(&self) -> bool {
        self.pausing.load(Ordering::SeqCst)
    }

    /// A pause is on and vCPU `index` has not reported to it yet.
    fn pause_pending(&self, index: usize) -> bool {
        self.pause_requested()
            && lock(&self.pause)
                .captured
                .get(index)
                .is_some_and(Option::is_none)
    }

    /// Stops every vCPU at an instruction boundary and collects what each
    /// was doing: running ones capture themselves on their own threads,
    /// parked ones say whether they are off or starting. `None` if the VM
    /// stopped meanwhile. The vCPUs stay paused until [`Machine::resume`]
    /// or a stop.
    pub fn pause(&self) -> Option<Vec<CpuSnapshot>> {
        {
            let mut p = lock(&self.pause);
            p.captured = vec![None; self.cpus.len()];
            p.requested = true;
            self.pausing.store(true, Ordering::SeqCst);
        }
        for slot in &self.cpus {
            let _guard = lock(&slot.power);
            slot.wake.notify_one();
        }
        let mut p = lock(&self.pause);
        loop {
            if self.stopping() {
                return None;
            }
            if p.captured.iter().all(Option::is_some) {
                return Some(p.captured.iter().flatten().cloned().collect());
            }
            // Again until all have reported: a cancel can be used up by
            // an exit already on its way, and a vCPU between exits may
            // have looked for the pause just before it began.
            let ids = self.handles(|i| p.captured[i].is_none());
            if let Err(e) = HvVcpu::force_exit(&ids) {
                warn!("hv_vcpus_exit: {e}");
            }
            p = self
                .paused
                .wait_timeout(p, std::time::Duration::from_millis(2))
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }

    /// Lets paused vCPUs carry on.
    pub fn resume(&self) {
        let mut p = lock(&self.pause);
        p.requested = false;
        self.pausing.store(false, Ordering::SeqCst);
        self.paused.notify_all();
    }

    /// vCPU `index` reports what it was doing and waits while the VM is
    /// paused; `false` when the VM stops instead of resuming.
    pub fn report_paused(&self, index: usize, what: CpuSnapshot) -> bool {
        let mut p = lock(&self.pause);
        if let Some(slot) = p.captured.get_mut(index) {
            *slot = Some(what);
        }
        self.paused.notify_all();
        while p.requested && !self.stopping() {
            p = self.paused.wait(p).unwrap_or_else(PoisonError::into_inner);
        }
        !self.stopping()
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
            Power::On | Power::Resuming => ret::ALREADY_ON,
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
                Power::On | Power::Resuming => AffinityState::On as i64,
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
        if let Err(e) = HvVcpu::force_exit(&self.handles(|_| true)) {
            warn!("hv_vcpus_exit: {e}");
        }
        {
            let _guard = lock(&self.pause);
            self.paused.notify_all();
        }
        self.exited.notify_all();
    }

    /// Forces every vCPU out of the guest again (a stop's cancel can be
    /// used up by an exit already on its way).
    pub fn kick(&self) {
        if let Err(e) = HvVcpu::force_exit(&self.handles(|_| true)) {
            warn!("hv_vcpus_exit: {e}");
        }
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

    /// [`Machine::wait_exit`] for at most `timeout`; `None` if the VM is
    /// still running.
    pub fn wait_exit_timeout(&self, timeout: std::time::Duration) -> Option<VmExit> {
        let exit = lock(&self.exit);
        let (exit, _) = self
            .exited
            .wait_timeout_while(exit, timeout, |e| e.is_none())
            .unwrap_or_else(PoisonError::into_inner);
        exit.clone()
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
        assert!(matches!(
            parked.join().unwrap(),
            Some(PowerOn::Start {
                entry: 0x4000_1000,
                context: 7
            })
        ));
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
        assert!(parked.join().unwrap().is_none());
        assert_eq!(waiter.join().unwrap(), VmExit::PowerOff);
        assert!(m.stopping());
    }

    #[test]
    fn a_pause_collects_every_vcpu_and_a_resume_lets_them_go() {
        let m = Arc::new(Machine::new(3, MmioBus::default()));
        // vCPU 0 runs (here: polls for the pause, as its exit loop does),
        // 1 is off, 2 was started by CPU_ON but has not run yet.
        let running = {
            let m = m.clone();
            std::thread::spawn(move || {
                while !m.pause_requested() {
                    std::thread::yield_now();
                }
                let state = VcpuState {
                    regs: vec![7],
                    ..VcpuState::default()
                };
                m.report_paused(0, CpuSnapshot::Running(Box::new(state)))
            })
        };
        let off = {
            let m = m.clone();
            std::thread::spawn(move || m.wait_power_on(1))
        };
        let starting = {
            let m = m.clone();
            m.power_on(2, 0x4000_2000, 9);
            // Reports itself and stays parked while paused.
            std::thread::spawn(move || {
                while !m.pause_requested() {
                    std::thread::yield_now();
                }
                m.wait_power_on(2)
            })
        };
        let cpus = m.pause().unwrap();
        assert!(matches!(&cpus[0], CpuSnapshot::Running(s) if s.regs == [7]));
        assert_eq!(cpus[1], CpuSnapshot::Off);
        assert_eq!(
            cpus[2],
            CpuSnapshot::Starting {
                entry: 0x4000_2000,
                context: 9
            }
        );
        m.resume();
        assert!(running.join().unwrap(), "resumed, not stopped");
        assert!(matches!(
            starting.join().unwrap(),
            Some(PowerOn::Start {
                entry: 0x4000_2000,
                context: 9
            })
        ));
        m.resume_cpu(1, VcpuState::default(), 5);
        assert!(matches!(
            off.join().unwrap(),
            Some(PowerOn::Resume {
                offset_shift: 5,
                ..
            })
        ));
    }

    #[test]
    fn a_stop_ends_a_pause() {
        let m = Arc::new(Machine::new(1, MmioBus::default()));
        let waiter = {
            let m = m.clone();
            std::thread::spawn(move || m.wait_power_on(0))
        };
        let pausing = {
            let m = m.clone();
            std::thread::spawn(move || {
                while !m.pause_requested() {
                    std::thread::yield_now();
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
                m.request_exit(VmExit::Stopped);
            })
        };
        // vCPU 0 reports Off; then the stop wakes everyone.
        let _ = m.pause();
        pausing.join().unwrap();
        assert!(waiter.join().unwrap().is_none());
    }
}
