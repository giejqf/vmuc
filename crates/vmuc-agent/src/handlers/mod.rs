// SPDX-License-Identifier: Apache-2.0
//! Request handlers. Each is a plain function over `std` and `/proc`, so
//! they are unit-testable on any Linux box.

mod exec;
mod mem;
mod net;
mod sockets;
mod time;
mod tunnel;

pub use exec::exec;
pub use mem::{compact, drop_caches, meminfo, psi};
pub use net::addresses;
pub use sockets::sockets_watch;
pub use time::time_sync;
pub use tunnel::tunnel;

use vmuc_proto::{AgentResponse, PROTO_VERSION};

pub fn hello() -> AgentResponse {
    AgentResponse::Hello {
        agent_version: env!("CARGO_PKG_VERSION").to_string(),
        proto_version: PROTO_VERSION,
        kernel: std::fs::read_to_string("/proc/sys/kernel/osrelease")
            .map(|s| s.trim().to_string())
            .unwrap_or_default(),
        hostname: std::fs::read_to_string("/proc/sys/kernel/hostname")
            .map(|s| s.trim().to_string())
            .unwrap_or_default(),
    }
}
