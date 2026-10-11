// SPDX-License-Identifier: Apache-2.0
//! Client side of an instance's `vmm.sock`: one request per connection,
//! length-prefixed JSON frames. `VsockConnect` turns the connection into
//! a raw byte stream to a guest port.

use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, bail};
use vmuc_config::paths::instance as files;
use vmuc_proto::frame::{read_frame, write_frame};
use vmuc_proto::{ControlRequest, ControlResponse};

fn connect(dir: &Path) -> anyhow::Result<UnixStream> {
    let path = dir.join(files::VMM_SOCK);
    UnixStream::connect(&path)
        .with_context(|| format!("connecting to {} (is it running?)", path.display()))
}

/// Sends one request and returns the reply.
pub fn request(dir: &Path, req: &ControlRequest) -> anyhow::Result<ControlResponse> {
    let mut s = connect(dir)?;
    s.set_read_timeout(Some(Duration::from_secs(30)))?;
    write_frame(&mut s, req)?;
    read_frame(&mut s)?
        .ok_or_else(|| anyhow::anyhow!("the VMM closed the connection without a reply"))
}

/// A byte stream to `port` inside the guest, through the VMM's vsock.
pub fn vsock_connect(dir: &Path, port: u32) -> anyhow::Result<UnixStream> {
    let mut s = connect(dir)?;
    s.set_read_timeout(Some(Duration::from_secs(10)))?;
    write_frame(&mut s, &ControlRequest::VsockConnect { port })?;
    match read_frame::<_, ControlResponse>(&mut s)? {
        Some(ControlResponse::Ok) => {
            s.set_read_timeout(None)?;
            Ok(s)
        }
        Some(ControlResponse::Error { message }) => bail!("vsock port {port}: {message}"),
        other => bail!("unexpected reply to vsock connect: {other:?}"),
    }
}
