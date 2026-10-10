// SPDX-License-Identifier: Apache-2.0
//! `Addresses`: the guest's interfaces and IPs, via `getifaddrs`.

use std::collections::BTreeMap;
use std::ffi::CStr;
use std::fs;
use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use vmuc_proto::{AgentResponse, Interface};

pub fn addresses() -> io::Result<AgentResponse> {
    let mut by_name: BTreeMap<String, Vec<IpAddr>> = BTreeMap::new();
    let mut head: *mut libc::ifaddrs = std::ptr::null_mut();
    // SAFETY: valid out-pointer; freed below with freeifaddrs.
    if unsafe { libc::getifaddrs(&raw mut head) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let mut cur = head;
    while !cur.is_null() {
        // SAFETY: `cur` is a node of the list getifaddrs returned.
        let ifa = unsafe { &*cur };
        // SAFETY: ifa_name is a NUL-terminated string owned by the list.
        let name = unsafe { CStr::from_ptr(ifa.ifa_name) }
            .to_string_lossy()
            .into_owned();
        if name != "lo" && !ifa.ifa_addr.is_null() {
            // SAFETY: ifa_addr points at a sockaddr whose family tells its size.
            let family = i32::from(unsafe { (*ifa.ifa_addr).sa_family });
            let ip = match family {
                libc::AF_INET => {
                    // SAFETY: family is AF_INET so this is a sockaddr_in.
                    let sin = unsafe { &*ifa.ifa_addr.cast::<libc::sockaddr_in>() };
                    Some(IpAddr::V4(Ipv4Addr::from(u32::from_be(
                        sin.sin_addr.s_addr,
                    ))))
                }
                libc::AF_INET6 => {
                    // SAFETY: family is AF_INET6 so this is a sockaddr_in6.
                    let sin6 = unsafe { &*ifa.ifa_addr.cast::<libc::sockaddr_in6>() };
                    let v6 = Ipv6Addr::from(sin6.sin6_addr.s6_addr);
                    // Link-local addresses are noise for the host.
                    (!v6.is_unicast_link_local()).then_some(IpAddr::V6(v6))
                }
                _ => None,
            };
            let entry = by_name.entry(name).or_default();
            if let Some(ip) = ip {
                entry.push(ip);
            }
        }
        cur = ifa.ifa_next;
    }
    // SAFETY: `head` came from getifaddrs and is freed exactly once.
    unsafe { libc::freeifaddrs(head) };

    let ifaces = by_name
        .into_iter()
        .map(|(name, addrs)| Interface {
            mac: fs::read_to_string(format!("/sys/class/net/{name}/address"))
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty() && s != "00:00:00:00:00:00"),
            name,
            addrs,
        })
        .collect();
    Ok(AgentResponse::Addresses(ifaces))
}

#[cfg(test)]
mod tests {
    #[test]
    fn addresses_do_not_include_loopback() {
        let super::AgentResponse::Addresses(v) = super::addresses().unwrap() else {
            panic!("expected addresses");
        };
        assert!(v.iter().all(|i| i.name != "lo"));
    }
}
