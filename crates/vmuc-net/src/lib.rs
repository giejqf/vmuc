// SPDX-License-Identifier: Apache-2.0
//! Networking: the vmnet relay helper (`vmuc-vmnet-helper`, the one
//! privileged piece), the VMM's side of starting it, and the port
//! forwarder. The user-space NAT is v0.2 work.

pub mod helper;
#[cfg(target_os = "macos")]
pub mod vmnet;
