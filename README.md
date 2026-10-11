# vmuc

ARM Linux virtual machines on Apple silicon, from the command line.

vmuc is a Lima-style VM manager (create, start, shell, file sharing,
port forwarding) built on its own Rust VMM that talks to
Hypervisor.framework directly. What sets it apart is memory: the guest
sees a generous ceiling, the host pays only for pages the guest touches,
and memory the guest frees goes back to macOS within seconds.

Status: pre-alpha, milestone M2. The VMM boots the guest kernel to a
busybox shell on Hypervisor.framework (`vmuc _boot`); disks, networking
and the instance lifecycle come next. See `docs/plan.md` for the roadmap
and `docs/feasibility.md` for why this works.

## Requirements

- macOS 15 (Sequoia) or newer on Apple silicon.
- Nothing else: no Docker, no QEMU, no kernel extensions.

## Development

The primary dev box can be Linux; everything except the hypervisor calls
builds and tests there. See `AGENTS.md` for the workflow and
`docs/plan.md` for the layout.

```
make check        # fmt, clippy, tests, darwin cross-check
make kernel       # cross-build the guest kernel and boot it under QEMU (Linux)
make e2e          # boot real VMs (on an Apple silicon Mac)
make mac-test     # rsync to $VMUC_MAC and run the test suite there
```

## License

Apache-2.0. See `LICENSE` and `NOTICE`.
