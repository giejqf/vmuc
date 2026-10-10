// SPDX-License-Identifier: Apache-2.0
//! Accept loop and per-connection dispatch.

use std::io;
use std::thread;

use vmuc_proto::frame::{read_frame, write_frame};
use vmuc_proto::{AgentRequest, AgentResponse, PROTO_VERSION};

use crate::handlers;
use crate::listen::{Conn, Listener};

pub fn run(listener: &Listener) -> anyhow::Result<()> {
    loop {
        match listener.accept() {
            Ok(conn) => {
                thread::Builder::new()
                    .name("vmuc-agent-conn".into())
                    .spawn(move || {
                        if let Err(e) = handle(conn) {
                            eprintln!("vmuc-agent: connection ended with error: {e}");
                        }
                    })?;
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => {
                eprintln!("vmuc-agent: accept failed: {e}; retrying");
                thread::sleep(std::time::Duration::from_millis(200));
            }
        }
    }
}

/// One request per connection. Streaming methods own the connection
/// until it closes.
pub fn handle(mut conn: Box<dyn Conn>) -> io::Result<()> {
    let Some(req) = read_frame::<_, AgentRequest>(&mut conn)? else {
        return Ok(());
    };
    match req {
        AgentRequest::Hello { proto_version } => {
            let resp = if proto_version == PROTO_VERSION {
                handlers::hello()
            } else {
                AgentResponse::Error {
                    message: format!(
                        "protocol mismatch: host speaks v{proto_version}, agent speaks v{PROTO_VERSION}"
                    ),
                }
            };
            write_frame(&mut conn, &resp)
        }
        AgentRequest::MemInfo => write_frame(&mut conn, &or_error(handlers::meminfo())),
        AgentRequest::Psi => write_frame(&mut conn, &or_error(handlers::psi())),
        AgentRequest::DropCaches => write_frame(&mut conn, &or_error(handlers::drop_caches())),
        AgentRequest::Compact => write_frame(&mut conn, &or_error(handlers::compact())),
        AgentRequest::Addresses => write_frame(&mut conn, &or_error(handlers::addresses())),
        AgentRequest::Exec {
            argv,
            stdin,
            timeout,
        } => write_frame(
            &mut conn,
            &or_error(handlers::exec(&argv, stdin.as_deref(), timeout)),
        ),
        AgentRequest::TimeSync {
            unix_nanos,
            max_drift_ms,
        } => write_frame(
            &mut conn,
            &or_error(handlers::time_sync(unix_nanos, max_drift_ms)),
        ),
        AgentRequest::SocketsWatch => handlers::sockets_watch(conn),
        AgentRequest::Tunnel { addr } => handlers::tunnel(conn, &addr),
    }
}

fn or_error(r: io::Result<AgentResponse>) -> AgentResponse {
    r.unwrap_or_else(|e| AgentResponse::Error {
        message: e.to_string(),
    })
}
