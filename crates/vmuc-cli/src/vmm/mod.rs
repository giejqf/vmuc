// SPDX-License-Identifier: Apache-2.0
//! `vmuc _vmm NAME`: the process that owns one instance's VM.
//!
//! Spawned detached by `vmuc start`. It renders a fresh seed (new boot
//! ID), builds the machine (disk, seed, vsock, hvc0, rng, NIC, virtiofs
//! shares), records its
//! pid, serves `vmm.sock`, probes readiness into `events.jsonl`, and on
//! SIGTERM/SIGINT asks the guest to power off (a second signal forces
//! it). A guest reset re-executes the process for a clean VM; the pid
//! stays the same. A suspended instance is resumed instead of booted,
//! into the same machine, and `vmuc suspend` ends the process the other
//! way (ADR 0007).

mod forward;
mod memory;
mod network;
mod ready;
mod server;
mod shares;
mod signals;
mod suspend;
mod watch;

use std::fs::{File, OpenOptions};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError, mpsc};
use std::time::{Duration, Instant, UNIX_EPOCH};

use anyhow::{Context, anyhow, bail};
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
use vmuc_vmm::snapshot::RamRestore;
use vmuc_vmm::{ConsoleOut, Vm, VmBuilder, VmConfig, VmExit};

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
    /// A pressure level to act on instead of the host's, until the instant.
    simulated: Mutex<Option<(vmuc_proto::PressureLevel, Instant)>>,
    forwards: Mutex<Vec<Forward>>,
    addrs: Mutex<Vec<std::net::IpAddr>>,
    /// The NIC's traffic counters, when there is a NIC.
    net: Option<Arc<vmuc_virtio::devices::net::NetCounters>>,
    /// The instance directory and its disk, for a suspend.
    dir: PathBuf,
    disk: PathBuf,
    /// The machine a resume must rebuild.
    shape: crate::suspend::Shape,
    /// To the main thread, which owns the VM.
    main: mpsc::Sender<suspend::Main>,
}

impl Shared {
    fn set_phase(&self, p: Phase) {
        *self.phase.lock().unwrap_or_else(PoisonError::into_inner) = p;
    }

    fn phase(&self) -> Phase {
        *self.phase.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn status(&self) -> InstanceStatus {
        InstanceStatus {
            name: self.name.clone(),
            phase: self.phase(),
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
    if matches!(result, Ok(Ended::Exit(VmExit::Reset))) {
        let exe = std::env::current_exe()?;
        let err = std::process::Command::new(exe).args(["_vmm", name]).exec();
        bail!("re-executing after guest reset: {err}");
    }
    match result? {
        Ended::Exit(VmExit::Crashed(why)) => bail!("the VM crashed: {why}"),
        _ => Ok(()),
    }
}

/// How the VM ended.
enum Ended {
    Exit(VmExit),
    Suspended,
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
) -> anyhow::Result<Ended> {
    let spawned = Instant::now();
    let resolved = inst.resolved(home)?;
    let events = EventLog::create(&inst.dir)?;
    let resume = crate::suspend::load(&inst.dir).context("reading the suspended state")?;
    let fs_shares = shares::open(&resolved, &events);
    let mut shape = suspend::shape(&resolved, &fs_shares);
    if let Some(s) = &resume {
        suspend::check(&inst.name, &s.machine, &shape)?;
    }
    let watched = fs_shares
        .iter()
        .filter(|s| s.mount.writable)
        .map(shares::Share::watched)
        .collect();
    let boot_id = if let Some(s) = &resume {
        s.boot_id.clone()
    } else {
        let agent = assets::agent()?;
        let mounts: Vec<_> = fs_shares.iter().map(|s| s.mount.clone()).collect();
        let boot_id = instance::write_seed(home, inst, &resolved, Some(&agent), &mounts)?;
        events.push(EventKind::Booting {
            boot_id: boot_id.clone(),
        });
        boot_id
    };
    let mut info = instance::boot_info(inst, &resolved)?;
    // Experiments and benchmarks append kernel parameters without editing
    // the instance (e.g. page_reporting.page_reporting_order=N).
    if let Some(extra) = std::env::var_os(assets::CMDLINE_ENV).filter(|v| !v.is_empty()) {
        info.cmdline.push(' ');
        info.cmdline.push_str(&extra.to_string_lossy());
    }
    // A duplicate's guest takes its own machine ID (ADR 0009).
    if resume.is_none()
        && let Some(id) = crate::duplicate::machine_id_param(&inst.dir)
    {
        info.cmdline.push(' ');
        info.cmdline.push_str(&id);
    }

    // A resumed guest's console carries on in the same log.
    let log = File::options()
        .create(true)
        .write(true)
        .append(resume.is_some())
        .truncate(resume.is_none())
        .open(inst.dir.join(files::CONSOLE_LOG))?;
    let (out, writer) = ConsoleOut::spawn(vec![Box::new(log)]);
    let mut b = VmBuilder::new(VmConfig {
        cpus: info.cpus,
        ram_size: info.memory_bytes,
        cmdline: info.cmdline.clone(),
        dax_windows: fs_shares.iter().filter(|s| s.mount.dax).count() as u32,
    })?;
    let devices = add_devices(&mut b, inst, &resolved, &events, &out, resume.as_ref())?;
    if devices.net.is_none() {
        shape.network = None;
    }
    // After the NIC, so its slot does not depend on the mounts.
    shares::attach(&mut b, fs_shares)?;
    let vm = start_vm(b, inst, resume.is_some())?;
    if resume.is_some() {
        events.push(EventKind::Resumed {
            boot_id: boot_id.clone(),
            restore_ms: spawned.elapsed().as_millis() as u64,
        });
    }
    release_freed_heap();

    let (main_tx, main_rx) = mpsc::channel();
    let shared = Arc::new(Shared {
        name: inst.name.clone(),
        boot_id,
        cpus: info.cpus,
        memory: info.memory_bytes,
        started: Instant::now(),
        machine: Arc::clone(vm.machine()),
        vsock: devices.vsock,
        phase: Mutex::new(Phase::Booting),
        errors: Mutex::new(Vec::new()),
        events,
        balloon: devices.balloon,
        reclaim: devices.reclaim,
        last_reclaim: Mutex::new(None),
        simulated: Mutex::new(None),
        forwards: Mutex::new(Vec::new()),
        addrs: Mutex::new(Vec::new()),
        net: devices.net,
        dir: inst.dir.clone(),
        disk: inst.disk(),
        shape,
        main: main_tx.clone(),
    });
    server::spawn(&inst.dir, Arc::clone(&shared))?;
    ready::spawn(Arc::clone(&shared));
    memory::spawn_responder(Arc::clone(&shared));
    forward::spawn(Arc::clone(&shared), resolved.port_forwards.clone())?;
    // Host changes made while suspended, with a second to spare.
    let away_since = resume
        .as_ref()
        .map(|s| UNIX_EPOCH + Duration::from_millis(s.stopped_at_ms.saturating_sub(1000)));
    watch::spawn(Arc::clone(&shared), watched, away_since)?;
    signals::spawn(signals, Arc::clone(&shared))?;

    let ended = serve(vm, &shared, main_tx, &main_rx)?;
    out.close();
    let _ = writer.join();
    Ok(ended)
}

/// What the control server and friends hold of the devices.
struct Devices {
    vsock: VsockConnector,
    balloon: BalloonHandle,
    reclaim: Arc<ReclaimStats>,
    net: Option<Arc<vmuc_virtio::devices::net::NetCounters>>,
}

/// Every device but the shares. A resumed machine has a NIC exactly
/// when the suspended one had.
fn add_devices(
    b: &mut VmBuilder,
    inst: &Instance,
    resolved: &vmuc_config::ResolvedConfig,
    events: &EventLog,
    out: &ConsoleOut,
    resume: Option<&crate::suspend::Suspended>,
) -> anyhow::Result<Devices> {
    let name = &inst.name;
    b.add_platform_devices(out.clone())?;
    let disk = OpenOptions::new()
        .read(true)
        .write(true)
        .open(inst.disk())?;
    let serial = format!("vmuc-{name}");
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
    let net = match resume.map(|s| s.machine.network.is_some()) {
        None => network::attach(b, resolved, events),
        Some(true) => Some(network::attach(b, resolved, events).ok_or_else(|| {
            anyhow!(
                "cannot resume {name}: networking is unavailable (see the warning above) \
                 and {name} was suspended with it"
            )
        })?),
        Some(false) => {
            events.push(EventKind::Warning {
                message: "no networking until the next boot: it was off when suspended".into(),
            });
            None
        }
    };
    Ok(Devices {
        vsock: connector,
        balloon,
        reclaim,
        net,
    })
}

/// Boots the guest, or resumes the suspended one with its RAM mapped from
/// the image; the suspended state goes before the guest runs, since the
/// disk moves on from then.
fn start_vm(b: VmBuilder, inst: &Instance, resume: bool) -> anyhow::Result<Vm> {
    let name = &inst.name;
    if resume {
        let consume = || crate::suspend::remove(&inst.dir);
        return b
            .restore_with(&crate::suspend::dir(&inst.dir), RamRestore::Map, consume)
            .map_err(|e| {
                anyhow!(
                    "cannot resume {name}: {e}; `vmuc stop --force {name}` discards the \
                     suspended state and the next start boots"
                )
            });
    }
    let kernel_path = assets::kernel()?;
    // Mapped, not read: a heap copy would stay billed for the VM's life.
    let kernel =
        MappedFile::open(&kernel_path).with_context(|| kernel_path.display().to_string())?;
    Ok(b.boot(&kernel, None)?)
}

/// The main thread from here: owns the VM until it exits or is suspended.
fn serve(
    vm: Vm,
    shared: &Shared,
    main_tx: mpsc::Sender<suspend::Main>,
    main_rx: &mpsc::Receiver<suspend::Main>,
) -> anyhow::Result<Ended> {
    let machine = Arc::clone(vm.machine());
    std::thread::Builder::new()
        .name("vm-exit".into())
        .spawn(move || {
            let _ = main_tx.send(suspend::Main::Exit(machine.wait_exit()));
        })?;
    let ended = loop {
        match main_rx.recv() {
            Ok(suspend::Main::Suspend(reply)) => match suspend::run(&vm, shared) {
                Ok(stats) => {
                    let _ = reply.send(Ok(stats));
                    break Ended::Suspended;
                }
                Err(e) => {
                    let _ = reply.send(Err(e));
                }
            },
            Ok(suspend::Main::Exit(exit)) => break Ended::Exit(exit),
            Err(_) => break Ended::Exit(VmExit::Stopped),
        }
    };
    let reason = match &ended {
        Ended::Exit(exit) => format!("{exit:?}"),
        Ended::Suspended => "suspended".to_owned(),
    };
    shared.events.push(EventKind::Exiting { reason });
    drop(vm);
    Ok(ended)
}
