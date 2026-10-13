// SPDX-License-Identifier: Apache-2.0
//! Building, booting and tearing down one VM.
//!
//! ```text
//! VmBuilder::new   hv_vm_create (IPA sized to RAM) → mmap RAM → hv_vm_map → hv_gic_create
//! add_* devices    onto the MMIO bus, with SPI lines
//! boot             load kernel + initrd, write FDT, spawn one thread per vCPU,
//!                  wait until every vCPU exists, power on the boot vCPU
//! Vm::wait         block until SYSTEM_OFF / SYSTEM_RESET / stop / crash
//! drop             join vCPUs → drop workers → GIC → hv_vm_destroy → munmap
//! ```
//!
//! The teardown order is a contract (AGENTS.md); `Drop` enforces it even
//! when boot fails halfway.

use std::sync::{Arc, OnceLock, PoisonError, RwLock, mpsc};
use std::thread::{self, JoinHandle};

use tracing::{debug, info};
use vmuc_hv::{Gic, GicConfig, HvError, HvVm, ReservedIntid};
use vmuc_virtio::devices::balloon::{Balloon, BalloonHandle, Reclaim};
use vmuc_virtio::{BusDevice, GuestMemory, Interrupt, IrqStatus, MmioTransport, VirtioDevice};

use crate::bus::{MmioBus, Overlap};
use crate::devices::console::ConsoleOut;
use crate::devices::pl011::Pl011;
use crate::devices::pl031::Pl031;
use crate::fdt::{self, FdtError, FdtParams, TimerIntids};
use crate::layout::{
    DAX_WINDOW_SIZE, GICD_BASE, GICR_BASE, GICR_WINDOW, PL011_BASE, PL011_SPI, PL031_BASE,
    PRIMECELL_SIZE, RAM_BASE, VIRTIO_SLOT_SIZE, VIRTIO_SLOTS, dax_window_base, spi_intid,
    virtio_slot_base, virtio_slot_spi,
};
use crate::loader::{BootLayout, ImageHeader, LoadError};
use crate::machine::{Machine, VmExit};
use crate::memory::reclaim::HvStage2;
use crate::memory::{GuestRam, ReclaimError, ReclaimStats, Reclaimer};
use crate::vcpu::run_vcpu;
use crate::window::FileWindow;

#[derive(Debug, thiserror::Error)]
pub enum VmError {
    #[error("hypervisor: {0}")]
    Hv(#[from] HvError),
    #[error("guest RAM: {0}")]
    Ram(#[from] std::io::Error),
    #[error(transparent)]
    Load(#[from] LoadError),
    #[error(transparent)]
    Fdt(#[from] FdtError),
    #[error(transparent)]
    Bus(#[from] Overlap),
    #[error("{0}")]
    Config(String),
}

/// What to boot.
#[derive(Debug, Clone)]
pub struct VmConfig {
    pub cpus: u32,
    pub ram_size: u64,
    pub cmdline: String,
    /// Guest physical space to reserve for virtiofs DAX windows.
    pub dax_windows: u32,
}

/// A level-triggered SPI on the in-kernel GIC.
#[derive(Debug)]
pub struct SpiLine(pub u32);

impl Interrupt for SpiLine {
    fn set_level(&self, asserted: bool) {
        if let Err(e) = Gic::set_spi(self.0, asserted) {
            tracing::warn!(intid = self.0, "hv_gic_set_spi: {e}");
        }
    }
}

/// The balloon's way back to the VMM: releases through the reclaimer,
/// and a hole in guest RAM stops the VM.
struct VmReclaim {
    reclaimer: Reclaimer,
    machine: Arc<OnceLock<Arc<Machine>>>,
}

impl Reclaim for VmReclaim {
    fn release(&self, gpa: u64, len: u64) -> Result<u64, String> {
        match self.reclaimer.release(gpa, len) {
            Ok(n) => Ok(n),
            // Nothing was unmapped: acknowledging is harmless.
            Err(e @ ReclaimError::OutOfRange { .. }) => {
                tracing::warn!("balloon: {e}");
                Ok(0)
            }
            Err(e) => {
                if let (ReclaimError::Hole { .. }, Some(m)) = (&e, self.machine.get()) {
                    m.request_exit(VmExit::Crashed(e.to_string()));
                }
                Err(e.to_string())
            }
        }
    }
}

pub struct VmBuilder {
    config: VmConfig,
    bus: MmioBus,
    transports: Vec<Arc<MmioTransport>>,
    /// Set once the machine exists, for device paths that must stop it.
    machine: Arc<OnceLock<Arc<Machine>>>,
    /// Open while releases may run; closed before `hv_vm_destroy`.
    reclaim_gate: Arc<RwLock<bool>>,
    gicr_frame: u64,
    gic: Gic,
    ram: GuestRam,
    hv: HvVm,
    /// DAX windows handed out so far.
    dax_windows: u32,
}

impl VmBuilder {
    pub fn new(config: VmConfig) -> Result<Self, VmError> {
        let max = HvVm::max_vcpu_count()?.min(64);
        if config.cpus == 0 || config.cpus > max {
            return Err(VmError::Config(format!(
                "{} vCPUs requested; this host allows 1 to {max}",
                config.cpus
            )));
        }
        let hv = create_vm(dax_window_base(config.ram_size, config.dax_windows))?;
        let ram = GuestRam::new(RAM_BASE, config.ram_size)?;
        ram.map_into(&hv)?;

        let (gicr_region, redist_align) = Gic::redistributor_geometry()?;
        let (_, dist_align) = Gic::distributor_geometry()?;
        if gicr_region as u64 > GICR_WINDOW
            || !GICR_BASE.is_multiple_of(redist_align as u64)
            || !GICD_BASE.is_multiple_of(dist_align as u64)
        {
            return Err(VmError::Config(format!(
                "this host's GIC frames (redistributors {gicr_region:#x}, alignment \
                 {redist_align:#x}) do not fit vmuc's guest physical map"
            )));
        }
        let gic = Gic::new(GicConfig {
            distributor_base: GICD_BASE,
            redistributor_base: GICR_BASE,
        })?;
        let gicr_frame = gic.redistributor_size()? as u64;
        Ok(Self {
            config,
            bus: MmioBus::default(),
            transports: Vec::new(),
            machine: Arc::new(OnceLock::new()),
            reclaim_gate: Arc::new(RwLock::new(true)),
            gicr_frame,
            gic,
            ram,
            hv,
            dax_windows: 0,
        })
    }

    /// The next of the DAX windows reserved in [`VmConfig`], for a
    /// virtio-fs device to map files into.
    pub fn add_file_window(&mut self) -> Result<Arc<FileWindow>, VmError> {
        if self.dax_windows == self.config.dax_windows {
            return Err(VmError::Config(format!(
                "only {} DAX windows were reserved",
                self.config.dax_windows
            )));
        }
        let base = dax_window_base(self.config.ram_size, self.dax_windows);
        self.dax_windows += 1;
        Ok(Arc::new(FileWindow::new(
            base,
            DAX_WINDOW_SIZE,
            Box::new(crate::window::HvMap),
        )))
    }

    /// Guest RAM for device models and loaders.
    pub fn memory(&self) -> GuestMemory {
        self.ram.memory()
    }

    /// Adds the PL011 (console output to `out`) and the PL031. Returns the
    /// UART so the caller can feed it keyboard input.
    pub fn add_platform_devices(&mut self, out: ConsoleOut) -> Result<Arc<Pl011>, VmError> {
        let uart = Arc::new(Pl011::new(Arc::new(SpiLine(spi_intid(PL011_SPI))), out));
        self.bus.insert(PL011_BASE, PRIMECELL_SIZE, uart.clone())?;
        self.bus
            .insert(PL031_BASE, PRIMECELL_SIZE, Arc::new(Pl031::default()))?;
        Ok(uart)
    }

    /// Adds a device at an arbitrary window (virtio-mmio slots use this).
    pub fn add_mmio(
        &mut self,
        base: u64,
        size: u64,
        dev: Arc<dyn BusDevice>,
    ) -> Result<(), VmError> {
        self.bus.insert(base, size, dev)?;
        Ok(())
    }

    /// Puts a virtio device in the next virtio-mmio slot, wired to that
    /// slot's SPI. Slot order is FDT order, so the first block device
    /// added is `/dev/vda`.
    pub fn add_virtio(
        &mut self,
        device: Box<dyn VirtioDevice>,
    ) -> Result<Arc<MmioTransport>, VmError> {
        let slot = self.transports.len() as u32;
        if slot >= VIRTIO_SLOTS {
            return Err(VmError::Config(format!(
                "more than {VIRTIO_SLOTS} virtio devices"
            )));
        }
        let line = Arc::new(SpiLine(spi_intid(virtio_slot_spi(slot))));
        let transport = Arc::new(MmioTransport::new(
            device,
            self.ram.memory(),
            Arc::new(IrqStatus::new(line)),
        ));
        self.bus
            .insert(virtio_slot_base(slot), VIRTIO_SLOT_SIZE, transport.clone())?;
        self.transports.push(Arc::clone(&transport));
        Ok(transport)
    }

    /// Adds the balloon with free page reporting, wired to a reclaimer
    /// over this VM's RAM. Returns its control handle and the counters.
    pub fn add_balloon(&mut self) -> Result<(BalloonHandle, Arc<ReclaimStats>), VmError> {
        let stats = Arc::new(ReclaimStats::default());
        // SAFETY: the host pointer and size are guest RAM's mapping, which
        // the Vm keeps until after it closes `reclaim_gate`.
        let reclaimer = unsafe {
            Reclaimer::new(
                self.ram.host_ptr(),
                self.ram.base(),
                self.ram.size(),
                Arc::clone(&self.reclaim_gate),
                Arc::clone(&stats),
                Box::new(HvStage2),
            )
        };
        let reclaim = Arc::new(VmReclaim {
            reclaimer,
            machine: Arc::clone(&self.machine),
        });
        let balloon = Balloon::new(reclaim)?;
        let handle = balloon.handle();
        self.add_virtio(Box::new(balloon))?;
        Ok((handle, stats))
    }

    /// Loads the kernel, writes the device tree and starts the vCPUs.
    pub fn boot(self, kernel: &[u8], initrd: Option<&[u8]>) -> Result<Vm, VmError> {
        let header = ImageHeader::parse(kernel)?;
        let layout = BootLayout::plan(
            self.config.ram_size,
            header,
            kernel.len() as u64,
            initrd.map(|i| i.len() as u64),
        )?;
        let mem = self.ram.memory();
        layout.load(&mem, kernel, initrd)?;

        let mut seed = [0u8; 64];
        getentropy(&mut seed)?;
        let mut kaslr = [0u8; 8];
        getentropy(&mut kaslr)?;
        let timers = TimerIntids {
            virt: Gic::reserved_intid(ReservedIntid::El1VirtualTimer)?,
            phys: Gic::reserved_intid(ReservedIntid::El1PhysicalTimer)?,
            ..TimerIntids::default()
        };
        let blob = fdt::build(&FdtParams {
            cpus: self.config.cpus,
            ram_size: self.config.ram_size,
            cmdline: &self.config.cmdline,
            initrd: layout.initrd.clone(),
            gicr_size: u64::from(self.config.cpus) * self.gicr_frame,
            virtio_slots: self.transports.len() as u32,
            rng_seed: &seed,
            kaslr_seed: u64::from_le_bytes(kaslr),
            timers,
        })?;
        mem.write(layout.fdt, &blob).map_err(LoadError::from)?;
        debug!(?layout, fdt_bytes = blob.len(), "guest loaded");

        let ram = RAM_BASE..RAM_BASE + self.config.ram_size;
        let machine = Arc::new(Machine::new(self.config.cpus, self.bus).with_ram(ram));
        let _ = self.machine.set(Arc::clone(&machine));
        let mut vm = Vm {
            machine,
            vcpu_threads: Vec::new(),
            transports: self.transports,
            reclaim_gate: self.reclaim_gate,
            _gic: self.gic,
            _hv: self.hv,
            _ram: self.ram,
        };
        let (tx, rx) = mpsc::channel();
        for index in 0..self.config.cpus as usize {
            let m = Arc::clone(&vm.machine);
            let tx = tx.clone();
            let handle = thread::Builder::new()
                .name(format!("vcpu{index}"))
                .spawn(move || run_vcpu(index, &m, |r| tx.send(r).unwrap_or(())))
                .map_err(VmError::Ram)?;
            vm.vcpu_threads.push(handle);
        }
        drop(tx);
        for _ in 0..self.config.cpus {
            match rx.recv() {
                Ok(Ok(_)) => {}
                Ok(Err(e)) => return Err(e.into()),
                Err(_) => return Err(VmError::Config("a vCPU thread died during creation".into())),
            }
        }
        info!(cpus = self.config.cpus, "booting");
        vm.machine.power_on(0, layout.entry, layout.fdt);
        Ok(vm)
    }
}

/// A running VM. Dropping it stops the guest and tears everything down
/// in contract order.
pub struct Vm {
    machine: Arc<Machine>,
    vcpu_threads: Vec<JoinHandle<()>>,
    /// Shut down after the vCPUs, so device workers stop touching guest
    /// memory before the VM and RAM go away.
    transports: Vec<Arc<MmioTransport>>,
    reclaim_gate: Arc<RwLock<bool>>,
    // Dropped in declaration order after `Drop::drop` has joined the
    // vCPUs: GIC, then the VM (hv_vm_destroy), then RAM (munmap).
    _gic: Gic,
    _hv: HvVm,
    _ram: GuestRam,
}

impl std::fmt::Debug for Vm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Vm")
            .field("machine", &self.machine)
            .finish_non_exhaustive()
    }
}

impl Vm {
    /// Shared state: ask it to stop, read exit counters.
    pub fn machine(&self) -> &Arc<Machine> {
        &self.machine
    }

    /// Blocks until the guest powers off, resets, crashes or is stopped,
    /// then tears the VM down.
    pub fn wait(self) -> VmExit {
        let why = self.machine.wait_exit();
        drop(self);
        why
    }
}

impl Drop for Vm {
    fn drop(&mut self) {
        self.machine.request_exit(VmExit::Stopped);
        for t in self.vcpu_threads.drain(..) {
            let _ = t.join();
        }
        for t in self.transports.drain(..) {
            t.shutdown();
        }
        // No release may be half done when the VM goes away.
        *self
            .reclaim_gate
            .write()
            .unwrap_or_else(PoisonError::into_inner) = false;
        let c = &self.machine.counts;
        let n = |a: &std::sync::atomic::AtomicU64| a.load(std::sync::atomic::Ordering::Relaxed);
        debug!(
            mmio = n(&c.mmio),
            sysreg = n(&c.sysreg),
            hvc = n(&c.hvc),
            canceled = n(&c.canceled),
            other = n(&c.other),
            "vCPUs joined; destroying VM"
        );
    }
}

/// Creates the VM with the smallest IPA width (36 bits, the default, or
/// more) that covers `top`, the end of guest RAM.
fn create_vm(top: u64) -> Result<HvVm, VmError> {
    let (default, max) = HvVm::ipa_size_limits()?;
    let needed = 64 - (top - 1).leading_zeros();
    if needed <= default {
        return Ok(HvVm::new()?);
    }
    if needed > max {
        return Err(VmError::Config(format!(
            "guest RAM ends at {top:#x}, beyond this host's {max}-bit guest physical space"
        )));
    }
    Ok(HvVm::with_ipa_size(needed)?)
}

/// Boot-time randomness for the FDT seeds.
fn getentropy(buf: &mut [u8]) -> std::io::Result<()> {
    use std::io::Read;
    std::fs::File::open("/dev/urandom")?.read_exact(buf)
}
