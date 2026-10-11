# vmuc

ARM Linux virtual machines on Apple silicon, from the command line.

vmuc is a Lima-style VM manager (create, start, shell, file sharing,
port forwarding) built on its own Rust VMM that talks to
Hypervisor.framework directly. What sets it apart is memory: the guest
sees a generous ceiling, the host pays only for pages the guest touches,
and memory the guest frees goes back to macOS within seconds.

Status: pre-alpha, milestone M5. Stock Ubuntu cloud images boot on
vmuc's own VMM and virtio devices, `vmuc start` / `vmuc shell` work end
to end over vsock (about 3 s from start to a shell on an M4 Pro), and
memory the guest frees is back in macOS within seconds (`vmuc stat`
shows it). Networking, file sharing and the TUI come next. See
`docs/plan.md` for the roadmap and `docs/feasibility.md` for why this
works.

```
vmuc create ubuntu        # download, verify and convert the cloud image
vmuc start ubuntu         # boot; returns when ssh works
vmuc shell ubuntu         # or: vmuc shell ubuntu -- uname -a
vmuc stat ubuntu          # host footprint, guest memory, bytes handed back
vmuc stop ubuntu
```

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
