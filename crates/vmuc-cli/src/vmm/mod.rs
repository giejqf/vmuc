// SPDX-License-Identifier: Apache-2.0
//! `vmuc _vmm NAME`: the process that owns one instance's VM.
//!
//! Spawned detached by `vmuc start`. It renders a fresh seed (new boot
//! ID), builds the machine (disk, seed, vsock, hvc0, rng), records its
//! pid, serves `vmm.sock`, probes readiness into `events.jsonl`, and on
//! SIGTERM/SIGINT asks the guest to power off (a second signal forces
//! it). A guest reset re-executes the process for a clean VM; the pid
//! stays the same.

mod ready;
mod server;
mod signals;

use std::fs::{File, OpenOptions};
use std::os::unix::process::CommandExt;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use vmuc_config::VmucHome;
use vmuc_config::paths::instance as files;
use vmuc_proto::{AgentRequest, EventKind, InstanceStatus, Phase};
use vmuc_virtio::devices::blk::{Block, BlockConfig};
use vmuc_virtio::devices::console::Console;
use vmuc_virtio::devices::rng::Rng;
use vmuc_virtio::devices::vsock::{GUEST_CID, Vsock, VsockConnector};
use vmuc_vmm::machine::Machine;
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
            addrs: Vec::new(),
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

fn boot_and_wait(
    home: &VmucHome,
    inst: &Instance,
    signals: signals::Blocked,
) -> anyhow::Result<VmExit> {
    let resolved = inst.resolved(home)?;
    let events = EventLog::create(&inst.dir)?;
    let kernel_path = assets::kernel()?;
    let kernel = std::fs::read(&kernel_path).with_context(|| kernel_path.display().to_string())?;
    let agent = assets::agent()?;
    let boot_id = instance::write_seed(home, inst, &resolved, Some(&agent), false)?;
    events.push(EventKind::Booting {
        boot_id: boot_id.clone(),
    });
    let info = instance::boot_info(inst, &resolved)?;

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
    let vm = b.boot(&kernel, None)?;

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
    });
    server::spawn(&inst.dir, Arc::clone(&shared))?;
    ready::spawn(Arc::clone(&shared));
    signals::spawn(signals, Arc::clone(&shared))?;

    let exit = vm.wait();
    shared.events.push(EventKind::Exiting {
        reason: format!("{exit:?}"),
    });
    out.close();
    let _ = writer.join();
    Ok(exit)
}
