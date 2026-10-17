# vmuc action plan

Companion to `feasibility.md`, which holds the evidence. This file holds
the decisions and the work. Every decision here is locked; change one by
editing this file in the same commit as the code that changes it.

## Product in one paragraph

`vmuc` launches ARM Linux virtual machines on Apple silicon Macs from the
command line, with automatic file sharing, port forwarding, and ssh, the
way Lima does, but on its own Rust VMM built directly on
Hypervisor.framework. Its distinguishing feature is memory that flows
both ways: the guest sees a generous ceiling, the host pays only for
pages the guest is actually using, and memory the guest frees is back in
macOS's hands within seconds. There is a TUI for watching and driving
instances, and nothing graphical beyond the terminal.

## Locked decisions

| Area | Decision | Why |
|------|----------|-----|
| Host floor | macOS 15.0+ (Sequoia), Apple silicon only | `hv_gic_*` in-kernel GICv3 is macOS 15+; it is the single biggest efficiency lever |
| Guest | arm64 Linux only, our own kernel, direct boot, no UEFI, no modules | Requirement 2; direct boot is fastest and simplest; built-in drivers mean one kernel fits every rootfs |
| Rootfs | Stock cloud images (Ubuntu 24.04 LTS first, then Alpine, Debian, Fedora) + cloud-init NoCloud `cidata.iso` | Lima's model; zero image maintenance for us |
| Language | Rust, edition 2024, latest stable toolchain, MSRV = stable minus two | No reason to lag |
| Hypervisor bindings | Hand-written `extern "C"` in `vmuc-hv`, no bindgen, no `hv`/`applevisor` crates | ~250 lines; type-checks on Linux without an SDK |
| Interrupts | Apple in-kernel GICv3 always on; no emulated GIC, no fallback | Floors macOS at 15; halves idle CPU and wake latency per ArcBox |
| Virtio | virtio-mmio transport, split queues, own device models: console, blk, net, vsock, fs, rng, balloon | PCI buys nothing without graphics or passthrough; mmio is what ArcBox, libkrun, Firecracker use |
| Memory | Ledger-tagged 128 KiB granules lazily populated, mapped once; free page reporting + unmap/fresh granules/remap release on a dedicated thread; host pressure responder that drops guest caches; no host-driven balloon targets | See `feasibility.md` requirement 3; plain anonymous memory is billed twice for pages host and guest share (ADR 0003); balloon targets never beat dropping caches (ADR 0004) |
| Guest page size | 4 KiB | 16 KiB costs 14–31 % more memory at idle for ~11 % more speed; compatibility was fine (E3) |
| Networking | Apple vmnet.framework through a small root helper (`vmuc-vmnet-helper`, one-time sudoers install like Lima's `socket_vmnet`): **shared** mode by default (guest gets a host-reachable 192.168.64.x address, macOS does DHCP and NAT), **bridged** mode optional (`network.mode: bridged`, `interface: en0`). No user-space NAT: without the helper an instance has no NIC, and ssh, file sharing and port forwarding still work over vsock (ADR 0011) | The host must reach the guest by IP (requirement added 2026-10-10); unprivileged vmnet needs the restricted `com.apple.vm.networking` entitlement, so the privileged part is isolated in a helper that only relays frames; a NAT would add outbound access only, slower, for a TCP/IP stack's worth of code |
| Port forwarding | Over vsock via the guest agent; dynamic from guest listening sockets plus static rules | Works without the helper; no ssh child processes |
| Shell | System `ssh` with `ProxyCommand vmuc _proxy <inst> 22` through a vsock tunnel; `ssh.config` written per instance | No host TCP port, works with any sshd, IDE-friendly |
| File sharing | Own virtiofs (FUSE-over-virtqueue passthrough); DAX per share, opt-in (`mounts[].dax`); host changes in writable shares replayed to guest watchers by the guest kernel | Only option without Apple devices; DAX takes shared-file reads out of the VM's memory but runs file faults on vCPUs (ADR 0005); virtiofs cannot report host changes, and a replay from inside the guest without the kernel gets only edits through (ADR 0006) |
| Process model | One binary `vmuc`; `vmuc start` spawns detached `vmuc _vmm <name>`; one VM per process | HV allows one VM per process; the entitled binary must own the VM |
| Control plane | JSON-lines over a unix socket per instance (`vmm.sock`); events appended to `events.jsonl`; status derived from PID liveness + boot-session marker | Lima's design; debuggable with `nc` and `tail` |
| Host↔agent | Length-prefixed JSON over vsock, one request type per connection; raw byte streams on dedicated vsock connections for tunnels | Simple; control traffic is tiny, bulk traffic bypasses it |
| Async | No async runtime inside the VMM. Dedicated OS threads per vCPU and per device, `mio` (kqueue) event loops for net and control | ArcBox's experience; fewer surprises on hot paths |
| Config | YAML, Lima-like schema, nullable fields, layering built-in < `~/.vmuc/_config/default.yaml` < instance < `override.yaml`; templates flattened at create | Proven |
| Formats | Disk is a raw sparse file; qcow2 read-converted at create time by our own reader (zlib clusters supported); discard punches holes with `F_PUNCHHOLE` | No qemu-img |
| TUI | ratatui + crossterm, `vmuc tui`, read-mostly with start/stop/shell actions | Requirement 1 |
| Allowed third-party crates | clap, clap_complete (shell completions, from clap's maintainers), serde/serde_yaml/serde_json, indexmap (ordered config maps), uuid (boot IDs), dirs (home and cache dirs), ratatui, crossterm, libc, thiserror, anyhow (CLI only), mio, flate2 (pure Rust), lzma-rs (pure Rust xz; C-based liblzma cannot cross-check for darwin without an SDK), sha2, hex, crc32fast, ureq (downloads; rustls on Linux, native-tls on macOS for the same SDK reason), tracing, `vm-fdt` (FDT writer), `virtio-bindings` (constants only), tempfile (dev only) | Non-VM concerns only |
| Forbidden crates | Anything implementing a VMM, hypervisor bindings, virtqueues, virtio devices, a virtiofs/FUSE server, a TCP/IP stack, or wrapping vz/QEMU/libkrun | Requirement 4 read strictly, and the memory manager must own guest memory |
| License | Apache-2.0 only; `NOTICE` carries attributions for code ported from ArcBox (MIT OR Apache-2.0) | Requirement |
| Distribution | Homebrew formula in our own tap, with bottles for macOS 15 and 26 that the tap's CI (`brew test-bot`) builds, signs ad-hoc with `vmuc.entitlements` and tests; kernel and agent as formula resources, inside the bottle | Lima and QEMU prove it; a bottle installs without Rust, LLVM or a build |
| Suspend | `vmuc suspend` writes a running instance to its directory and ends the VMM; `vmuc start` resumes it with guest RAM mapped copy-on-write from the image; one suspended state per instance, resumed at most once (ADR 0007) | Programs survive and a suspended instance costs no memory; a restore takes ~40 ms (E5) |
| Duplicate | `vmuc duplicate SOURCE NEW` copies a stopped instance: its configuration (without a pinned MAC), its disk cloned copy-on-write, and a new machine ID on the copy's first boot; `vmuc resume` names `start` for a suspended instance (ADR 0009) | A second machine like one already provisioned, in under a second and no space until the disks diverge |
| Out of scope for v0.1 | Containers runtime, Kubernetes, macOS guests, snapshots beyond suspend and duplicate (named snapshots, clones of running instances, migration), DAX by default, Intel Macs, Linux hosts, GUI | Scope control; templates can install Docker inside the guest |

## Repository layout

```
vmuc/
  AGENTS.md                  agent and contributor rules (loaded by Claude Code)
  Cargo.toml                 workspace
  LICENSE                    Apache-2.0
  NOTICE                     attributions
  vmuc.entitlements          com.apple.security.hypervisor + network
  Makefile                   check / test / mac-test / kernel / e2e / bench
  crates/
    vmuc-hv/                 Hypervisor.framework FFI + safe wrappers (vm, vcpu, exit decode, gic)
    vmuc-vmm/                guest memory + memory manager, vcpu threads, exit dispatch,
                             PSCI, FDT, kernel loader, PL011, PL031, MMIO bus, device manager
    vmuc-virtio/             virtqueue, mmio transport, devices/{console,blk,net,vsock,rng,balloon,fs}
    vmuc-fs/                 virtiofs server: FUSE protocol + passthrough backend
    vmuc-net/                NAT datapath, DHCP, DNS forwarder, port forwarder
    vmuc-image/              downloads + cache, sha256, qcow2 reader, GPT, ISO9660 writer, sparse files
    vmuc-config/             YAML schema, defaults layering, templates, validation
    vmuc-proto/              CLI↔VMM and host↔agent message types
    vmuc-agent/              guest agent (aarch64-unknown-linux-musl, static)
    vmuc-tui/                ratatui front end
    vmuc-cli/                the `vmuc` binary: CLI + hidden `_vmm`, `_proxy` roles
  guest/
    kernel/                  defconfig fragment, pinned version, build script, QEMU boot test
    templates/               ubuntu.yaml, alpine.yaml, debian.yaml, fedora.yaml, docker.yaml, _images/*.yaml
  tests/
    e2e/                     Mac-only: boot, ssh, mounts, ports, memory reclaim scenario
    bench/                   boot time, idle CPU, net, fs, memory probes (each rerunnable)
  docs/
    feasibility.md  plan.md  architecture/  adr/  experiments/  benchmarks/  logs/
  packaging/
    homebrew/vmuc.rb         formula (mirrored into the tap repo)
  .github/workflows/         linux.yml (check, clippy, test, cross-check darwin, kernel build + QEMU boot)
                             macos.yml (build, unit tests on GH runner; e2e on self-hosted)
```

Crates stay this coarse until a file needs a `// ====` section divider,
which is the signal to split it.

## Architecture

### Process and state

```
vmuc start NAME
  └─ spawns detached:  vmuc _vmm NAME          (one per instance; owns the HV VM)
       ├─ writes  ~/.vmuc/NAME/vmm.pid         (atomic rename; ignored if older than host boot)
       ├─ serves  ~/.vmuc/NAME/vmm.sock        (JSON-lines: status, stat, stop, forward, console)
       ├─ appends ~/.vmuc/NAME/events.jsonl    (booting / running / degraded / exiting, port events)
       ├─ logs    ~/.vmuc/NAME/vmm.log, console.log
       └─ threads: vcpu×N, blk, net-tx, net-rx/NAT, vsock, fs×mounts, reclaim, control
vmuc shell NAME   → ssh -F ~/.vmuc/NAME/ssh.config NAME   (ProxyCommand: vmuc _proxy NAME 22)
vmuc tui          → reads every instance's sock + events
```

Instance directory (`~/.vmuc/<name>/`): `vmuc.yaml`, `vmuc-version`,
`disk.img`, `cidata.iso`, `vmm.pid`, `vmm.sock`, `events.jsonl`,
`vmm.log`, `console.log`, `console.sock`, `ssh.config`, `boot-session`.
Global: `~/.vmuc/_config/{user,user.pub,default.yaml,override.yaml}`,
`~/.vmuc/_templates/`, cache in `~/Library/Caches/vmuc/`.

### Guest physical map (fixed, documented in the FDT generator)

| IPA | Contents |
|-----|----------|
| 0x0800_0000 | GICv3 distributor (64 KiB) |
| 0x0900_0000 | PL011 UART, SPI 1 |
| 0x0901_0000 | PL031 RTC, SPI 2 |
| 0x0A00_0000 | virtio-mmio slots, 32 × 4 KiB, SPI 32 + slot |
| 0x0C00_0000 | GICv3 redistributors (64 MiB window; the framework's region is 32 MiB on an M4 Pro, sized for every vCPU it supports, so it cannot sit at QEMU's 0x080A_0000 without covering the devices; see `docs/experiments/2026-10-11-hv-contract-m4-pro.md`) |
| 0x4000_0000 | RAM: kernel Image at +text_offset, initrd at max(kernel end, min(RAM/2, 128 MiB)) on a 2 MiB boundary, FDT in the last 2 MiB |

### Boot sequence

1. `vmuc-cli` resolves config, ensures kernel and image are cached and
   verified, converts qcow2 → raw sparse if needed, finds the root
   partition (GPT), writes `cidata.iso` with a fresh per-boot instance ID.
2. `_vmm` creates the VM (IPA 40-bit), maps RAM, creates the GIC, builds
   devices, writes kernel + FDT, creates N vCPUs on N threads (once),
   sets BSP PC/X0/CPSR, starts the BSP; secondaries park until PSCI
   CPU_ON.
3. Kernel cmdline: `console=hvc0 earlycon=pl011,0x09000000 root=PARTUUID=<root> rw panic=-1 ...` (PARTUUID so device order cannot matter; the kernel has no knob for the 2 s page-reporting delay).
4. Agent (installed by cloud-init from the cidata ISO on first boot, then
   a systemd unit) connects on vsock; host marks `running` when ssh
   probe over the tunnel succeeds and `/run/vmuc-boot-done` carries the
   boot ID.

### Memory manager (`vmuc-vmm/src/memory/`)

Components:

- `GuestRam`: one region reserved like `hv_vm_allocate` reserves it and
  backed by a ledger-tagged memory entry per 128 KiB granule
  (`vmuc_hv::guest_memory`), never pre-touched. Mapped once with
  `hv_vm_map` RWX. Host cost at boot is the kernel image plus whatever
  the guest touches, each page billed once (ADR 0003).
- `Reclaimer` (on the balloon device's own worker thread, never a vCPU):
  consumes ranges from the balloon device's reporting queue. For each:
  align inward to the 128 KiB granule; skip if nothing is resident
  (`mincore`); `hv_vm_unmap` → fresh granules at the same host address →
  `hv_vm_map` RWX → ack. A remap failure is
  fatal for the instance (the guest would have a hole in RAM); the
  error path tries once more, then stops the VM with a clear message.
  Batches consecutive descriptors into one unmap/map when contiguous.
- `Accounting`: per-instance counters (bytes reported, bytes released,
  release latency histogram, host `phys_footprint` via `proc_pid_rusage`,
  guest `MemTotal`/`MemAvailable` via the agent), exposed on `vmm.sock`
  for `vmuc stat` and the TUI.
- `PressureResponder`: reads macOS memory pressure (the
  `kern.memorystatus_vm_pressure_level` sysctl, every 5 s; `vmuc
  _pressure` simulates a level for experiments). On *warn* or
  *critical*: ask the agent to `sync; echo 3 > drop_caches; echo 1 >
  compact_memory` at most once per 30 s. Nothing else: E1 found that a
  balloon target never returned more than that and pinned memory the
  guest needed, so there is none (ADR 0004,
  `docs/experiments/2026-10-13-e1-pressure-policy.md`).
- Guest side: kernel with `VIRTIO_BALLOON=y`, `PAGE_REPORTING=y`,
  `TRANSPARENT_HUGEPAGE=y`, `INIT_ON_FREE_DEFAULT_ON=n`,
  `PAGE_POISONING=n`; the command line sets
  `page_reporting.page_reporting_order=5` (128 KiB) instead of the 2 MiB
  pageblock default: the bulk path is as fast and an idle guest hands
  back ~220 MiB more (`docs/experiments/2026-10-11-reporting-order.md`),
  and `swiotlb=noforce` (no device bounces, so the 64 MiB buffer is
  waste; `docs/experiments/2026-10-12-idle-footprint.md`). The kernel
  reports its whole free list on every pass instead of a sixteenth
  (ADR 0002), so freed memory is back in about 2 s instead of 7–10.
  Agent exposes `meminfo`, `psi`, `drop_caches`, `compact` RPCs.

Invariants, also in AGENTS.md: only the unmap/refresh/remap sequence
returns memory; `madvise` never does; never unmap a range smaller than a
host page; never map a different host address into a live IPA range;
reclaim never runs on a vCPU thread.

### Networking (`vmuc-net`)

Two processes share the work:

- `vmuc-vmnet-helper` (root, tiny, audited): opens one vmnet interface
  per instance with `vmnet_start_interface` in `VMNET_SHARED_MODE`
  (default) or `VMNET_BRIDGED_MODE` with the configured host interface,
  with TSO and virtio-net headers on macOS 15.4+, and relays Ethernet
  frames between the interface and the VMM through two shared-memory
  rings whose fd arrives over a `SOCK_DGRAM` unix socketpair (which also
  carries wake-ups; ADR 0001). Older helpers relay over the socketpair
  itself. Frames are opaque to it; it bounds-checks the ring indices and
  record headers it reads. It is started with `sudo` through a sudoers
  rule that `vmuc sudoers` prints (Lima's `limactl sudoers` pattern) and
  exits when the VMM goes. It never touches guest memory or the VM.
- The VMM's virtio-net device (MRG_RXBUF; checksum offload and TSO when
  the helper speaks virtio-net headers) moves frames between the
  virtqueues and the rings on a dedicated thread.

In shared mode macOS runs DHCP and NAT: the guest gets a `192.168.64.0/24`
address reachable from the host, so `ssh user@<guest-ip>` works without
vmuc in the path, and `host.vmuc.internal` resolves to the host's
address on that bridge. The agent reports the guest's addresses so
`vmuc list` and `ssh.config` can show them. In bridged mode the guest
takes a lease from the LAN's DHCP and is reachable from other machines.

Port forwarding to `127.0.0.1` stays, for callers that expect
`localhost:<port>`: the agent streams listening-socket add/remove events
(netlink `inet_diag`, polled every 3 s plus an eBPF `sys_exit_bind`
tracepoint where available). The host applies `port_forwards` rules
(Lima's rule shape: guest ip/port/range, host ip/port/range, proto,
ignore, static) and opens loopback listeners; each accepted connection
opens a vsock connection to the agent, which dials the guest socket.

There is no user-space NAT (ADR 0011). The one studied in
feasibility.md would have been a sudo-free fallback with outbound
access only.

### Control and agent protocol (`vmuc-proto`)

`vmm.sock`: newline-delimited JSON `{ "id", "method", "params" }` /
`{ "id", "result" | "error" }`. Methods: `status`, `stat`, `stop {force}`,
`forwards`, `console {follow}`, `reclaim_now`, `simulate_pressure`.
Agent on vsock port 7700: same framing; one request per connection;
methods: `hello`, `meminfo`, `psi`, `drop_caches`, `compact`,
`sockets.watch` (streams events), `tunnel {addr}` (then raw bytes),
`exec` (for probes), `time.sync`, `fs_events {tag, events}` (host
changes in a writable share, replayed for the guest's file watchers;
ADR 0006).

### CLI surface (v0.1)

```
vmuc [--json] COMMAND            results as JSON; exit codes by kind of failure (ADR 0010)
vmuc create   [TEMPLATE|FILE|URL] [--name] [--cpus] [--memory] [--disk] [--mount] [--set yq-like]
vmuc start    NAME [--timeout] [--foreground]   (waits until ready; running already is fine)
vmuc stop     NAME [--force]     (--force also discards a suspended state)
vmuc suspend  NAME               save the running instance; `start` or `resume` resumes it
vmuc resume   NAME [--timeout]   resume a suspended instance (refuses any other)
vmuc duplicate SOURCE NEW [--cpus] [--memory] [--disk] [--mount] [--set] [--start]
                                 copy a stopped instance (ADR 0009)
vmuc restart  NAME
vmuc shell    NAME [-- CMD...]   [--workdir]
vmuc exec     NAME [--workdir] [--tty] -- PROGRAM ARGS...   arguments as given
vmuc copy     SRC DST            (scp through the tunnel)
vmuc list     [NAME...] [--format table|json] [-q]
vmuc stat     NAME [--watch]     memory in/out, footprint, reclaim latency, cpu, net
vmuc delete   NAME [--force]
vmuc edit     NAME [--set PATH=VALUE] [--cpus] [--memory] [--mount]   (no flags: $EDITOR)
vmuc info                        versions, kernel, templates, host checks (hv support, macOS version)
vmuc template list|show NAME
vmuc prune                       cache
vmuc tui
vmuc completion SHELL
vmuc _vmm NAME        hidden
vmuc _proxy NAME PORT hidden
```

### Guest kernel (`guest/kernel`)

Linux LTS current at project start (6.12 or newer LTS), as few patches
as possible (`guest/kernel/patches/`, each with its ADR), arm64 defconfig
reduced then extended: virtio-mmio, virtio blk/net/console/vsock/fs/rng/
balloon, page reporting, ext4/btrfs/xfs/erofs/overlayfs/fuse, netfilter
and bridge built-in (for Docker templates), cgroup v2, PSI, no modules,
no graphics, no sound, no USB, HZ=250, NO_HZ_IDLE, 4 KiB pages,
`CONFIG_ARM64_VA_BITS_48`. Built by `make kernel` on Linux with
`aarch64-linux-gnu-` cross tools, published as a GitHub release asset
`vmuc-kernel-<ver>-arm64.img` with sha256 in `vmuc-image/src/assets.rs`.

## Milestones

Sizes are engineering-weeks for one engineer working with coding agents.
"Linux" means the x86 dev box suffices; "Mac" means an Apple silicon
machine is required.

### M0 — Bootstrap (1 week, Linux + a few hours on a Mac)

- Workspace, LICENSE, NOTICE, AGENTS.md, Makefile, `rust-toolchain.toml`,
  `.cargo/config.toml` with `aarch64-apple-darwin` and
  `aarch64-unknown-linux-musl` targets.
- `vmuc-hv` FFI surface ported from ArcBox's `ffi.rs` (attributed),
  `cfg`-gated so the workspace builds on Linux as empty stubs.
- CI: `linux.yml` runs fmt, clippy, tests, `cargo check --target aarch64-apple-darwin`;
  `macos.yml` builds and unit-tests on `macos-15` and `macos-26`.
- Packaging smoke: formula builds locally, `codesign -d --entitlements`
  shows the hypervisor key, a bottled copy keeps it.
- Secure the Mac (ssh-reachable, macOS 15+, Xcode CLT, 16 GB+).
- Exit: green CI on both OSes; `vmuc info` prints host checks.

### M1 — Guest kernel and QEMU harness (1–2 weeks, Linux)

- Kernel config and build script; QEMU `-M virt` boot test with
  virtio-mmio blk/net/console/vsock/rng/balloon(`free-page-reporting=on`).
- `vmuc-image`: downloader + cache + sha256, qcow2 → raw, GPT root
  partition lookup, ISO9660 writer, sparse-file helpers.
- `vmuc-config`: schema, layering, templates (`ubuntu`, `alpine`).
- cidata contents (Lima's layout, reduced): `user-data`, `meta-data`,
  `network-config`, `boot.sh`, agent binary, per-boot ID.
- `vmuc-agent` skeleton: hello, meminfo, exec, sockets.watch, tunnel.
- Exit: Ubuntu 24.04 cloud image boots under QEMU with our kernel, user
  logs in over ssh through the agent tunnel, free page reporting is
  visible in QEMU's balloon stats.

### M2 — First light on Hypervisor.framework (2–3 weeks, Mac)

- `vmuc-vmm`: guest RAM, vCPU threads, exit dispatch (data abort with
  PC+4, HVC/SMC without, sysreg RAZ/WI, WFI, vtimer, cancel), PSCI,
  GIC creation and SPI wiring, FDT, kernel loader, PL011, PL031.
- Boot the M1 kernel with a busybox initramfs to a serial shell.
- SMP: 4 vCPUs through PSCI CPU_ON.
- Teardown contract and `hv_vcpus_exit` based stop.
- Benchmark: `bench/boot-serial` (power-on to `init` prompt). Target < 400 ms.
- Exit: `vmuc _vmm` boots to a shell on the console; reboot and poweroff work.

### M3 — Virtio core and storage (2 weeks, Linux first, Mac to verify)

- Split virtqueue with batching and EVENT_IDX, mmio transport,
  device manager, IRQ assertion through the GIC.
- Devices: console (hvc0 + console.sock), blk (multi-queue, discard →
  punch hole, flush), rng.
- Exit: Ubuntu cloud image boots to login on HV; `bench/boot-ssh`
  baseline recorded.

### M4 — vsock, agent, lifecycle CLI (2 weeks, Mac)

- virtio-vsock device + host side (connect to guest ports, accept from guest).
- `_vmm` control socket, events file, PID/boot-session logic.
- CLI: create/start/stop/restart/list/delete/shell/copy/info/edit,
  `ssh.config` + `_proxy`, readiness probes.
- Exit: `vmuc start ubuntu && vmuc shell ubuntu` works end to end;
  `bench/boot-ssh` target < 3 s from `start` to prompt.

### M5 — Memory manager (2 weeks, Mac)

- Balloon device: F_REPORTING, F_STATS_VQ (F_DEFLATE_ON_OOM until
  ADR 0004).
- Reclaimer thread with the unmap/refresh/remap sequence, accounting,
  `vmuc stat`.
- Pressure responder (warn → drop/compact), agent `drop_caches`,
  `compact`, `psi`.
- Optional balloon target with the hysteresis rules; default off
  (retired by ADR 0004).
- Probes: `bench/mem-reclaim` reproduces lima#2789 (allocate N GiB in
  tmpfs, free, watch host footprint); `bench/stage2-matrix` reproduces
  ArcBox's madvise matrix so a macOS update cannot silently break us.
- Exit targets: ≥ 90% of freed memory back within 10 s; idle Ubuntu
  guest footprint < 350 MiB after first reclaim; no reclaim-induced
  stall > 5 ms on any vCPU.

### M6 — Networking (2–3 weeks, Mac)

- `vmuc-vmnet-helper`: vmnet FFI (`vmnet_start_interface`,
  `vmnet_read`/`vmnet_write`, interface parameters), shared and bridged
  modes, socketpair relay, `vmuc sudoers`.
- virtio-net device wired to the relay socket; MAC from config or
  derived from the instance name.
- Agent reports addresses; `vmuc list` shows the guest IP; `ssh.config`
  gets a second `Host NAME-ip` entry using it.
- Port forwarder: rules, dynamic events from the agent, static rules,
  loopback listeners, vsock tunnels.
- `bench/net-iperf`: targets > 10 Gbps single-flow each direction
  (Lima measures VZ NAT at 59 Gbit/s, so vmnet has headroom),
  < 0.3 ms vsock RPC p50.
- Exit: `curl` from the guest works; `ssh user@<guest-ip>` from the host
  works; `python -m http.server` in the guest is reachable on
  `localhost` automatically.

### M7 — virtiofs (3–4 weeks, Linux first via /dev/fuse, then Mac)

- FUSE 7.3x protocol server, passthrough backend with inode table,
  handles, xattr, O_DIRECT/flush semantics, errno translation, uid/gid
  mapping policy. Test on Linux by mounting through `/dev/fuse`.
- virtio-fs device, one per mount, multi-queue; fstab entries via cidata.
- `bench/fs`: fio and `git status` on a large tree; target ≥ VZ
  virtiofs on the same host (measure Lima vz as the reference).
- Exit: `~` shared read-only by default, writable mounts work, builds
  run from a shared tree.

### M8 — TUI (1–2 weeks, Linux for layout, Mac for live data)

- Instance list with status, vCPU load, guest used/available, host
  footprint, reclaim sparkline, forwarded ports; console tail pane;
  start/stop/shell key bindings. Reads only from sockets and files; no
  privileged state.

### M9 — Release engineering (1–2 weeks)

- Tap repository and formula, kernel resource, bottle build on a
  self-hosted runner, `vmuc info` self-check. *Decided 2026-10-12 that the
  Homebrew release would wait for 0.2.0; on 2026-10-16 the user chose to
  release 0.1.0 to the tap (`giejqf/homebrew-vmuc`) after all.*
- Release workflow: tag → Linux job builds kernel + agent assets → Mac
  job builds, signs, tests → GitHub release draft stays draft until
  assets exist (ArcBox's ordering lesson).
- Docs: README, quick start, config reference generated from the
  schema, benchmarks page with the numbers above.
- v0.1.0.

Total: roughly 18–24 engineer-weeks to v0.1.0.

### Experiments queued after v0.1

- E1 Balloon target policy tuning with real workloads: done 2026-10-13
  (`docs/experiments/2026-10-13-e1-pressure-policy.md`); balloon targets
  retired (ADR 0004), and found on the way: double-billed guest RAM
  (ADR 0003).
- E2 virtiofs DAX via per-file `hv_vm_map` (respect the no-overlap rule):
  done 2026-10-13 (`docs/experiments/2026-10-13-e2-virtiofs-dax.md`); DAX
  lands opt-in per share (ADR 0005).
- E3 16 KiB guest pages: fewer stage-2 entries, 32 MiB reporting
  granularity, userspace compatibility survey: done 2026-10-13
  (`docs/experiments/2026-10-13-e3-16k-pages.md`); 11 % faster under a
  build, nothing broke, but 14–31 % more memory at idle, so 4 KiB stays.
- E4 inotify propagation for writable mounts: done 2026-10-13
  (`docs/experiments/2026-10-13-e4-inotify-propagation.md`); host changes
  reach guest watchers as local ones do, replayed by guest kernel patch
  0002 from the VMM's FSEvents stream (ADR 0006).
- E5 Snapshot/restore: done 2026-10-13
  (`docs/experiments/2026-10-13-e5-snapshot-restore.md`); vCPUs, the
  GIC and the timers restore exactly through the macOS 15 API, a restore
  with guest RAM mapped copy-on-write takes ~40 ms and costs only what
  the guest then writes, and reclaim needs no exception for it (unlike
  mvm). Prototype in `vmuc _boot`; instances suspend and resume with it
  (ADR 0007).
- E6 APFS sparse accounting: done 2026-10-13
  (`docs/experiments/2026-10-13-e6-apfs-sparse-files.md`); APFS really
  allocates holes of up to 16 MiB past a file's allocated end when it
  flushes, and keeps holes punched afterwards, so every writer that
  leaves holes punches them once flushed; the allocation assertions run
  on macOS too.

## Development workflow

Daily loop on the Linux box:

```
make check        # fmt, clippy -D warnings, test, cargo check --target aarch64-apple-darwin
make kernel       # cross-build guest kernel (cached), run QEMU boot test
make agent        # aarch64-unknown-linux-musl static build
make mac-test     # rsync tree to $VMUC_MAC, cargo test there, stream output back
make mac-e2e      # same, runs tests/e2e (needs HV)
make bench NAME=  # runs tests/bench/NAME on the Mac, appends a dated row to docs/benchmarks/NAME.md
```

Rules of thumb:

- Anything that does not call `hv_*` gets a unit test that runs on
  Linux. Device models take `&mut [u8]` guest memory and a queue, never
  a VM handle.
- `vmuc-hv` compiles to stubs off macOS so clippy covers call sites.
- Every HV-specific change runs `make mac-e2e` before it lands.
- Every number lands in `docs/benchmarks/` or `docs/experiments/` in the
  same commit series, with the probe checked in under `tests/bench/`.

## Immediate next steps (this week)

1. Confirm the handful of judgment calls listed in the chat summary
   (macOS 15 floor, `vm-fdt`/`virtio-bindings` allowance, tap name and
   GitHub org, Mac access plan).
2. `git init`, M0 scaffolding, first CI run.
3. Order or provision the Mac; M1 proceeds on Linux meanwhile.
