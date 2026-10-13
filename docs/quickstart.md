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
vmuc suspend ubuntu       # saves it; the next start resumes it (~0.2 s)
vmuc stop ubuntu          # --force if the guest does not power off
vmuc delete ubuntu        # --force stops it first
```

`create` takes `--cpus`, `--memory 8GiB`, `--disk 100GiB`,
`--mount DIR[:w]` and `--set path=value` for any other setting.
`vmuc template list` shows the templates (`ubuntu`, `alpine`, `docker`,
…). `vmuc start --foreground` keeps the VM attached to the terminal
with its log; Ctrl-C powers it off.

`vmuc suspend` keeps the guest's programs as they are and frees all of
its host memory and CPU: the state goes to `~/.vmuc/NAME/suspend/`
(about the memory the guest had in use), and `vmuc start` resumes from
it once, with the guest's files synced, its clock set and its network
and shares as before. `vmuc stop --force` on a suspended instance
discards the state instead, so the next start boots. Changing the CPUs,
memory, network or shares of a suspended instance keeps it from
resuming until the change is undone; shares with `dax` cannot be
suspended yet.

`vmuc shell` starts in the guest path of your current directory when it
is under a share (your home is, by default), else in the guest home;
`--workdir DIR` picks one. `vmuc completion zsh|bash|fish` prints a
completion script, and `vmuc prune` empties the image cache.

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

`vmuc copy` copies over ssh in either direction (`-r` for directories):

```
vmuc copy ./build.tar ubuntu:/tmp/
vmuc copy -r ubuntu:/var/log/apt ./apt-logs
```

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

`docs/config.md` lists every key with its default (generated from the
schema; `guest/templates/reference.yaml` is the same as commented YAML,
and `vmuc create template:reference` makes an instance from it). Change
an instance with `vmuc edit NAME`, or at creation with `vmuc create
--set path=value`. Settings layer: built-in defaults <
`~/.vmuc/_config/default.yaml` < the instance's `vmuc.yaml` <
`~/.vmuc/_config/override.yaml`.
