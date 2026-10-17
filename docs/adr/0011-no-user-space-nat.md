# ADR 0011 — No user-space NAT

**Status:** accepted, 2026-10-17, at the user's request. It takes the
user-space NAT, planned for v0.2 as the sudo-free fallback, out of the
plan; the design study stays in
[feasibility.md](../feasibility.md#networking-without-vmnet).

## Context

The feasibility study planned a NAT inside the VMM (ARP, DHCP, DNS,
ICMP and UDP proxies, TCP terminated and spliced to host sockets) so
that vmuc would need no root. Before v0.1 the plan added a requirement
the NAT cannot meet: the host reaches the guest by IP. vmnet through the
root helper became the network, and the NAT moved to v0.2 as the
fallback for users who do not install the helper.

As a fallback it would give those users one thing they lack, outbound
network access from the guest. It would not give them the guest's
address on the host, guest-to-guest traffic or bridged mode, and
ArcBox's measurements of the same design (22.7 Gbit/s host to guest,
15 guest to host) are a fraction of vmnet's through the helper (87 and
136, [net-iperf](../benchmarks/net-iperf.md)). It is also a
TCP/IP stack's worth of code to own.

What a user needs without the helper already works over vsock: ssh,
`vmuc exec`, file sharing and port forwarding to `localhost`.

## Decision

- vmuc has one network, vmnet through `vmuc-vmnet-helper`. There is no
  user-space NAT, now or as a later fallback.
- Without the helper an instance boots without a NIC, as in v0.1.

## Consequences

- Guest internet access needs the helper and its sudoers rule, installed
  once (`vmuc sudoers`).
- `crates/vmuc-net` stays the helper, its rings and its launch.
- The forbidden-crates rule against a TCP/IP stack stays: dropping the
  NAT does not make one worth depending on.
