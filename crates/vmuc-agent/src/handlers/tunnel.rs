// SPDX-License-Identifier: Apache-2.0
//! `Tunnel`: connect to a guest TCP address and splice bytes with the
//! host connection in both directions. This is how `vmuc shell` reaches
//! sshd and how dynamic port forwards reach guest services.

use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpStream};
use std::thread;
use std::time::Duration;

use vmuc_proto::AgentResponse;
use vmuc_proto::frame::write_frame;

use crate::listen::Conn;

pub fn tunnel(mut conn: Box<dyn Conn>, addr: &str) -> io::Result<()> {
    let guest = match connect(addr) {
        Ok(s) => s,
        Err(e) => {
            write_frame(
                &mut conn,
                &AgentResponse::Error {
                    message: format!("connect {addr}: {e}"),
                },
            )?;
            return Ok(());
        }
    };
    write_frame(&mut conn, &AgentResponse::TunnelReady)?;
    splice(conn, guest)
}

fn connect(addr: &str) -> io::Result<TcpStream> {
    let s = TcpStream::connect_timeout(
        &addr
            .parse()
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?,
        Duration::from_secs(5),
    )?;
    s.set_nodelay(true)?;
    Ok(s)
}

/// Copies host→guest on this thread and guest→host on a helper thread.
/// Each direction propagates EOF as a write-side shutdown so half-closed
/// connections (ssh does this) drain correctly.
fn splice(host: Box<dyn Conn>, guest: TcpStream) -> io::Result<()> {
    let mut host_rx = host;
    let mut host_tx = host_rx.try_clone_conn()?;
    let mut guest_rx = guest.try_clone()?;
    let mut guest_tx = guest;

    let g2h = thread::spawn(move || {
        let r = copy(&mut guest_rx, &mut host_tx);
        let _ = host_tx.shutdown_write();
        r
    });
    let r = copy(&mut host_rx, &mut guest_tx);
    let _ = guest_tx.shutdown(Shutdown::Write);
    let _ = g2h.join();
    r.map(|_| ())
}

fn copy(from: &mut dyn Read, to: &mut dyn Write) -> io::Result<u64> {
    let mut buf = vec![0u8; 64 * 1024];
    let mut total = 0u64;
    loop {
        let n = match from.read(&mut buf) {
            Ok(0) => return Ok(total),
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        to.write_all(&buf[..n])?;
        total += n as u64;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use vmuc_proto::frame::read_frame;

    #[test]
    fn tunnel_relays_both_ways() {
        // Guest-side echo server.
        let echo = TcpListener::bind("127.0.0.1:0").unwrap();
        let echo_addr = echo.local_addr().unwrap();
        thread::spawn(move || {
            let (mut s, _) = echo.accept().unwrap();
            let mut buf = [0u8; 64];
            loop {
                let n = s.read(&mut buf).unwrap();
                if n == 0 {
                    break;
                }
                s.write_all(&buf[..n]).unwrap();
            }
        });

        // "Host" side: a TCP pair standing in for vsock.
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let host_addr = l.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (s, _) = l.accept().unwrap();
            tunnel(Box::new(s), &echo_addr.to_string()).unwrap();
        });

        let mut client = TcpStream::connect(host_addr).unwrap();
        let ready: AgentResponse = read_frame(&mut client).unwrap().unwrap();
        assert_eq!(ready, AgentResponse::TunnelReady);
        client.write_all(b"ping").unwrap();
        let mut buf = [0u8; 4];
        client.read_exact(&mut buf).unwrap();
        assert_eq!(&buf, b"ping");
        client.shutdown(Shutdown::Write).unwrap();
        let mut rest = Vec::new();
        client.read_to_end(&mut rest).unwrap();
        assert_eq!(rest.len(), 0);
        server.join().unwrap();
    }

    #[test]
    fn unreachable_target_reports_error_frame() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let host_addr = l.local_addr().unwrap();
        thread::spawn(move || {
            let (s, _) = l.accept().unwrap();
            tunnel(Box::new(s), "127.0.0.1:1").unwrap();
        });
        let mut client = TcpStream::connect(host_addr).unwrap();
        let r: AgentResponse = read_frame(&mut client).unwrap().unwrap();
        assert!(matches!(r, AgentResponse::Error { .. }));
    }
}
