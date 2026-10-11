// SPDX-License-Identifier: Apache-2.0
//! virtiofs server: the FUSE protocol the guest's virtiofs driver speaks,
//! answered by a passthrough backend over a host directory. Independent
//! of the transport: `vmuc-virtio`'s virtio-fs device feeds it requests
//! read from virtqueues; tests feed it bytes.

pub mod abi;
pub mod host;
pub mod inodes;
pub mod passthrough;
