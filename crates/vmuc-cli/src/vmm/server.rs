// SPDX-License-Identifier: Apache-2.0
//! The `vmm.sock` control server: one request per connection, a thread
//! per connection (control traffic is tiny). `VsockConnect` hands the
//! connection itself to the vsock muxer, which replies once the guest
//! has answered and then carries raw bytes.

use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use vmuc_config::paths::instance as files;
use vmuc_proto::jsonl::{read_line, write_line};
use vmuc_proto::{ControlRequest, ControlResponse};

use super::Shared;

pub fn spawn(dir: &Path, shared: Arc<Shared>) -> std::io::Result<()> {
    let path = dir.join(files::VMM_SOCK);
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path)?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    std::thread::Builder::new()
        .name("control".into())
        .spawn(move || {
            for conn in listener.incoming().flatten() {
                let shared = Arc::clone(&shared);
                let _ = std::thread::Builder::new()
                    .name("control-conn".into())
                    .spawn(move || handle(conn, &shared));
            }
        })?;
    Ok(())
}

fn handle(mut s: UnixStream, shared: &Arc<Shared>) {
    let Ok(Some(req)) = read_line::<_, ControlRequest>(&mut s) else {
        return;
    };
    let resp = match req {
        ControlRequest::Status => ControlResponse::Status(shared.status()),
        ControlRequest::Stop { force } => {
            shared.stop(force);
            ControlResponse::Ok
        }
        ControlRequest::Stat => ControlResponse::Stat(super::memory::stat(shared)),
        ControlRequest::ReclaimNow => match super::memory::reclaim_now(shared) {
            Ok(()) => ControlResponse::Ok,
            Err(message) => ControlResponse::Error { message },
        },
        ControlRequest::SimulatePressure { level, seconds } => {
            super::memory::simulate(shared, level, Duration::from_secs(seconds.into()));
            ControlResponse::Ok
        }
        ControlRequest::VsockConnect { port } => {
            let reply = Box::new(|stream: &mut _, r: Result<(), String>| {
                let resp = match r {
                    Ok(()) => ControlResponse::Ok,
                    Err(message) => ControlResponse::Error { message },
                };
                let _ = write_line(stream, &resp);
            });
            let Ok(mut fallback) = s.try_clone() else {
                return;
            };
            if let Err(e) = shared.vsock.connect(s, port, reply) {
                let _ = write_line(
                    &mut fallback,
                    &ControlResponse::Error {
                        message: e.to_string(),
                    },
                );
            }
            return;
        }
        ControlRequest::Forwards => ControlResponse::Forwards(super::forward::list(shared)),
        ControlRequest::Suspend => match super::suspend::request(shared) {
            Ok(st) => ControlResponse::Suspended {
                ram_bytes: st.ram_bytes,
                millis: st.stopped.as_millis() as u64,
            },
            Err(message) => ControlResponse::Error { message },
        },
    };
    let _ = write_line(&mut s, &resp);
}
