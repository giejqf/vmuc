// SPDX-License-Identifier: Apache-2.0
//! `vmuc _proxy`: an ssh `ProxyCommand` that tunnels stdio to a TCP port
//! inside the guest through the agent.

use std::io::{self, Read, Write};
use std::thread;

use anyhow::bail;
use vmuc_proto::frame::{read_frame, write_frame};
use vmuc_proto::{AgentRequest, AgentResponse};

use crate::agent_client;

pub fn run(agent_spec: &str, guest_port: u16) -> anyhow::Result<()> {
    let mut conn = agent_client::connect(agent_spec)?;
    write_frame(
        &mut conn,
        &AgentRequest::Tunnel {
            addr: format!("127.0.0.1:{guest_port}"),
        },
    )?;
    match read_frame::<_, AgentResponse>(&mut conn)? {
        Some(AgentResponse::TunnelReady) => {}
        Some(AgentResponse::Error { message }) => bail!("agent: {message}"),
        other => bail!("unexpected reply from agent: {other:?}"),
    }

    let mut conn_rx = conn.split()?;
    let mut conn_tx = conn;
    let stdin_to_conn = thread::spawn(move || {
        let r = io::copy(&mut io::stdin().lock(), &mut conn_tx);
        let _ = conn_tx.shutdown_write();
        r
    });
    let mut stdout = io::stdout().lock();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = match conn_rx.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e.into()),
        };
        stdout.write_all(&buf[..n])?;
        stdout.flush()?;
    }
    // ssh exits when its stdout closes; don't wait for stdin forever.
    drop(stdout);
    let _ = stdin_to_conn;
    Ok(())
}
