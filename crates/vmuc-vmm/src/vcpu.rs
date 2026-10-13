// SPDX-License-Identifier: Apache-2.0
//! vCPU threads and exit dispatch.
//!
//! Each vCPU is created once, on its own OS thread, and lives until the
//! VM is torn down; PSCI CPU_OFF parks the thread and CPU_ON wakes it
//! with a new entry point (recreating a vCPU deadlocks the framework).
//! PC rules, checked on hardware by `vmuc-hv`'s contract test: after a
//! data abort or a system-register trap the VMM advances PC; after HVC
//! the framework already has; after SMC the VMM must.

use std::sync::atomic::Ordering;

use tracing::{debug, error, warn};
use vmuc_hv::{ExceptionClass, HvResult, HvVcpu, MmioInfo, VcpuExit, reg, sys_reg};

use crate::layout::mpidr;
use crate::machine::{CpuSnapshot, Machine, PowerOn, VmExit};
use crate::psci::{self, PsciCall, ret};

/// EL1h, D, A, I and F masked: the arm64 boot protocol's entry state.
const CPSR_EL1H_MASKED: u64 = 0x3c5;

/// What the vCPU loop does after a PSCI call.
enum PsciOutcome {
    Return(u64),
    CpuOff,
}

/// Runs vCPU `index` until the VM stops. Called on the vCPU's own thread;
/// `created` reports the framework handle (or the failure) once.
pub fn run_vcpu(index: usize, m: &Machine, created: impl FnOnce(HvResult<u64>)) {
    let vcpu = match HvVcpu::new() {
        Ok(v) => v,
        Err(e) => return created(Err(e)),
    };
    // GICv3 routes by affinity, and the topology is final once vCPUs
    // run, so MPIDR is set before anything starts.
    if let Err(e) = vcpu.set_sys_reg(sys_reg::HV_SYS_REG_MPIDR_EL1, mpidr(index as u32)) {
        return created(Err(e));
    }
    let reset_sctlr = vcpu.get_sys_reg(sys_reg::HV_SYS_REG_SCTLR_EL1).unwrap_or(0);
    m.register_vcpu(index, vcpu.id());
    created(Ok(vcpu.id()));

    let mut ctx = Ctx {
        vcpu: &vcpu,
        m,
        index,
    };
    while let Some(on) = m.wait_power_on(index) {
        let ready = match on {
            PowerOn::Start { entry, context } => [
                (reg::HV_REG_PC, entry),
                (reg::HV_REG_X0, context),
                (reg::HV_REG_X1, 0),
                (reg::HV_REG_X2, 0),
                (reg::HV_REG_X3, 0),
                (reg::HV_REG_CPSR, CPSR_EL1H_MASKED),
            ]
            .iter()
            .try_for_each(|&(r, v)| vcpu.set_reg(r, v))
            .and_then(|()| vcpu.set_sys_reg(sys_reg::HV_SYS_REG_SCTLR_EL1, reset_sctlr)),
            PowerOn::Resume {
                state,
                offset_shift,
            } => vcpu.restore(&state, state.vtimer_offset.wrapping_add(offset_shift)),
        };
        if let Err(e) = ready {
            m.request_exit(VmExit::Crashed(format!("vCPU {index}: reset: {e}")));
            break;
        }
        if let Err(why) = ctx.run_until_off() {
            m.request_exit(why);
            break;
        }
    }
    debug!(vcpu = index, "vCPU thread exiting");
    // `vcpu` drops here, on its own thread.
}

struct Ctx<'a> {
    vcpu: &'a HvVcpu,
    m: &'a Machine,
    index: usize,
}

impl Ctx<'_> {
    /// Runs the guest until CPU_OFF (`Ok`) or until the VM must stop
    /// (`Err` with the reason, `Stopped` when someone else asked).
    fn run_until_off(&mut self) -> Result<(), VmExit> {
        loop {
            // Before every entry, not only on a cancel: an exit for MMIO
            // can come back instead of the cancel a pause asked for.
            if self.m.pause_requested() {
                let state = self
                    .vcpu
                    .capture()
                    .map_err(|e| self.crash(&format!("capture: {e}")))?;
                if !self
                    .m
                    .report_paused(self.index, CpuSnapshot::Running(Box::new(state)))
                {
                    return Err(VmExit::Stopped);
                }
            }
            let exit = self
                .vcpu
                .run()
                .map_err(|e| self.crash(&format!("hv_vcpu_run: {e}")))?;
            match exit {
                VcpuExit::Canceled => {
                    self.m.counts.canceled.fetch_add(1, Ordering::Relaxed);
                    if self.m.stopping() {
                        return Err(VmExit::Stopped);
                    }
                }
                VcpuExit::Exception { class, syndrome } => {
                    if self.exception(class, syndrome)? {
                        return Ok(());
                    }
                }
                VcpuExit::VtimerActivated => {
                    // The in-kernel GIC owns the vtimer; this would mean it
                    // does not, and the guest would never see a tick.
                    warn!(vcpu = self.index, "unexpected vtimer exit; unmasking");
                    self.m.counts.other.fetch_add(1, Ordering::Relaxed);
                    self.hv(self.vcpu.set_vtimer_mask(false))?;
                }
                VcpuExit::Unknown(r) => return Err(self.crash(&format!("unknown exit reason {r}"))),
            }
        }
    }

    /// Handles one trapped exception; `Ok(true)` when the vCPU powered off.
    fn exception(&mut self, class: ExceptionClass, syndrome: u64) -> Result<bool, VmExit> {
        match class {
            // RAM is only ever unmapped for the microseconds a release
            // takes (the guest promised not to touch reported pages, but a
            // stray access must not read zeros or lose a write): retry.
            ExceptionClass::DataAbort(MmioInfo { address, .. })
            | ExceptionClass::InstructionAbort { address, .. }
                if self.m.is_ram(address) =>
            {
                self.m.counts.reclaim_faults.fetch_add(1, Ordering::Relaxed);
                std::thread::yield_now();
            }
            ExceptionClass::DataAbort(m) => {
                self.m.counts.mmio.fetch_add(1, Ordering::Relaxed);
                self.hv(self.mmio(&m))?;
            }
            ExceptionClass::SystemRegister {
                is_write,
                rt,
                op0,
                op1,
                crn,
                crm,
                op2,
            } => {
                // Read-as-zero, write-ignored: Linux pokes debug and PMU
                // registers the framework leaves to us (OSDLR_EL1 first).
                self.m.counts.sysreg.fetch_add(1, Ordering::Relaxed);
                debug!(
                    vcpu = self.index,
                    op0, op1, crn, crm, op2, is_write, "sysreg RAZ/WI"
                );
                if !is_write && rt != 31 {
                    self.hv(self.vcpu.set_reg(u32::from(rt), 0))?;
                }
                self.hv(self.vcpu.skip_instruction())?;
            }
            ExceptionClass::HypercallHvc(imm) => {
                self.m.counts.hvc.fetch_add(1, Ordering::Relaxed);
                if self.smccc(imm)? {
                    return Ok(true);
                }
            }
            ExceptionClass::SmcCall(imm) => {
                self.m.counts.hvc.fetch_add(1, Ordering::Relaxed);
                self.hv(self.vcpu.skip_instruction())?;
                if self.smccc(imm)? {
                    return Ok(true);
                }
            }
            // Idles inside hv_vcpu_run with the in-kernel GIC; harmless.
            ExceptionClass::WaitForInterrupt => {
                self.m.counts.other.fetch_add(1, Ordering::Relaxed);
            }
            other => {
                return Err(self.crash(&format!(
                    "unhandled exception {other:?} (ESR {syndrome:#x})"
                )));
            }
        }
        Ok(false)
    }

    /// Emulates one MMIO access. Without ISV the access size and register
    /// are unknown (LDP/STP, atomics), so it is refused and skipped.
    fn mmio(&self, m: &MmioInfo) -> HvResult<()> {
        if !m.isv {
            warn!(
                vcpu = self.index,
                addr = format_args!("{:#x}", m.address),
                "data abort without ISV; skipped"
            );
            return self.vcpu.skip_instruction();
        }
        let size = usize::from(m.access_size);
        let device = self.m.bus().find(m.address);
        if device.is_none() {
            debug!(
                addr = format_args!("{:#x}", m.address),
                m.is_write, "MMIO to nothing"
            );
        }
        if m.is_write {
            let v = if m.register == 31 {
                0
            } else {
                self.vcpu.get_reg(u32::from(m.register))?
            };
            if let Some((dev, off)) = device {
                dev.write(off, &v.to_le_bytes()[..size]);
            }
        } else {
            let mut buf = [0u8; 8];
            if let Some((dev, off)) = device {
                dev.read(off, &mut buf[..size]);
            }
            let mut v = u64::from_le_bytes(buf);
            if m.sign_extend && size < 8 {
                let shift = 64 - 8 * size as u32;
                v = (((v << shift) as i64) >> shift) as u64;
            }
            if !m.sixty_four {
                v &= 0xffff_ffff;
            }
            if m.register != 31 {
                self.vcpu.set_reg(u32::from(m.register), v)?;
            }
        }
        self.vcpu.skip_instruction()
    }

    /// An SMCCC call through HVC or SMC (PC already past the instruction).
    /// `Ok(true)` when this vCPU executed CPU_OFF.
    fn smccc(&mut self, imm: u16) -> Result<bool, VmExit> {
        let x = [0, 1, 2, 3].map(|r| self.vcpu.get_reg(r).unwrap_or(0));
        let outcome = if imm == 0 {
            self.psci(PsciCall::decode(x))?
        } else {
            PsciOutcome::Return(ret::NOT_SUPPORTED as u64)
        };
        match outcome {
            PsciOutcome::Return(v) => {
                self.hv(self.vcpu.set_reg(reg::HV_REG_X0, v))?;
                Ok(false)
            }
            PsciOutcome::CpuOff => Ok(true),
        }
    }

    fn psci(&mut self, call: PsciCall) -> Result<PsciOutcome, VmExit> {
        let r = |v: i64| PsciOutcome::Return(v as u64);
        Ok(match call {
            PsciCall::Version => PsciOutcome::Return(psci::VERSION),
            PsciCall::MigrateInfoType => PsciOutcome::Return(psci::MIGRATE_NOT_REQUIRED),
            PsciCall::Features(id) => r(psci::features(id)),
            PsciCall::SystemOff => return Err(VmExit::PowerOff),
            PsciCall::SystemReset => return Err(VmExit::Reset),
            PsciCall::CpuOff => {
                self.m.power_off(self.index);
                debug!(vcpu = self.index, "CPU_OFF");
                PsciOutcome::CpuOff
            }
            PsciCall::CpuOn {
                target_mpidr,
                entry,
                context,
            } => r(self.m.cpu_on(target_mpidr, entry, context)),
            PsciCall::AffinityInfo {
                target_mpidr,
                lowest_level,
            } => r(self.m.affinity_info(target_mpidr, lowest_level)),
            PsciCall::Unsupported(id) => {
                debug!(
                    vcpu = self.index,
                    id = format_args!("{id:#x}"),
                    "unsupported SMCCC call"
                );
                r(ret::NOT_SUPPORTED)
            }
        })
    }

    fn hv<T>(&self, r: HvResult<T>) -> Result<T, VmExit> {
        r.map_err(|e| self.crash(&format!("hypervisor call failed: {e}")))
    }

    /// Logs the vCPU state and returns the crash reason.
    fn crash(&self, what: &str) -> VmExit {
        let pc = self.vcpu.get_reg(reg::HV_REG_PC).unwrap_or(0);
        let cpsr = self.vcpu.get_reg(reg::HV_REG_CPSR).unwrap_or(0);
        let elr = self
            .vcpu
            .get_sys_reg(sys_reg::HV_SYS_REG_ELR_EL1)
            .unwrap_or(0);
        let msg = format!(
            "vCPU {}: {what} at PC {pc:#x} (CPSR {cpsr:#x}, ELR_EL1 {elr:#x})",
            self.index
        );
        error!("{msg}");
        VmExit::Crashed(msg)
    }
}
