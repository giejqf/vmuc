# Quick start

vmuc runs arm64 Linux VMs on Apple silicon Macs (macOS 15 or newer).

## Install

From a release, once one is published:

```
brew install giejqf/vmuc/vmuc
```

From source (Rust stable, with the `aarch64-unknown-linux-musl` target
for the guest agent):

```
rustup target add aarch64-unknown-linux-musl
make agent                                   # static guest agent
# guest kernel: download the `kernel` workflow artifact into guest/kernel/out/
cargo build --release -p vmuc-cli
make sign                                    # ad-hoc sign with the hypervisor entitlement
```

Then check the host and the install:

```
vmuc info
```

`supported: yes` means this Mac can run VMs; `ready: yes` means the
guest kernel and agent were found.

## First instance

```
vmuc create ubuntu        # downloads and verifies the cloud image once
vmuc start ubuntu         # boots; returns when ssh works (~3 s)
vmuc shell ubuntu         # or: vmuc shell ubuntu -- uname -a
vmuc list
vmuc stop ubuntu          # --force if the guest does not power off
vmuc delete ubuntu
```

`create` takes `--cpus`, `--memory 8GiB`, `--disk 100GiB` and
`--mount DIR[:w]`. `vmuc template list` shows the templates (`ubuntu`,
`alpine`, `docker`, …).

## Files

Your home directory is shared read-only at the same path inside the
guest, over vmuc's own virtiofs. Add writable shares with
`--mount ~/src:w` at create time, or in the instance's `vmuc.yaml`:

```yaml
mounts:
- location: ~/src
  writable: true
  mountPoint: /src        # optional; defaults to the same path
```

macOS asks for permission the first time the guest reads a protected
folder (Desktop, Documents, Downloads), and the guest waits until you
answer.

## Ports

A TCP port a guest program listens on (1024 and up, not 22) appears on
the host's `localhost` within a couple of seconds, through vsock; no
guest network is needed. `portForwards` in `vmuc.yaml` changes the rules.

## Memory

The guest sees its configured memory, but the Mac pays only for what it
touches, and memory the guest frees goes back to macOS within seconds.

```
vmuc stat ubuntu          # host footprint, guest memory, bytes handed back
vmuc stat -w ubuntu       # refresh every second
vmuc reclaim ubuntu       # ask the guest to drop caches now
```

## Watching everything

```
vmuc tui
```

↑/↓ select, `s` start, `x` stop, `X` force stop, Enter shell, `r`
reclaim, `c` console pane, `q` quit.

## Guest networking

ssh, file sharing and port forwarding all work without a guest network.
For the guest to reach the internet through macOS's vmnet, install the
small root helper and its sudoers rule; `vmuc sudoers` prints the
commands. Without them instances boot without a NIC and `vmuc start`
says so.

## Configuration

`guest/templates/reference.yaml` (or `vmuc create template:reference`)
lists every key with its default and explains how the built-in
defaults, `~/.vmuc/_config/default.yaml`, the instance's `vmuc.yaml` and
`~/.vmuc/_config/override.yaml` layer.
