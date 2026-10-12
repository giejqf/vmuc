// SPDX-License-Identifier: Apache-2.0
//! The instance's NIC: virtio-net to a vmnet interface through the root
//! helper. Without the helper (not installed, no sudoers rule) the
//! instance still boots, with a warning: ssh, port forwards and the agent
//! all ride vsock.

use std::process::Child;

use vmuc_config::ResolvedConfig;
use vmuc_config::schema::NetworkMode;
use vmuc_net::helper::Mode;
use vmuc_proto::EventKind;
use vmuc_virtio::devices::net::{Net, parse_mac};
use vmuc_vmm::VmBuilder;

use crate::assets;
use crate::state::EventLog;

/// Adds the NIC if the helper starts; returns the helper process, which
/// exits when the VMM's end of the socket closes.
pub fn attach(b: &mut VmBuilder, resolved: &ResolvedConfig, events: &EventLog) -> Option<Child> {
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
    match Net::new(mac, launched.socket, launched.hello.virtio_header, rings)
        .map_err(|e| e.to_string())
        .and_then(|net| {
            b.add_virtio(Box::new(net))
                .map(|_| ())
                .map_err(|e| e.to_string())
        }) {
        Ok(()) => Some(launched.child),
        Err(e) => warn(format!("networking unavailable: {e}")),
    }
}
