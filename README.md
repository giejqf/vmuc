# vmuc

ARM Linux virtual machines on Apple silicon, from the command line.

vmuc creates and runs arm64 Linux VMs the way Lima does: stock cloud
images, cloud-init, ssh, file sharing and port forwarding. Underneath is
its own Rust VMM that drives Hypervisor.framework directly, and owning the
VMM is what lets it treat memory differently. The guest sees a generous
ceiling, the Mac pays only for the pages the guest is using, and memory
the guest frees goes back to macOS within seconds instead of staying with
the VM.

Measured on a Mac mini M4 Pro with an Ubuntu 24.04 guest, each number
with a rerunnable probe behind it ([benchmarks](docs/benchmarks/README.md)):

- `vmuc start` to an ssh prompt in 2.6 s (Lima on Virtualization.framework:
  7.7 s). A suspended instance is back in 0.13–0.17 s with its programs as
  they were.
- 99–100 % of 2 GiB the guest frees is back in macOS within 10 s, 90 % of
  it within about 2 s; under Virtualization.framework none of it is.
- An idle 4-vCPU guest costs 0.23–0.27 % of one core.
- 87 Gbit/s host to guest and 136 guest to host on one TCP flow, over
  macOS's NAT.
- Duplicating a stopped instance takes milliseconds: its disk is an APFS
  clone.

## Install

```
brew install giejqf/vmuc/vmuc
vmuc info                 # `supported: yes` and `ready: yes`
```

Guests reach the network through macOS's vmnet, which needs root (or an
entitlement Apple grants), so vmuc ships a small root helper that only
relays frames. `vmuc sudoers` prints how to install it and its sudoers
rule. Without it guests boot without a NIC, and ssh, file sharing and port
forwarding still work over vsock.

## Use

```
vmuc create ubuntu        # download, verify and convert the cloud image
                          # (--mount ~/src:w adds a writable share)
vmuc start ubuntu         # boot; returns when ssh works
vmuc shell ubuntu         # or: vmuc shell ubuntu -- uname -a
vmuc stat ubuntu          # host footprint, guest memory, bytes handed back
vmuc suspend ubuntu       # save it as it is; `vmuc resume ubuntu` brings it back
vmuc duplicate ubuntu dev # a second machine like it, sharing the disk copy-on-write
vmuc stop ubuntu
vmuc tui                  # every instance, live, with keys for all of the above
```

Your home directory is shared read-only into the guest by default, and
ports the guest listens on appear on the Mac's localhost.

Scripts and coding agents get a stable interface: `--json` on every
command, an exit code for each kind of failure, idempotent `start`, `stop`
and `suspend`, and `vmuc exec NAME -- PROGRAM ARGS…`, which runs a program
with its arguments exactly as given and exits with its status
([scripts and agents](docs/quickstart.md#scripts-and-agents)).

## Requirements

- An Apple silicon Mac with macOS 15 (Sequoia) or newer. vmuc is developed
  and measured on macOS 26; macOS 15 is supported by the code but not
  tested.
- Guests are arm64 Linux, booted with vmuc's own kernel and the image's
  root filesystem. Ubuntu 24.04 is what is tested; templates for Alpine
  and Docker (Ubuntu with Docker installed) ship as well (`vmuc template
  list`).
- Nothing else: no Docker, no QEMU, no kernel extensions.

## Status

0.1.0 is the first release. Not there yet: named snapshots. Not planned:
a guest network without the root helper, x86 guests, macOS guests, a
GUI.

## Documentation

- [Quick start](docs/quickstart.md): install, first instance, files,
  ports, memory, networking, configuration, scripts and agents.
- [Configuration reference](docs/config.md): every setting and its
  default.
- [Benchmarks](docs/benchmarks/README.md): the numbers against the targets
  and against Lima on Virtualization.framework.
- [Changelog](CHANGELOG.md).
- [How it is built](docs/README.md): the plan and its locked decisions,
  design records, experiments and logs.

## Development

The primary dev box can be Linux; everything except the hypervisor calls
builds and tests there. See [AGENTS.md](AGENTS.md) for the workflow and
[docs/plan.md](docs/plan.md) for the layout.

```
make check        # fmt, clippy, tests, darwin cross-check
make kernel       # cross-build the guest kernel and boot it under QEMU (Linux)
make e2e          # boot real VMs (on an Apple silicon Mac)
make mac-test     # rsync to $VMUC_MAC and run the test suite there
```

## License

Apache-2.0. See [LICENSE](LICENSE) and [NOTICE](NOTICE). The guest kernel
patches in `guest/kernel/patches/` are GPL-2.0-only, the license of the
code they change.
