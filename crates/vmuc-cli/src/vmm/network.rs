// SPDX-License-Identifier: Apache-2.0
//! The instance's NIC: virtio-net to a vmnet interface through the root
//! helper. Without the helper (not installed, no sudoers rule) the
//! instance still boots, with a warning: ssh, port forwards and the agent
//! all ride vsock.

use std::io::Read;
use std::process::Child;
use std::sync::Arc;

use vmuc_config::ResolvedConfig;
use vmuc_config::schema::NetworkMode;
use vmuc_net::helper::Mode;
use vmuc_proto::EventKind;
use vmuc_virtio::devices::net::{Net, NetCounters, parse_mac};
use vmuc_vmm::VmBuilder;

use crate::assets;
use crate::state::EventLog;

/// Adds the NIC if the helper starts, and watches the helper. Returns the
/// NIC's traffic counters.
pub fn attach(
    b: &mut VmBuilder,
    resolved: &ResolvedConfig,
    events: &EventLog,
) -> Option<Arc<NetCounters>> {
    let mode = match resolved.network.mode {
        NetworkMode::Shared => Mode::Shared,
        NetworkMode::Bridged => Mode::Bridged(resolved.network.interface.clone()),
    };
    let warn = |msg: String| {
        tracing::warn!("{msg}");
        events.push(EventKind::Warning { message: msg });
        None
    };
    let Some(mac) = parse_mac(&resolved.network.mac_address) else {
        return warn(format!(
            "bad MAC address {:?}; no networking",
            resolved.network.mac_address
        ));
    };
    let launched = match vmuc_net::launch::launch(&assets::vmnet_helper(), &mode) {
        Ok(l) => l,
        Err(e) => return warn(format!("networking unavailable: {e}")),
    };
    tracing::info!(?launched.hello, "vmnet interface up");
    let rings = launched.region.as_ref();
    let net = match Net::new(mac, launched.socket, launched.hello.virtio_header, rings) {
        Ok(net) => net,
        Err(e) => return warn(format!("networking unavailable: {e}")),
    };
    let counters = net.counters();
    if let Err(e) = b.add_virtio(Box::new(net)) {
        return warn(format!("networking unavailable: {e}"));
    }
    watch(launched.child, events.clone());
    Some(counters)
}

/// The helper ends by itself only when something went wrong (otherwise it
/// outlives this process by up to a second): say so, with what it printed,
/// instead of leaving the guest's network silently dead.
fn watch(mut child: Child, events: EventLog) {
    let spawned = std::thread::Builder::new()
        .name("vmnet-helper".into())
        .spawn(move || {
            let mut stderr = String::new();
            if let Some(mut s) = child.stderr.take() {
                let _ = s.read_to_string(&mut stderr);
            }
            let status = child
                .wait()
                .map_or_else(|e| e.to_string(), |s| s.to_string());
            let detail = stderr.trim();
            let message = if detail.is_empty() {
                format!("networking stopped: the vmnet helper exited ({status})")
            } else {
                format!("networking stopped: {detail} ({status})")
            };
            tracing::warn!("{message}");
            events.push(EventKind::Warning { message });
        });
    if let Err(e) = spawned {
        tracing::warn!("cannot watch the vmnet helper: {e}");
    }
}

#[cfg(test)]
mod tests {
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    use super::*;

    #[test]
    fn a_helper_that_dies_leaves_a_warning_with_its_words() {
        let dir = tempfile::tempdir().unwrap();
        let events = EventLog::create(dir.path()).unwrap();
        let child = Command::new("sh")
            .args([
                "-c",
                "echo 'vmuc-vmnet-helper: shared region: too small' >&2; exit 3",
            ])
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        watch(child, events);
        let t = Instant::now();
        let warning = loop {
            let found = crate::state::read_events(dir.path())
                .into_iter()
                .find_map(|e| match e.kind {
                    EventKind::Warning { message } => Some(message),
                    _ => None,
                });
            if let Some(w) = found {
                break w;
            }
            assert!(t.elapsed() < Duration::from_secs(5), "no warning");
            std::thread::sleep(Duration::from_millis(10));
        };
        assert!(warning.contains("shared region: too small"), "{warning}");
        assert!(warning.contains("exit status: 3"), "{warning}");
    }
}
