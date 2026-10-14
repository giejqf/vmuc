// SPDX-License-Identifier: Apache-2.0
//! Suspending the instance (ADR 0007). The main thread owns the VM, so a
//! control request reaches it as a [`Main`] message; it syncs the guest's
//! files, snapshots the VM into the instance's `suspend/` directory and
//! commits it, after which the VMM ends. Any failure lets the guest run
//! on where it was.

use std::fs::File;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use vmuc_config::ResolvedConfig;
use vmuc_config::schema::NetworkMode;
use vmuc_proto::{AgentRequest, AgentResponse, Phase};
use vmuc_vmm::{Vm, VmExit};

use super::shares::Share;
use super::{Shared, ready};
use crate::suspend::{Nic, Shape, ShareShape, Suspended};

/// How long the guest may take to flush its files.
const SYNC_TIMEOUT: Duration = Duration::from_secs(30);

/// What the VMM's main thread waits for.
pub enum Main {
    Exit(VmExit),
    Suspend(mpsc::Sender<Result<Stats, String>>),
}

#[derive(Debug, Clone, Copy)]
pub struct Stats {
    pub ram_bytes: u64,
    /// From the pause until the state was committed.
    pub stopped: Duration,
}

/// The machine as configured, with the NIC the configuration asks for.
pub fn shape(resolved: &ResolvedConfig, shares: &[Share]) -> Shape {
    let mode = match resolved.network.mode {
        NetworkMode::Shared => "shared".to_owned(),
        NetworkMode::Bridged => format!("bridged ({})", resolved.network.interface),
    };
    Shape {
        cpus: resolved.cpus,
        memory: resolved.memory.bytes(),
        network: Some(Nic {
            mac: resolved.network.mac_address.clone(),
            mode,
        }),
        shares: shares
            .iter()
            .map(|s| ShareShape {
                tag: s.mount.tag.clone(),
                location: s.root().to_owned(),
                writable: s.mount.writable,
                dax: s.mount.dax,
            })
            .collect(),
    }
}

/// Refuses to resume into a machine unlike the suspended one.
pub fn check(name: &str, then: &Shape, now: &Shape) -> anyhow::Result<()> {
    let changed = then.differences(now);
    if changed.is_empty() {
        return Ok(());
    }
    crate::exit::fail!(
        crate::exit::Kind::WrongState,
        "cannot resume {name}: its machine changed since it was suspended ({}); revert that \
         with `vmuc edit {name}`, or `vmuc stop --force {name}` discards the suspended state \
         and the next start boots",
        changed.join(", ")
    )
}

/// The control server's side: hands the request to the main thread and
/// waits for the outcome.
pub fn request(shared: &Shared) -> Result<Stats, String> {
    match shared.phase() {
        Phase::Running | Phase::Degraded => {}
        Phase::Booting => return Err("it is still booting; suspend it once it runs".into()),
        Phase::Stopping => return Err("it is stopping".into()),
    }
    if let Some(s) = shared.shape.shares.iter().find(|s| s.dax) {
        return Err(format!(
            "{} is shared with DAX, which suspend does not support yet",
            s.location.display()
        ));
    }
    let (tx, rx) = mpsc::channel();
    shared
        .main
        .send(Main::Suspend(tx))
        .map_err(|_| "the VMM is shutting down".to_owned())?;
    rx.recv()
        .unwrap_or_else(|_| Err("the VMM ended while suspending".into()))
}

/// On the main thread: writes the instance's suspended state. On `Ok` the
/// VM stays paused for the VMM to end; on `Err` the guest runs on.
pub fn run(vm: &Vm, shared: &Shared) -> Result<Stats, String> {
    // First the guest's own files, so that the disk alone is consistent
    // should the suspended state ever be discarded.
    let sync = AgentRequest::Exec {
        argv: vec!["sync".into()],
        stdin: None,
        timeout: Some(SYNC_TIMEOUT.as_secs() as u32),
    };
    match ready::agent_call(&shared.vsock, &sync, SYNC_TIMEOUT + Duration::from_secs(5)) {
        Ok(AgentResponse::Exec { status: 0, .. }) => {}
        other => tracing::warn!(?other, "the guest did not sync before the suspend"),
    }
    let dir = crate::suspend::dir(&shared.dir);
    // Whatever a suspend that failed left there.
    let _ = std::fs::remove_dir_all(&dir);
    let stopped_at_ms = crate::state::now_ms();
    let started = Instant::now();
    let snap = vm.snapshot(&dir).map_err(|e| e.to_string())?;
    let record = Suspended {
        vmuc_version: env!("CARGO_PKG_VERSION").to_owned(),
        boot_id: shared.boot_id.clone(),
        stopped_at_ms,
        machine: shared.shape.clone(),
    };
    let committed = File::open(&shared.disk)
        .and_then(|disk| disk.sync_all())
        .and_then(|()| crate::suspend::commit(&shared.dir, &record));
    if let Err(e) = committed {
        vm.resume();
        let _ = std::fs::remove_dir_all(&dir);
        return Err(format!("writing the suspended state: {e}"));
    }
    tracing::info!(
        ram_mib = snap.ram_bytes >> 20,
        pause = ?snap.pause,
        total = ?started.elapsed(),
        "suspended"
    );
    Ok(Stats {
        ram_bytes: snap.ram_bytes,
        stopped: started.elapsed(),
    })
}
