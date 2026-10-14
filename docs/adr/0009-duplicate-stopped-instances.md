# ADR 0009 — A stopped instance can be duplicated

**Status:** accepted, 2026-10-14, at the user's request. It takes clones
of stopped instances out of plan.md's "out of scope for v0.1"; named
snapshots and migration stay out.

## Context

A second machine like one already set up (a provisioned dev box, a few
identical test guests) should not need a second provisioning run. An
instance is three persistent files: `vmuc.yaml`, `vmuc-version` and a
sparse raw `disk.img` (up to the configured size, 100 GiB by default);
everything else in its directory belongs to one run. APFS clones a file
in constant time, the two sharing blocks until one of them writes.

Most of a guest's identity already follows the instance name: the MAC
address (`mac_address_for`, unless the instance pins one), and with it
the DHCP lease (`dhcp-identifier: mac`); the hostname (`vmuc-NAME`); the
SSH host keys, which cloud-init regenerates because every boot has a new
instance ID. The disk carries one more: `/etc/machine-id`. Measured on
Ubuntu 24.04 (2026-10-14): one boot with `systemd.machine_id=ID` on the
kernel command line writes ID to `/etc/machine-id` for good, and later
boots without it keep it.

## Decision

- `vmuc duplicate SOURCE NEW` (alias `clone`) takes `create`'s
  `--cpus`, `--memory`, `--disk`, `--mount` and `--set` for the copy,
  and `--start`. A disk can grow, not shrink.
- The source must be stopped. A running guest's disk is changing under
  the copy, and a suspended guest's disk lacks what its page cache held;
  that state belongs to the one instance that resumes it.
- The copy gets the source's configuration without an instance-level
  `network.macAddress`, its `vmuc-version`, and its disk cloned
  (`clonefile`; a sparse copy where the volume cannot clone). Nothing
  else: no logs, sockets, events or suspended state.
- The copy's first cold boot passes `systemd.machine_id=` with a new
  random ID; the VMM drops the request once the agent says hello.
- `vmuc resume NAME` names what `start` does for a suspended instance
  and refuses one that is not suspended. The TUI suspends (`z`),
  resumes (`s` on a suspended instance) and duplicates (`d`, asking for
  the new name).

## Consequences

- Duplicating takes well under a second and no space until the two
  disks diverge; each disk's allocation, as `vmuc list` and `du` report
  it, counts the shared blocks for both.
- Static port forwards to fixed host ports collide when both copies run,
  as they would for any two instances: the second's listener fails and
  says so.
- A guest without systemd keeps the source's machine ID.
