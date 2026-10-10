// SPDX-License-Identifier: Apache-2.0
//! `SocketsWatch`: stream the set of listening TCP/UDP sockets.
//!
//! v0.1 reads `/proc/net/{tcp,tcp6,udp,udp6}` every 3 seconds and diffs.
//! The netlink `inet_diag` path and the eBPF `sys_exit_bind` tracepoint
//! that make this instant are M6 work; the wire format does not change.

use std::collections::HashSet;
use std::fs;
use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::thread;
use std::time::Duration;

use vmuc_proto::frame::write_frame;
use vmuc_proto::{AgentResponse, Proto, Socket, SocketEvent};

use crate::listen::Conn;

const TCP_LISTEN: &str = "0A";
/// UDP sockets have no listen state; an unconnected bound socket shows
/// state 07 (`TCP_CLOSE`) with a zero remote address.
const UDP_UNCONNECTED: &str = "07";

pub fn sockets_watch(mut conn: Box<dyn Conn>) -> io::Result<()> {
    let mut known: HashSet<Socket> = HashSet::new();
    loop {
        let now = snapshot();
        for s in now.difference(&known) {
            write_frame(
                &mut conn,
                &AgentResponse::SocketEvent(SocketEvent::Added(*s)),
            )?;
        }
        for s in known.difference(&now) {
            write_frame(
                &mut conn,
                &AgentResponse::SocketEvent(SocketEvent::Removed(*s)),
            )?;
        }
        known = now;
        thread::sleep(Duration::from_secs(3));
    }
}

fn snapshot() -> HashSet<Socket> {
    let mut set = HashSet::new();
    for (path, proto, v6) in [
        ("/proc/net/tcp", Proto::Tcp, false),
        ("/proc/net/tcp6", Proto::Tcp, true),
        ("/proc/net/udp", Proto::Udp, false),
        ("/proc/net/udp6", Proto::Udp, true),
    ] {
        let Ok(text) = fs::read_to_string(path) else {
            continue;
        };
        set.extend(parse_proc_net(&text, proto, v6));
    }
    set
}

fn parse_proc_net(text: &str, proto: Proto, v6: bool) -> Vec<Socket> {
    text.lines()
        .skip(1)
        .filter_map(|line| {
            let mut f = line.split_whitespace();
            let _sl = f.next()?;
            let local = f.next()?;
            let remote = f.next()?;
            let state = f.next()?;
            let wanted = match proto {
                Proto::Tcp => state == TCP_LISTEN,
                Proto::Udp => state == UDP_UNCONNECTED && remote_is_zero(remote),
            };
            if !wanted {
                return None;
            }
            let (ip_hex, port_hex) = local.split_once(':')?;
            let port = u16::from_str_radix(port_hex, 16).ok()?;
            let ip = parse_hex_ip(ip_hex, v6)?;
            Some(Socket { proto, ip, port })
        })
        .collect()
}

fn remote_is_zero(remote: &str) -> bool {
    remote
        .split_once(':')
        .is_some_and(|(ip, port)| ip.bytes().all(|b| b == b'0') && port == "0000")
}

/// `/proc/net` prints IPv4 as one little-endian u32 and IPv6 as four
/// little-endian u32 words.
fn parse_hex_ip(hex: &str, v6: bool) -> Option<IpAddr> {
    if v6 {
        if hex.len() != 32 {
            return None;
        }
        let mut bytes = [0u8; 16];
        for (i, chunk) in hex.as_bytes().chunks(8).enumerate() {
            let word = u32::from_str_radix(std::str::from_utf8(chunk).ok()?, 16).ok()?;
            bytes[i * 4..i * 4 + 4].copy_from_slice(&word.to_le_bytes());
        }
        let v6 = Ipv6Addr::from(bytes);
        // A v4-mapped listener on [::] shows up as v6; keep it as v6 so
        // the host knows it accepts both.
        Some(IpAddr::V6(v6))
    } else {
        let word = u32::from_str_radix(hex, 16).ok()?;
        Some(IpAddr::V4(Ipv4Addr::from(word.to_le_bytes())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TCP: &str = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n\
   0: 0100007F:0CEA 00000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0 1 0 0 100 0 0 10 0\n\
   1: 00000000:0016 00000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0 2 0 0 100 0 0 10 0\n\
   2: 0100007F:A3B2 0100007F:0016 01 00000000:00000000 00:00000000 00000000  1000        0 3 0 0 20 4 30 10 -1\n";

    #[test]
    fn tcp_listeners_only() {
        let v = parse_proc_net(TCP, Proto::Tcp, false);
        assert_eq!(v.len(), 2);
        assert!(v.contains(&Socket {
            proto: Proto::Tcp,
            ip: "127.0.0.1".parse().unwrap(),
            port: 3306
        }));
        assert!(v.contains(&Socket {
            proto: Proto::Tcp,
            ip: "0.0.0.0".parse().unwrap(),
            port: 22
        }));
    }

    #[test]
    fn v6_address_words_are_little_endian() {
        let ip = parse_hex_ip("00000000000000000000000001000000", true).unwrap();
        assert_eq!(ip, "::1".parse::<IpAddr>().unwrap());
        let ip = parse_hex_ip("0000000000000000FFFF00000100007F", true).unwrap();
        assert_eq!(ip, "::ffff:127.0.0.1".parse::<IpAddr>().unwrap());
    }

    #[test]
    fn udp_unconnected_bound_sockets() {
        let udp = "  sl  local_address rem_address   st ...\n\
   0: 00000000:0044 00000000:0000 07 00000000:00000000 00:00000000 00000000     0        0 1 0 0 0\n\
   1: 0100007F:1F90 0100007F:0035 01 00000000:00000000 00:00000000 00000000     0        0 2 0 0 0\n";
        let v = parse_proc_net(udp, Proto::Udp, false);
        assert_eq!(
            v,
            vec![Socket {
                proto: Proto::Udp,
                ip: "0.0.0.0".parse().unwrap(),
                port: 68
            }]
        );
    }

    #[test]
    fn live_snapshot_works() {
        let _ = snapshot();
    }
}
