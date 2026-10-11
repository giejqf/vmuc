// SPDX-License-Identifier: Apache-2.0
//! Port forwarding over vsock: guest listeners appear on the host's
//! loopback, with no network device or root involved.
//!
//! One thread keeps an agent `sockets_watch` stream open (reconnecting
//! when the agent restarts) and turns listener events into commands; one
//! mio loop owns the host listeners, so idle costs nothing; every
//! accepted connection gets its own thread pair that dials the agent and
//! splices through a `tunnel` to the guest port. Rules are the instance's
//! `portForwards` (first match wins, Lima's shape) ending in the
//! catch-all. TCP only for now; guest port 22 is left alone because ssh
//! already rides vsock.

use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, Shutdown, SocketAddr, TcpStream};
use std::os::unix::net::UnixStream;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, PoisonError};
use std::time::Duration;

use mio::net::TcpListener;
use mio::{Events, Interest, Poll, Token, Waker};
use vmuc_config::defaults::ResolvedPortForward;
use vmuc_config::schema::Proto as RuleProto;
use vmuc_proto::frame::{read_frame, write_frame};
use vmuc_proto::{
    AGENT_VSOCK_PORT, AgentRequest, AgentResponse, EventKind, Forward, ForwardState, Proto, Socket,
    SocketEvent,
};

use super::{Shared, ready};

const WAKER: Token = Token(0);

/// Where a guest listener should appear on the host, if anywhere.
///
/// A rule with an unspecified guest IP matches listeners on the wildcard
/// address or plain localhost, as Lima's catch-all does, not every
/// loopback alias (systemd-resolved's 127.0.0.53 stub stays in the
/// guest). Privileged host ports are skipped: the VMM cannot bind them.
pub fn host_addr(rules: &[ResolvedPortForward], s: &Socket) -> Option<SocketAddr> {
    if s.proto != Proto::Tcp || s.port == 22 {
        return None;
    }
    let common = s.ip.is_unspecified()
        || s.ip == IpAddr::V4(Ipv4Addr::LOCALHOST)
        || s.ip == IpAddr::V6(Ipv6Addr::LOCALHOST);
    let rule = rules.iter().find(|r| {
        let proto_ok = matches!(r.proto, RuleProto::Any | RuleProto::Tcp);
        let [lo, hi] = r.guest_port_range;
        let ip_ok = if r.guest_ip.is_unspecified() {
            common
        } else {
            r.guest_ip == s.ip
        };
        proto_ok && (lo..=hi).contains(&s.port) && ip_ok
    })?;
    if rule.ignore {
        return None;
    }
    let port = rule.host_port_range[0].checked_add(s.port - rule.guest_port_range[0])?;
    (port >= 1024).then(|| SocketAddr::new(rule.host_ip, port))
}

/// The address to dial inside the guest for a listener.
fn guest_target(s: &Socket) -> SocketAddr {
    let ip = match s.ip {
        IpAddr::V4(v) if v.is_unspecified() => IpAddr::V4(Ipv4Addr::LOCALHOST),
        IpAddr::V6(v) if v.is_unspecified() => IpAddr::V6(Ipv6Addr::LOCALHOST),
        other => other,
    };
    SocketAddr::new(ip, s.port)
}

enum Cmd {
    Add(Socket),
    Remove(Socket),
}

pub fn spawn(shared: Arc<Shared>, rules: Vec<ResolvedPortForward>) -> io::Result<()> {
    let poll = Poll::new()?;
    let waker = Arc::new(Waker::new(poll.registry(), WAKER)?);
    let (tx, rx) = mpsc::channel();
    let watcher_shared = Arc::clone(&shared);
    let watcher_waker = Arc::clone(&waker);
    std::thread::Builder::new()
        .name("forward-watch".into())
        .spawn(move || watch(&watcher_shared, &tx, &watcher_waker))?;
    std::thread::Builder::new()
        .name("forward".into())
        .spawn(move || listen(&shared, poll, &rx, &rules))?;
    Ok(())
}

/// Keeps a `sockets_watch` stream open and forwards its events.
fn watch(shared: &Shared, tx: &Sender<Cmd>, waker: &Waker) {
    while !shared.machine.stopping() {
        let stream = ready::dial(&shared.vsock, AGENT_VSOCK_PORT, Duration::from_secs(2));
        let Ok(mut stream) = stream else {
            std::thread::sleep(Duration::from_secs(1));
            continue;
        };
        if write_frame(&mut stream, &AgentRequest::SocketsWatch).is_err() {
            continue;
        }
        let mut known: Vec<Socket> = Vec::new();
        while let Ok(Some(resp)) = read_frame::<_, AgentResponse>(&mut stream) {
            let cmd = match resp {
                AgentResponse::SocketEvent(SocketEvent::Added(s)) => {
                    known.push(s);
                    Cmd::Add(s)
                }
                AgentResponse::SocketEvent(SocketEvent::Removed(s)) => {
                    known.retain(|k| *k != s);
                    Cmd::Remove(s)
                }
                _ => continue,
            };
            if tx.send(cmd).is_err() {
                return;
            }
            let _ = waker.wake();
        }
        // The stream ended (agent restart, reboot): forget what it reported.
        for s in known {
            let _ = tx.send(Cmd::Remove(s));
        }
        let _ = waker.wake();
        std::thread::sleep(Duration::from_secs(1));
    }
}

struct Listening {
    listener: TcpListener,
    guest: Socket,
}

fn listen(shared: &Arc<Shared>, mut poll: Poll, rx: &Receiver<Cmd>, rules: &[ResolvedPortForward]) {
    let mut events = Events::with_capacity(32);
    let mut listeners: HashMap<Token, Listening> = HashMap::new();
    let mut by_socket: HashMap<Socket, Token> = HashMap::new();
    let mut next = 1usize;
    loop {
        if poll.poll(&mut events, None).is_err() {
            return;
        }
        for ev in &events {
            if ev.token() == WAKER {
                continue;
            }
            let Some(l) = listeners.get(&ev.token()) else {
                continue;
            };
            while let Ok((conn, _)) = l.listener.accept() {
                let guest = guest_target(&l.guest);
                let shared = Arc::clone(shared);
                let _ = std::thread::Builder::new()
                    .name("forward-conn".into())
                    .spawn(move || tunnel(&shared, conn, guest));
            }
        }
        while let Ok(cmd) = rx.try_recv() {
            match cmd {
                Cmd::Add(s) => {
                    let Some(host) = host_addr(rules, &s) else {
                        continue;
                    };
                    if by_socket.contains_key(&s) {
                        continue;
                    }
                    let guest = guest_target(&s).to_string();
                    let state = match TcpListener::bind(host) {
                        Ok(mut listener) => {
                            let token = Token(next);
                            next += 1;
                            if poll
                                .registry()
                                .register(&mut listener, token, Interest::READABLE)
                                .is_ok()
                            {
                                listeners.insert(token, Listening { listener, guest: s });
                                by_socket.insert(s, token);
                                ForwardState::Forwarding
                            } else {
                                ForwardState::Failed
                            }
                        }
                        Err(e) => {
                            tracing::warn!(%host, "port forward: {e}");
                            ForwardState::Failed
                        }
                    };
                    record(shared, host.to_string(), guest, state);
                }
                Cmd::Remove(s) => {
                    if let Some(token) = by_socket.remove(&s)
                        && let Some(mut l) = listeners.remove(&token)
                    {
                        let host = l
                            .listener
                            .local_addr()
                            .map(|a| a.to_string())
                            .unwrap_or_default();
                        let _ = poll.registry().deregister(&mut l.listener);
                        record(
                            shared,
                            host,
                            guest_target(&s).to_string(),
                            ForwardState::Stopped,
                        );
                    }
                }
            }
        }
        if shared.machine.stopping() {
            return;
        }
    }
}

fn record(shared: &Shared, host: String, guest: String, state: ForwardState) {
    let f = Forward {
        proto: Proto::Tcp,
        host,
        guest,
        state,
    };
    {
        let mut all = shared
            .forwards
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        all.retain(|x| x.host != f.host);
        if state != ForwardState::Stopped {
            all.push(f.clone());
        }
    }
    shared.events.push(EventKind::Forward(f));
}

/// One forwarded connection: agent tunnel to the guest port, then bytes
/// both ways with half-closes passed along.
fn tunnel(shared: &Shared, host: mio::net::TcpStream, guest: SocketAddr) {
    // Back to a blocking std stream for the copy threads.
    let host: TcpStream = {
        use std::os::fd::{FromRawFd, IntoRawFd};
        // SAFETY: the fd comes straight out of the mio stream we own.
        unsafe { TcpStream::from_raw_fd(host.into_raw_fd()) }
    };
    let _ = host.set_nonblocking(false);
    let _ = host.set_nodelay(true);
    let Ok(mut agent) = ready::dial(&shared.vsock, AGENT_VSOCK_PORT, Duration::from_secs(2)) else {
        return;
    };
    let req = AgentRequest::Tunnel {
        addr: guest.to_string(),
    };
    if write_frame(&mut agent, &req).is_err() {
        return;
    }
    if !matches!(
        read_frame::<_, AgentResponse>(&mut agent),
        Ok(Some(AgentResponse::TunnelReady))
    ) {
        return;
    }
    splice(host, agent);
}

fn splice(host: TcpStream, agent: UnixStream) {
    let (Ok(mut host_rx), Ok(mut agent_rx)) = (host.try_clone(), agent.try_clone()) else {
        return;
    };
    let (mut host_tx, mut agent_tx) = (host, agent);
    let up = std::thread::spawn(move || {
        let _ = copy(&mut host_rx, &mut agent_tx);
        let _ = agent_tx.shutdown(Shutdown::Write);
    });
    let _ = copy(&mut agent_rx, &mut host_tx);
    let _ = host_tx.shutdown(Shutdown::Write);
    let _ = up.join();
}

fn copy(from: &mut impl Read, to: &mut impl Write) -> io::Result<u64> {
    let mut buf = vec![0u8; 64 * 1024];
    let mut total = 0;
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

/// Current forwards, for `vmm.sock`'s `forwards`.
pub fn list(shared: &Shared) -> Vec<Forward> {
    shared
        .forwards
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sock(ip: &str, port: u16) -> Socket {
        Socket {
            proto: Proto::Tcp,
            ip: ip.parse().unwrap(),
            port,
        }
    }

    #[test]
    fn catch_all_maps_to_the_same_loopback_port_and_skips_ssh() {
        let rules = vec![ResolvedPortForward::catch_all()];
        assert_eq!(
            host_addr(&rules, &sock("0.0.0.0", 8080)),
            Some("127.0.0.1:8080".parse().unwrap())
        );
        assert_eq!(
            host_addr(&rules, &sock("::", 3000)),
            Some("127.0.0.1:3000".parse().unwrap())
        );
        assert_eq!(host_addr(&rules, &sock("0.0.0.0", 22)), None);
        assert_eq!(
            host_addr(&rules, &sock("127.0.0.1", 5000)),
            Some("127.0.0.1:5000".parse().unwrap())
        );
        assert_eq!(
            host_addr(&rules, &sock("127.0.0.53", 53)),
            None,
            "resolved's stub stays"
        );
        assert_eq!(
            host_addr(&rules, &sock("0.0.0.0", 80)),
            None,
            "privileged host port"
        );
        let udp = Socket {
            proto: Proto::Udp,
            ..sock("0.0.0.0", 53)
        };
        assert_eq!(host_addr(&rules, &udp), None);
    }

    #[test]
    fn first_matching_rule_wins_including_ignores_and_offsets() {
        let mut ignore = ResolvedPortForward::catch_all();
        ignore.guest_port_range = [5000, 5999];
        ignore.ignore = true;
        let mut shift = ResolvedPortForward::catch_all();
        shift.guest_port_range = [8000, 8010];
        shift.host_port_range = [18000, 18010];
        let rules = vec![ignore, shift, ResolvedPortForward::catch_all()];
        assert_eq!(host_addr(&rules, &sock("0.0.0.0", 5432)), None);
        assert_eq!(
            host_addr(&rules, &sock("0.0.0.0", 8005)),
            Some("127.0.0.1:18005".parse().unwrap())
        );
        assert_eq!(
            host_addr(&rules, &sock("0.0.0.0", 9000)),
            Some("127.0.0.1:9000".parse().unwrap())
        );
    }

    #[test]
    fn guest_targets_use_loopback_for_wildcards() {
        assert_eq!(
            guest_target(&sock("0.0.0.0", 80)),
            "127.0.0.1:80".parse().unwrap()
        );
        assert_eq!(guest_target(&sock("::", 80)), "[::1]:80".parse().unwrap());
        assert_eq!(
            guest_target(&sock("10.0.0.5", 80)),
            "10.0.0.5:80".parse().unwrap()
        );
    }
}
