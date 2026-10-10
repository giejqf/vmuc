# vmuc feasibility study

Date: 2026-10-10. Written before any code exists. Every claim below cites
where it comes from: the ArcBox tree (`../arcbox`, MIT OR Apache-2.0), the
Lima tree (`../lima`, Apache-2.0), or a public source.

## Verdict

**Feasible, with one hard platform floor: macOS 15 (Sequoia) or newer on
Apple silicon.** All six requirements are satisfiable, and the one that
makes the project distinctive, returning guest memory to the host, is
*only* satisfiable the way the requirements already demand: with a custom
VMM on Hypervisor.framework. Virtualization.framework cannot do it at
all. The remaining risk is engineering volume, not unknown physics.

| # | Requirement | Verdict | Evidence |
|---|-------------|---------|----------|
| 1 | Rust, CLI-first, TUI, no GUI | Routine | clap + ratatui; Lima's CLI is the model |
| 2 | ARM macOS host, ARM Linux guest only | Simplifies everything | No cross-arch, no x86 paths, no UEFI needed |
| 3 | Custom memory manager, guest frees memory to host | **Proven on HV** | ArcBox `Stage2Refresh` (measured 5.0 GB → 1.95 GB in 10 s); mvm PR #3478 (1.6 GB → 78 MB in 10.9 s) |
| 4 | Hypervisor.framework only, no vz/qemu | Proven | ArcBox HV backend, libkrun, mvm all boot Linux this way |
| 5 | No graphics | Simplifies | Serial console + virtio only |
| 6 | Performance and efficiency | Achievable | ArcBox HV: 22.7 Gbps net, SPI-only wake; targets in plan |

Release as a Homebrew formula under Apache-2.0: feasible. The
`com.apple.security.hypervisor` entitlement works with an ad-hoc
signature, which a formula can apply at install time (QEMU and Lima do
exactly this).

## Requirement 3 in depth: giving memory back

### Why Lima cannot (issue lima-vm/lima#2789)

The issue reports that a VZ VM that peaks at 8 GB stays at 8 GB on the
host until restart. Lima attaches a `VZVirtioTraditionalMemoryBalloonDevice`
but never drives it, and an instrumented test on macOS 26 (1 GB VM,
256 MB allocated then freed, caches dropped, memory compacted) showed
host RSS **never decreasing**: "The guest cooperated fully. The host
process ignored it." A Lima maintainer called it a macOS bug that
Docker Desktop shares.

ArcBox's measurements agree and explain it: on VZ a 15.35 GB balloon
inflation left the daemon's `phys_footprint` byte-identical, and under
host pressure macOS *compressed* the ballooned pages as if live. Guest
RAM lives in Apple's XPC helper process, so the embedding process has no
handle on it. (`../arcbox/virt/arcbox-vmm/AGENTS.md`, "Releasing guest
RAM".)

OrbStack's "dynamic memory" is the proof that it can be done, and
OrbStack runs its own hypervisor, not VZ.

### What works on Hypervisor.framework

Two independent implementations converged on the same sequence:

1. Guest kernel runs **virtio-balloon free page reporting**
   (`CONFIG_VIRTIO_BALLOON`, `CONFIG_PAGE_REPORTING`). Linux reports
   free runs at `page_reporting_order` granularity, which defaults to
   the pageblock order: **2 MiB on a 4 KiB-page arm64 guest**, about two
   seconds after the pages become free (`page_reporting.page_reporting_delay_ms`,
   default 2000).
2. For each reported range the host does, in order:
   `hv_vm_unmap(gpa, len)` → `mmap(host_addr, len, MAP_FIXED|MAP_ANON|MAP_PRIVATE)`
   → `hv_vm_map(host_addr, gpa, len, RWX)` → acknowledge the report.
3. Cost: about 40 µs per 2 MiB (ArcBox). Footprint and resident size
   drop together; a later guest write re-faults and is billed honestly.

Measured results:

| Source | Scenario | Before | Peak | After release |
|--------|----------|--------|------|---------------|
| ArcBox, 16 GB HV VM, 3 GiB tmpfs held then freed | daemon `phys_footprint` | 1898 MB | 5008 MB | 1951 MB after 10 s |
| mvm PR #3478, 2 GB guest, 2×768 MiB tmpfs freed | supervisor footprint | 1610 MiB | — | 78 MiB, first 1 GiB back in 10.9 s |

What does **not** work, measured by ArcBox on macOS 26.4 with a probe that
dirties RAM from a real vCPU (`stage2_matrix`, 2026-09-25):

- `madvise(MADV_DONTNEED)`: a deactivation hint on Darwin, footprint
  unchanged.
- `madvise(MADV_FREE_REUSABLE)`: footprint unchanged, "re-billed nothing
  and hid a 768 MB resident set". mvm found the same: after refill the
  footprint read 512 MiB against 1621 MiB actually resident.
- `hv_vm_unmap` alone: pages stay pinned.
- A host-side `memset` calibration *appears* to show `MADV_FREE_REUSABLE`
  working and is the wrong experiment: pages dirtied through the stage-2
  mapping are pinned by the hypervisor until the mapping owner replaces
  the backing.

Constraints that shape the design:

- Host pages are 16 KiB. Reported ranges must be aligned **inward** to
  16 KiB; XNU rounds a misaligned range outward and `hv_vm_unmap` refuses
  sub-page ranges, either of which would discard a neighbour's data.
  Anything smaller than a host page stays billed. Balloon *inflation*
  hands over 4 KiB PFNs one at a time and therefore reclaims only
  contiguous runs; **reporting, not inflation, is the bulk path.**
- `hv_vm_unmap`/`hv_vm_map` on a sub-range of a live mapping is safe only
  when the *same* host address is mapped back. Apple silicon refuses
  overlapping remaps of a different host range (matters later for DAX).
- The guest promises not to touch a page while it is being released
  (virtio spec §5.5.1 reporting semantics). If it does, it takes a
  stage-2 fault into the MMIO path and reads zero, which is the correct
  value for a reported-free page.
- Host-driven balloon *targets* are a second mechanism with real
  failure modes. ArcBox's engine kept the policy dormant after two
  incidents: a "used + 256 MiB headroom" rule collapsed an idle guest to
  its floor (MemAvailable 0 for ~98 s every ~8.5 min, ~2.8 cores burned),
  and on a pressured host it oscillated every 5 min. A blind shrink to
  128 MB once starved a compose stack for 18 h. Reporting needs no host
  target; targets are an optional, conservative layer.

Conclusion: requirement 3 is met by (a) free page reporting with the
unmap/refresh/remap release, always on, plus (b) a small host-pressure
responder that asks the guest to drop caches and compact, plus (c) an
optional, off-by-default balloon target with strong hysteresis. The
"custom memory manager" is the host side of (a)–(c) and the accounting
around it. Design details are in `plan.md`.

## Requirement 4: Hypervisor.framework without vz or QEMU

What a Linux guest needs from the host, and where each piece comes from:

| Piece | Approach | Reference |
|-------|----------|-----------|
| VM, vCPUs, memory, exits | `hv_vm_create`, `hv_vcpu_create/run`, `hv_vm_map`; hand-written `extern "C"` bindings (~230 lines in ArcBox, no bindgen) | `../arcbox/virt/arcbox-hv/src/ffi.rs` |
| Interrupt controller | Apple's in-kernel GICv3 via `hv_gic_*` (macOS 15+). Devices assert SPIs with `hv_gic_set_spi`; with the in-kernel GIC, WFI is handled inside `hv_vcpu_run`, so asserting the SPI *is* the wake, no `hv_vcpus_exit` kick | ArcBox ADR 0001; removing kicks took vsock RPC p50 0.81 → 0.30 ms and idle CPU 9.3% → 6.7% |
| Timer | Architected vtimer; FDT advertises `arm,armv8-timer` PPIs 13/14/11/10 | ArcBox `fdt.rs` |
| Boot | Direct kernel boot: load `Image` (arm64 Linux boot protocol), generate a device tree, set PC/X0, no UEFI, no bootloader | ArcBox `setup.rs`; libkrun; Firecracker |
| Console | PL011 UART emulation for early boot plus virtio-console | ArcBox `pl011.rs` |
| RTC | PL031 emulation (needed or the guest clock starts at 1970) | ArcBox `pl031.rs` |
| Devices | virtio-mmio transport, split virtqueues, our own device models | ArcBox `arcbox-virtio-core`; `virtio-bindings` for constants only |
| PSCI | HVC-based PSCI 1.0 subset: VERSION, FEATURES, CPU_ON, CPU_OFF, AFFINITY_INFO, SYSTEM_OFF/RESET | ArcBox `psci.rs` |

Known sharp edges collected from ArcBox (all go into AGENTS.md):

- HV does **not** advance PC on data aborts; the VMM adds 4. It **does**
  advance PC on HVC/SMC. Getting this wrong wedges boot silently.
- Unknown system-register traps (EC 0x18) must be read-as-zero /
  write-ignored with PC += 4, or Linux hangs on an `OSDLR_EL1` write.
- Create each vCPU once and reuse it across PSCI CPU_OFF/CPU_ON.
  Re-creating deadlocks on `hv_vcpu_create`'s global lock.
- `HvVcpu` is `!Send`: one OS thread per vCPU, created and destroyed on
  that thread.
- Teardown order is a contract: join vCPUs → join workers holding guest
  memory → GIC → `hv_vm_destroy` → unmap guest memory.
- One VM per process. Each instance is its own process.
- Without the in-kernel GIC the guest dies in arch_timer/GIC probe. An
  emulated GIC (libkrun's approach) is possible on macOS 11–14 but costs
  a user-space exit per interrupt. We take the macOS 15 floor instead.

macOS version: `hv_gic_*` is macOS 15+. ArcBox builds with the Xcode 26
SDK; mvm requires macOS 26 for its HVF backend. We target **macOS 15+**
at runtime and test on both 15 and 26.

## Requirement 1 and 6: process model, CLI, efficiency

Lima's shape transfers directly and is the right one for CLI-first:

- A single binary with hidden roles. `limactl` is the CLI, the per-instance
  `hostagent` daemon, and a driver server. For vmuc the hidden role is
  the VMM process itself, since HV requires the entitled process to own
  the VM.
- Per-instance state on disk under `~/.vmuc/<name>/`, status *derived*
  from PID-file liveness plus a host boot-session marker (stale PIDs
  after reboot are ignored), events as a JSON-lines file any CLI
  invocation can tail, a unix socket only for live queries.
- Config as layered YAML with nullable fields (built-in < user default <
  instance < override), templates flattened at create time, CLI flags
  that are just edits to the YAML.
- Readiness as ordered probes keyed to a per-boot instance ID.
- `shell` is ssh. Lima's fastest variant is ssh over vsock
  (6.3 Gbit/s vs 4 Gbit/s over the NAT); vmuc does ssh over a vsock
  tunnel through the guest agent, so no host TCP port is needed.

Efficiency levers that ArcBox measured and that we adopt from day one:
in-kernel GIC with SPI-only wake, batched virtqueue pop/push (not
batching "causes excessive VM exits"), dedicated I/O threads rather than
an async runtime inside the VMM, TSO on virtio-net, a tickless guest
kernel. ArcBox's idle CPU of 6.7% is with an 18-vCPU guest at HZ=1000 and
no `CPU_IDLE`; vmuc targets a 4-vCPU guest at HZ=250 with NO_HZ_IDLE.

## Networking without vmnet

vmnet.framework needs either root or the `com.apple.vm.networking`
entitlement, which is restricted (provisioning profile from Apple). An
ad-hoc signed binary claiming it is killed by AMFI on launch (ArcBox
`bundle/arcbox.dev.entitlements` comment). Lima solves this with
gvisor-tap-vsock (Go) or a sudo-installed `socket_vmnet`. Neither is
available to us.

So vmuc ships its own user-space NAT, modeled on ArcBox's datapath
rather than a full TCP/IP stack: ARP, DHCP, DNS forwarding, ICMP echo
proxy, UDP proxy, and TCP *termination plus splice* to real host
sockets. ArcBox measures this at 22.7 Gbps host→guest and 15 Gbps
guest→host single-flow on HV, against ~10 Gbps on VZ. Port forwarding
goes over vsock (guest agent reports listening sockets via netlink
`inet_diag`, host opens listeners and tunnels each connection), not
through the NAT. Outbound only via the NAT; no bridged networking in
v0.1.

## File sharing

Lima's default on VZ is Apple's virtiofs. We have no Apple devices, so
vmuc needs its own virtiofs server: a FUSE-protocol server reading
requests from a virtqueue and executing them against the host directory
(passthrough). ArcBox has one (`arcbox-fs` + `arcbox-virtio-fs`, HV only,
with DAX via per-file `hv_vm_map`). This is the largest single component
after the VMM and is scheduled after networking. The FUSE protocol is
host-OS independent, so the server can be developed and tested on the
Linux box through `/dev/fuse` before it ever meets a virtqueue.

## Guest images and kernel

Direct kernel boot means vmuc ships its **own kernel** (all drivers
built in, `CONFIG_MODULES=n`, like ArcBox and Firecracker) and boots
**stock distro cloud images** as the root filesystem with cloud-init
NoCloud for user, keys, mounts and provisioning (Lima's `cidata.iso`
mechanism). Ubuntu cloud images are qcow2 with compressed clusters, so
vmuc needs a qcow2 reader that converts to a raw sparse file at create
time (Lima has a pure-Go one; vmuc writes its own in Rust). A GPT parse
at create time finds the root partition for `root=/dev/vdaN`.

The Linux x86 box is the right place to build the kernel: cross-compile
arm64 on 20 cores, then boot the kernel, rootfs, cloud-init seed, and
guest agent under `qemu-system-aarch64 -M virt` (TCG, no KVM needed)
with the same virtio-mmio device set, including QEMU's
`free-page-reporting=on` balloon to validate the guest side of
requirement 3. That is a QEMU *test harness* on the dev box, not a QEMU
dependency in the product.

## Distribution: Homebrew formula

- Lima's `Makefile` runs `codesign -f -v --entitlements vz.entitlements -s - limactl`
  and its homebrew-core formula builds from source and is bottled for
  arm64; so ad-hoc entitlements survive Homebrew's bottling and
  relocation (Homebrew re-signs with `--preserve-metadata=entitlements`).
- vmuc's formula: `depends_on arch: :arm64`, `depends_on macos: :sequoia`,
  build with `cargo install`, then
  `codesign --force --sign - --entitlements vmuc.entitlements bin/vmuc`.
  Entitlements: `com.apple.security.hypervisor`, network client/server.
  Nothing restricted.
- Kernel image ships as a formula `resource` with a pinned sha256
  (offline-capable); dev builds download it from the GitHub release.
- Start in a tap (`brew install <org>/vmuc/vmuc`); homebrew-core once
  there are users.
- GitHub-hosted macOS arm64 runners **cannot run Hypervisor.framework**
  (no nested virtualization; `HV_UNSUPPORTED`). Lima runs its vz CI on
  `macos-15-large`, which is Intel, and ArcBox on `macos-26` for build
  and unit tests only. vmuc end-to-end CI needs a self-hosted Apple
  silicon runner.

## Development on an x86 Linux host

What can and cannot be done here:

| Can do on this box | Cannot do here |
|--------------------|----------------|
| All hypervisor-agnostic crates with unit tests: virtqueue and device models against byte buffers, FDT, qcow2, GPT, ISO9660, config/templates, CLI, TUI, net datapath (tun/pcap harness), virtiofs server (`/dev/fuse` harness), guest agent | Anything that calls `hv_*` |
| `cargo check` and `cargo clippy --target aarch64-apple-darwin` for the whole workspace (hand-written FFI needs no SDK to type-check) | Linking or running macOS binaries |
| Cross-compile the guest kernel; boot it with rootfs, cloud-init, and agent under QEMU TCG; validate free page reporting on the guest side | Measuring anything about performance |
| Cross-compile the agent for `aarch64-unknown-linux-musl` and run it under QEMU | Entitlement and Homebrew packaging checks |

An Apple silicon Mac on macOS 15+ with Xcode command line tools is
required from milestone M2 onward, reachable over ssh so the Linux box
can `rsync` the tree and run `cargo test` and the e2e suite remotely.
Options: a Mac mini on the desk, an EC2 `mac2` instance (bare metal, HV
works), or a hosted Mac from MacStadium or Scaleway. Hardware with ≥16 GB
RAM, so the memory experiments have room.

## Licensing and reuse

- ArcBox is MIT OR Apache-2.0. Porting specific, well-understood pieces
  (the HV FFI surface, the release sequence, the FDT layout, exit decode
  tables) into vmuc under Apache-2.0 is allowed with attribution in
  `NOTICE`. Vendoring whole crates is not the plan; they carry a daemon,
  Docker, and sandbox architecture vmuc does not want.
- Lima is Apache-2.0 and Go; it informs design only (CLI shape, instance
  directory, cidata, requirements, port forwarding rules).
- Third-party Rust crates are fine for non-VM concerns (clap, serde,
  ratatui, libc, thiserror, mio, flate2). Crates that implement a VMM,
  hypervisor bindings, virtio queues or devices, or a virtiofs server
  are out, per requirement 4 read strictly. `virtio-bindings` (constants
  only) and `vm-fdt` (a device-tree *writer*) are the two borderline
  cases; the plan allows them. See `plan.md` "Locked decisions".

## Risk register

| Risk | Likelihood | Mitigation |
|------|-----------|------------|
| `hv_gic_*` behaviour differs between macOS 15 and 26 | Medium | Test matrix on both; ArcBox uses the config-object API from the Xcode 26 SDK |
| Cloud images misbehave under a foreign kernel (missing built-ins: netfilter, overlayfs, br_netfilter, nbd…) | Medium | Kernel config is a superset of what Ubuntu/Alpine/Docker expect; QEMU boot test on Linux gates every kernel change |
| Guest memory fragmentation leaves freed memory unreported (below 2 MiB runs) | Medium | Agent triggers `compact_memory` on host pressure; optional balloon target as a fallback; measure |
| Homebrew bottle drops the entitlement | Low | Verify with a real bottle in M0; Lima proves the path |
| Apple changes `hv_vm_map` semantics (overlap, alignment) | Low | Probe binary (`stage2_matrix` equivalent) checked in and run on every macOS update |
| x86 dev box slows the HV-specific work | High, certain | Secure the Mac before M2; all non-HV work stays on Linux |
| Volume: VMM + virtio + NAT + virtiofs + CLI + TUI ≈ 25k–35k lines | Certain | Strict milestone order, memory manager early, DAX and snapshots deferred |

## Sources

- `../arcbox/virt/arcbox-vmm/AGENTS.md` "Releasing guest RAM";
  `../arcbox/virt/arcbox-vmm/src/vmm/darwin_hv/page_release.rs`;
  `../arcbox/virt/arcbox-virtio-balloon/`;
  `../arcbox/engine/arcbox-engine/src/vm_lifecycle/balloon/`;
  `../arcbox/docs/architecture/hv-backend.md`;
  `../arcbox/docs/adr/0001-hv-spi-is-the-whole-wake.md`;
  `../arcbox/docs/benchmarks/network.md`
- `../lima/website/content/en/docs/dev/internals.md`;
  `../lima/pkg/driver/vz/vm_darwin.go` (balloon attached, never driven);
  `../lima/Makefile` (ad-hoc codesign); `../lima/.github/workflows/test.yml`
- https://github.com/lima-vm/lima/issues/2789
- https://github.com/thewesjohnson/macos-virtio-balloon-test
- https://github.com/tinylabscom/mvm/issues/3381 and pull/3478
- https://orbstack.dev/blog/dynamic-memory
- https://github.com/containers/libkrun (HVF backend, emulated GIC path)
- https://docs.kernel.org/admin-guide/kernel-parameters.html (`page_reporting.*`)
- https://docs.github.com/actions/reference/github-hosted-runners-reference (no nested virtualization on macOS runners)
