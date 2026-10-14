// SPDX-License-Identifier: Apache-2.0
//! Networking: the vmnet relay helper (`vmuc-vmnet-helper`, the one
//! privileged piece), the shared-memory rings it relays through, and the
//! VMM's side of starting it. The user-space NAT is v0.2 work.

pub mod handoff;
pub mod helper;
pub mod launch;
pub mod region;
pub mod ring;
pub mod ringrelay;
#[cfg(target_os = "macos")]
pub mod vmnet;
