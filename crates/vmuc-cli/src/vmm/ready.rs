// SPDX-License-Identifier: Apache-2.0
//! Readiness: the instance is `running` once the guest agent answers and
//! sshd sends its banner through an agent tunnel. Probes retry quickly
//! (a refused vsock connect costs a few microseconds) because this is on
//! the `vmuc start` critical path.

use std::io::Read;
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use vmuc_proto::frame::{read_frame, write_frame};
use vmuc_proto::{AGENT_VSOCK_PORT, AgentRequest, AgentResponse, EventKind, PROTO_VERSION, Phase};
use vmuc_virtio::devices::vsock::VsockConnector;

use super::Shared;

/// Give up on readiness after this long (cloud-init can install a lot).
const READY_TIMEOUT: Duration = Duration::from_secs(600);
const RETRY: Duration = Duration::from_millis(50);

/// A byte stream to a guest vsock port, opened in-process.
pub fn dial(vsock: &VsockConnector, port: u32, timeout: Duration) -> std::io::Result<UnixStream> {
    let (ours, theirs) = UnixStream::pair()?;
    let (tx, rx) = mpsc::channel();
    vsock.connect(
        theirs,
        port,
        Box::new(move |_, r| {
            let _ = tx.send(r);
        }),
    )?;
    match rx.recv_timeout(timeout) {
        Ok(Ok(())) => Ok(ours),
        Ok(Err(e)) => Err(std::io::Error::new(
            std::io::ErrorKind::ConnectionRefused,
            e,
        )),
        Err(_) => Err(std::io::ErrorKind::TimedOut.into()),
    }
}

/// One agent request on a fresh connection.
pub fn agent_call(
    vsock: &VsockConnector,
    req: &AgentRequest,
    timeout: Duration,
) -> std::io::Result<AgentResponse> {
    let mut s = dial(vsock, AGENT_VSOCK_PORT, timeout)?;
    s.set_read_timeout(Some(timeout))?;
    write_frame(&mut s, req)?;
    read_frame(&mut s)?.ok_or_else(|| std::io::ErrorKind::UnexpectedEof.into())
}

/// sshd answers through a tunnel with its `SSH-` banner.
fn probe_ssh(vsock: &VsockConnector) -> std::io::Result<()> {
    let mut s = dial(vsock, AGENT_VSOCK_PORT, Duration::from_secs(1))?;
    s.set_read_timeout(Some(Duration::from_secs(2)))?;
    write_frame(
        &mut s,
        &AgentRequest::Tunnel {
            addr: "127.0.0.1:22".into(),
        },
    )?;
    match read_frame::<_, AgentResponse>(&mut s)? {
        Some(AgentResponse::TunnelReady) => {}
        other => return Err(std::io::Error::other(format!("tunnel: {other:?}"))),
    }
    let mut banner = [0u8; 4];
    s.read_exact(&mut banner)?;
    if &banner == b"SSH-" {
        Ok(())
    } else {
        Err(std::io::Error::other("not an ssh banner"))
    }
}

pub fn spawn(shared: Arc<Shared>) {
    let _ = std::thread::Builder::new()
        .name("ready".into())
        .spawn(move || run(&shared));
}

fn requirement(shared: &Shared, step: u32, description: &str, done: bool, failed: bool) {
    shared.events.push(EventKind::Requirement {
        step,
        total: 2,
        description: description.into(),
        done,
        failed,
    });
}

/// Retries `probe` until it succeeds, the VM stops, or the deadline.
fn wait_for(shared: &Shared, deadline: Instant, mut probe: impl FnMut() -> bool) -> bool {
    loop {
        if shared.machine.stopping() {
            return false;
        }
        if probe() {
            return true;
        }
        if Instant::now() > deadline {
            return false;
        }
        std::thread::sleep(RETRY);
    }
}

fn run(shared: &Shared) {
    let deadline = Instant::now() + READY_TIMEOUT;
    let hello = AgentRequest::Hello {
        proto_version: PROTO_VERSION,
    };
    requirement(shared, 1, "guest agent", false, false);
    let agent_up = wait_for(shared, deadline, || {
        matches!(
            agent_call(&shared.vsock, &hello, Duration::from_secs(1)),
            Ok(AgentResponse::Hello { .. })
        )
    });
    if !agent_up {
        return fail(shared, 1, "guest agent");
    }
    requirement(shared, 1, "guest agent", true, false);
    requirement(shared, 2, "ssh", false, false);
    if !wait_for(shared, deadline, || probe_ssh(&shared.vsock).is_ok()) {
        return fail(shared, 2, "ssh");
    }
    requirement(shared, 2, "ssh", true, false);
    shared.set_phase(Phase::Running);
    shared.events.push(EventKind::Running { addrs: Vec::new() });

    // The guest clock starts from the RTC's whole seconds; tighten it.
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos() as i64);
    let sync = AgentRequest::TimeSync {
        unix_nanos: now,
        max_drift_ms: 100,
    };
    let _ = agent_call(&shared.vsock, &sync, Duration::from_secs(2));
}

fn fail(shared: &Shared, step: u32, what: &str) {
    if shared.machine.stopping() {
        return;
    }
    requirement(shared, step, what, false, true);
    let msg = format!("{what} did not become ready within {READY_TIMEOUT:?}");
    shared
        .errors
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .push(msg.clone());
    shared.set_phase(Phase::Degraded);
    shared
        .events
        .push(EventKind::Degraded { errors: vec![msg] });
}
