// SPDX-License-Identifier: Apache-2.0
//! Wire types shared by the CLI, the per-instance VMM and the guest agent.
//!
//! Two channels use these types, both carrying newline-free JSON in
//! length-prefixed frames (see [`frame`]):
//!
//! - **CLI ↔ VMM** over the instance's `vmm.sock` unix socket:
//!   [`ControlRequest`] / [`ControlResponse`], plus [`Event`] lines
//!   appended to `events.jsonl`.
//! - **Host ↔ guest agent** over vsock port [`AGENT_VSOCK_PORT`]:
//!   [`AgentRequest`] / [`AgentResponse`]. One request per connection;
//!   streaming methods keep sending responses, and `Tunnel` switches the
//!   connection to raw bytes after `TunnelReady`.
//!
//! Control traffic is tiny, so JSON wins on debuggability. Bulk data
//! never goes through these frames.

pub mod frame;

use std::net::IpAddr;

use serde::{Deserialize, Serialize};

/// Protocol version; bump on any incompatible change to the enums below.
pub const PROTO_VERSION: u32 = 1;

/// vsock port the guest agent listens on.
pub const AGENT_VSOCK_PORT: u32 = 7700;

/// The host's well-known vsock CID.
pub const HOST_CID: u32 = 2;

// ---------------------------------------------------------------------------
// Host ↔ guest agent
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "method", content = "params", rename_all = "snake_case")]
pub enum AgentRequest {
    /// Handshake. The agent answers with its version; mismatched
    /// `proto_version` is an error.
    Hello { proto_version: u32 },
    /// `/proc/meminfo` summary.
    MemInfo,
    /// `/proc/pressure/memory`.
    Psi,
    /// `sync; echo 3 > /proc/sys/vm/drop_caches`.
    DropCaches,
    /// `echo 1 > /proc/sys/vm/compact_memory`.
    Compact,
    /// Stream [`AgentResponse::SocketEvent`] until the connection closes.
    /// Starts with one `Added` per currently listening socket.
    SocketsWatch,
    /// Guest IP addresses and interfaces.
    Addresses,
    /// Connect to `addr` inside the guest, reply `TunnelReady`, then
    /// relay raw bytes both ways until either side closes.
    Tunnel { addr: String },
    /// Run a command as root and capture its output.
    Exec {
        argv: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stdin: Option<String>,
        /// Seconds; `None` means no limit.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout: Option<u32>,
    },
    /// Step the guest clock to the host's time when the drift exceeds
    /// `max_drift_ms`.
    TimeSync { unix_nanos: i64, max_drift_ms: u32 },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", content = "data", rename_all = "snake_case")]
pub enum AgentResponse {
    Hello {
        agent_version: String,
        proto_version: u32,
        kernel: String,
        hostname: String,
    },
    MemInfo(MemInfo),
    Psi(Psi),
    Ok,
    SocketEvent(SocketEvent),
    Addresses(Vec<Interface>),
    TunnelReady,
    Exec {
        status: i32,
        stdout: String,
        stderr: String,
    },
    TimeSync {
        /// Signed drift measured before any adjustment, in milliseconds.
        drift_ms: i64,
        adjusted: bool,
    },
    Error {
        message: String,
    },
}

/// Bytes, from `/proc/meminfo` (kB there, bytes here).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemInfo {
    pub total: u64,
    pub free: u64,
    pub available: u64,
    pub buffers: u64,
    pub cached: u64,
    pub swap_total: u64,
    pub swap_free: u64,
    pub shmem: u64,
    pub anon_pages: u64,
}

/// `/proc/pressure/memory` averages, in percent.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct Psi {
    pub some_avg10: f32,
    pub some_avg60: f32,
    pub full_avg10: f32,
    pub full_avg60: f32,
}

impl Eq for Psi {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Proto {
    Tcp,
    Udp,
}

/// A listening socket in the guest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Socket {
    pub proto: Proto,
    pub ip: IpAddr,
    pub port: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SocketEvent {
    Added(Socket),
    Removed(Socket),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Interface {
    pub name: String,
    pub mac: Option<String>,
    pub addrs: Vec<IpAddr>,
}

// ---------------------------------------------------------------------------
// CLI ↔ VMM
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "method", content = "params", rename_all = "snake_case")]
pub enum ControlRequest {
    Status,
    Stat,
    Stop {
        force: bool,
    },
    Forwards,
    /// Set the balloon target in bytes (0 = fully deflated).
    BalloonTarget {
        bytes: u64,
    },
    /// Ask the guest to drop caches and compact so reporting can drain.
    ReclaimNow,
    /// Open a vsock stream to the guest on `port`. After `Ok` the
    /// connection carries raw bytes (used by `vmuc _proxy` and the port
    /// forwarder, which then speak the agent's `Tunnel` protocol).
    VsockConnect {
        port: u32,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", content = "data", rename_all = "snake_case")]
pub enum ControlResponse {
    Status(InstanceStatus),
    Stat(Stat),
    Ok,
    Forwards(Vec<Forward>),
    Error { message: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Booting,
    Running,
    Degraded,
    Stopping,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstanceStatus {
    pub name: String,
    pub phase: Phase,
    pub boot_id: String,
    pub pid: u32,
    pub cpus: u32,
    pub memory: u64,
    pub addrs: Vec<IpAddr>,
    pub uptime_secs: u64,
    pub errors: Vec<String>,
}

/// Memory accounting: the numbers behind `vmuc stat`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stat {
    /// Host `phys_footprint` of the VMM process.
    pub host_footprint: u64,
    /// Guest view.
    pub guest_total: u64,
    pub guest_available: u64,
    /// Cumulative bytes the guest reported free.
    pub reported: u64,
    /// Cumulative bytes released with unmap/refresh/remap.
    pub released: u64,
    /// Bytes currently held by the balloon (inflation), 0 when off.
    pub balloon: u64,
    /// p50 / p99 release latency per 2 MiB range, microseconds.
    pub release_p50_us: u32,
    pub release_p99_us: u32,
    pub vcpu_exec_nanos: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Forward {
    pub proto: Proto,
    pub host: String,
    pub guest: String,
    pub state: ForwardState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ForwardState {
    Forwarding,
    Failed,
    Stopped,
}

/// One line of `events.jsonl`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Event {
    /// Unix milliseconds.
    pub time: u64,
    #[serde(flatten)]
    pub kind: EventKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum EventKind {
    Booting {
        boot_id: String,
    },
    Running {
        addrs: Vec<IpAddr>,
    },
    Degraded {
        errors: Vec<String>,
    },
    Requirement {
        step: u32,
        total: u32,
        description: String,
        done: bool,
        failed: bool,
    },
    Forward(Forward),
    Exiting {
        reason: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_request_round_trips_and_is_tagged() {
        let r = AgentRequest::Tunnel {
            addr: "127.0.0.1:22".into(),
        };
        let s = serde_json::to_string(&r).unwrap();
        assert_eq!(s, r#"{"method":"tunnel","params":{"addr":"127.0.0.1:22"}}"#);
        assert_eq!(serde_json::from_str::<AgentRequest>(&s).unwrap(), r);
        let s = serde_json::to_string(&AgentRequest::MemInfo).unwrap();
        assert_eq!(s, r#"{"method":"mem_info"}"#);
    }

    #[test]
    fn event_is_flat() {
        let e = Event {
            time: 1,
            kind: EventKind::Booting {
                boot_id: "b".into(),
            },
        };
        let s = serde_json::to_string(&e).unwrap();
        assert_eq!(s, r#"{"time":1,"event":"booting","boot_id":"b"}"#);
        assert_eq!(serde_json::from_str::<Event>(&s).unwrap(), e);
    }

    #[test]
    fn socket_event_round_trip() {
        let ev = SocketEvent::Added(Socket {
            proto: Proto::Tcp,
            ip: "0.0.0.0".parse().unwrap(),
            port: 22,
        });
        let s = serde_json::to_string(&AgentResponse::SocketEvent(ev.clone())).unwrap();
        let back: AgentResponse = serde_json::from_str(&s).unwrap();
        assert_eq!(back, AgentResponse::SocketEvent(ev));
    }
}
