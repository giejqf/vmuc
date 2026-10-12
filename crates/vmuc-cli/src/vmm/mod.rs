// SPDX-License-Identifier: Apache-2.0
//! `vmuc _vmm NAME`: the process that owns one instance's VM.
//!
//! Spawned detached by `vmuc start`. It renders a fresh seed (new boot
//! ID), builds the machine (disk, seed, vsock, hvc0, rng, NIC, virtiofs
//! shares), records its
//! pid, serves `vmm.sock`, probes readiness into `events.jsonl`, and on
//! SIGTERM/SIGINT asks the guest to power off (a second signal forces
//! it). A guest reset re-executes the process for a clean VM; the pid
//! stays the same.

mod forward;
mod memory;
mod network;
mod ready;
mod server;
mod shares;
mod signals;

use std::fs::{File, OpenOptions};
use std::os::unix::process::CommandExt;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use vmuc_config::VmucHome;
use vmuc_config::paths::instance as files;
use vmuc_proto::{AgentRequest, EventKind, Forward, InstanceStatus, Phase};
use vmuc_virtio::devices::balloon::BalloonHandle;
use vmuc_virtio::devices::blk::{Block, BlockConfig};
use vmuc_virtio::devices::console::Console;
use vmuc_virtio::devices::rng::Rng;
use vmuc_virtio::devices::vsock::{GUEST_CID, Vsock, VsockConnector};
use vmuc_vmm::loader::MappedFile;
use vmuc_vmm::machine::Machine;
use vmuc_vmm::memory::ReclaimStats;
use vmuc_vmm::{ConsoleOut, VmBuilder, VmConfig, VmExit};

use crate::assets;
use crate::instance::{self, Instance};
use crate::state::{self, EventLog};

/// How long a graceful stop waits for the guest before forcing it.
const STOP_GRACE: Duration = Duration::from_secs(60);

/// What the control server, the readiness prober and the signal thread share.
pub struct Shared {
    name: String,
    boot_id: String,
    cpus: u32,
    memory: u64,
    started: Instant,
    machine: Arc<Machine>,
    vsock: VsockConnector,
    phase: Mutex<Phase>,
    errors: Mutex<Vec<String>>,
    events: EventLog,
    balloon: BalloonHandle,
    reclaim: Arc<ReclaimStats>,
    last_reclaim: Mutex<Option<Instant>>,
    pressure_balloon: bool,
    min_available: u64,
    forwards: Mutex<Vec<Forward>>,
    addrs: Mutex<Vec<std::net::IpAddr>>,
    /// The NIC's traffic counters, when there is a NIC.
    net: Option<Arc<vmuc_virtio::devices::net::NetCounters>>,
}

impl Shared {
    fn set_phase(&self, p: Phase) {
        *self.phase.lock().unwrap_or_else(PoisonError::into_inner) = p;
    }

    fn status(&self) -> InstanceStatus {
        InstanceStatus {
            name: self.name.clone(),
            phase: *self.phase.lock().unwrap_or_else(PoisonError::into_inner),
            boot_id: self.boot_id.clone(),
            pid: std::process::id(),
            cpus: self.cpus,
            memory: self.memory,
            addrs: self
                .addrs
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone(),
            uptime_secs: self.started.elapsed().as_secs(),
            errors: self
                .errors
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone(),
        }
    }

    /// Asks the guest to power off and forces the VM down if it has not
    /// within [`STOP_GRACE`]; `force` skips the asking.
    fn stop(self: &Arc<Self>, force: bool) {
        if force {
            return self.machine.request_exit(VmExit::Stopped);
        }
        self.set_phase(Phase::Stopping);
        let me = Arc::clone(self);
        std::thread::spawn(move || {
            let poweroff = AgentRequest::Exec {
                argv: vec!["systemctl".into(), "poweroff".into()],
                stdin: None,
                timeout: Some(10),
            };
            if ready::agent_call(&me.vsock, &poweroff, Duration::from_secs(5)).is_err() {
                tracing::warn!("guest agent unreachable; forcing the VM off");
                return me.machine.request_exit(VmExit::Stopped);
            }
            let deadline = Instant::now() + STOP_GRACE;
            while !me.machine.stopping() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(100));
            }
            me.machine.request_exit(VmExit::Stopped);
        });
    }
}

pub fn run(home: &VmucHome, name: &str) -> anyhow::Result<()> {
    let inst = Instance::open(home, name)?;
    if let Some(pid) = state::running_pid(&inst.dir)
        && pid != std::process::id()
    {
        bail!("{name} is already running (pid {pid})");
    }
    // Before any thread exists, so every thread inherits the mask.
    let signals = signals::block()?;
    state::write_pid(&inst.dir)?;
    let result = boot_and_wait(home, &inst, signals);
    state::remove_pid(&inst.dir);
    if matches!(result, Ok(VmExit::Reset)) {
        let exe = std::env::current_exe()?;
        let err = std::process::Command::new(exe).args(["_vmm", name]).exec();
        bail!("re-executing after guest reset: {err}");
    }
    match result? {
        VmExit::Crashed(why) => bail!("the VM crashed: {why}"),
        _ => Ok(()),
    }
}

/// macOS's allocator keeps freed large blocks (the kernel image, the
/// seed ISO, the agent binary) dirty and billed; hand them back once the
/// guest is loaded, since this process lives as long as the VM.
#[cfg(target_os = "macos")]
fn release_freed_heap() {
    unsafe extern "C" {
        fn malloc_zone_pressure_relief(zone: *mut std::ffi::c_void, goal: usize) -> usize;
    }
    // SAFETY: a NULL zone means every zone; goal 0 means as much as possible.
    let freed = unsafe { malloc_zone_pressure_relief(std::ptr::null_mut(), 0) };
    tracing::debug!(freed, "returned freed heap to the system");
}

#[cfg(not(target_os = "macos"))]
fn release_freed_heap() {}

fn boot_and_wait(
    home: &VmucHome,
    inst: &Instance,
    signals: signals::Blocked,
) -> anyhow::Result<VmExit> {
    let resolved = inst.resolved(home)?;
    let events = EventLog::create(&inst.dir)?;
    let kernel_path = assets::kernel()?;
    let agent = assets::agent()?;
    let fs_shares = shares::open(&resolved, &events);
    let mounts: Vec<_> = fs_shares.iter().map(|s| s.mount.clone()).collect();
    let boot_id = instance::write_seed(home, inst, &resolved, Some(&agent), &mounts)?;
    events.push(EventKind::Booting {
        boot_id: boot_id.clone(),
    });
    let mut info = instance::boot_info(inst, &resolved)?;
    // Experiments and benchmarks append kernel parameters without editing
    // the instance (e.g. page_reporting.page_reporting_order=N).
    if let Some(extra) = std::env::var_os(assets::CMDLINE_ENV).filter(|v| !v.is_empty()) {
        info.cmdline.push(' ');
        info.cmdline.push_str(&extra.to_string_lossy());
    }

    let log = File::create(inst.dir.join(files::CONSOLE_LOG))?;
    let (out, writer) = ConsoleOut::spawn(vec![Box::new(log)]);
    let mut b = VmBuilder::new(VmConfig {
        cpus: info.cpus,
        ram_size: info.memory_bytes,
        cmdline: info.cmdline.clone(),
    })?;
    b.add_platform_devices(out.clone())?;
    let disk = OpenOptions::new()
        .read(true)
        .write(true)
        .open(inst.disk())?;
    let serial = format!("vmuc-{}", inst.name);
    b.add_virtio(Box::new(Block::new(
        disk,
        &BlockConfig {
            read_only: false,
            serial,
        },
    )?))?;
    let seed = File::open(inst.seed())?;
    b.add_virtio(Box::new(Block::new(
        seed,
        &BlockConfig {
            read_only: true,
            serial: "cidata".into(),
        },
    )?))?;
    let vsock = Vsock::new(GUEST_CID)?;
    let connector = vsock.connector();
    b.add_virtio(Box::new(vsock))?;
    b.add_virtio(Box::new(Console::new(Arc::new(out.clone()))?))?;
    b.add_virtio(Box::new(Rng::new()?))?;
    let (balloon, reclaim) = b.add_balloon()?;
    // Last, so disks keep their names: the NIC, when the helper starts.
    let net = network::attach(&mut b, &resolved, &events);
    // After the NIC, so its slot does not depend on the mounts.
    shares::attach(&mut b, fs_shares)?;
    // Mapped, not read: a heap copy would stay billed for the VM's life.
    let kernel =
        MappedFile::open(&kernel_path).with_context(|| kernel_path.display().to_string())?;
    let vm = b.boot(&kernel, None)?;
    drop(kernel);
    release_freed_heap();

    let shared = Arc::new(Shared {
        name: inst.name.clone(),
        boot_id,
        cpus: info.cpus,
        memory: info.memory_bytes,
        started: Instant::now(),
        machine: Arc::clone(vm.machine()),
        vsock: connector,
        phase: Mutex::new(Phase::Booting),
        errors: Mutex::new(Vec::new()),
        events,
        balloon,
        reclaim,
        last_reclaim: Mutex::new(None),
        pressure_balloon: resolved.memory_policy.pressure_balloon,
        min_available: resolved.memory_policy.min_available.bytes(),
        forwards: Mutex::new(Vec::new()),
        addrs: Mutex::new(Vec::new()),
        net,
    });
    server::spawn(&inst.dir, Arc::clone(&shared))?;
    ready::spawn(Arc::clone(&shared));
    memory::spawn_responder(Arc::clone(&shared));
    forward::spawn(Arc::clone(&shared), resolved.port_forwards.clone())?;
    signals::spawn(signals, Arc::clone(&shared))?;

    let exit = vm.wait();
    shared.events.push(EventKind::Exiting {
        reason: format!("{exit:?}"),
    });
    out.close();
    let _ = writer.join();
    Ok(exit)
}
