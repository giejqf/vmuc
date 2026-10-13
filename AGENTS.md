# vmuc — Repository Guidelines for Coding Agents

This is the only root instruction file. Claude Code reads `AGENTS.md`
directly; there is no `CLAUDE.md`. Changes to this file need human
approval.

## What vmuc is

A CLI-first manager for ARM Linux virtual machines on Apple silicon
Macs, written in Rust on a custom VMM that talks to Hypervisor.framework
directly. Think Lima's user experience on an engine that owns guest
memory, so memory the guest frees goes back to macOS within seconds.
There is a terminal UI and no graphical UI.

The decisions are in `docs/plan.md` ("Locked decisions") and the
evidence behind them in `docs/feasibility.md`. Read both before
proposing architecture. Do not re-open a locked decision in a code
change; propose it in `docs/adr/` first.

## Hard requirements (never trade these away)

1. Rust. CLI first; the TUI is a client of the same control socket. No
   web UI, no desktop app.
2. Host: macOS 15+ on Apple silicon. Guest: arm64 Linux. Nothing else,
   no compatibility shims for anything else.
3. Memory is the product: guest free page reporting is always on and
   released to the host with the unmap/refresh/remap sequence; host
   pressure is answered; every byte is accounted for in `vmuc stat`.
4. Hypervisor.framework only. No Virtualization.framework, no QEMU, no
   libkrun, no vmnet, no crate that wraps any of them. Our own
   hypervisor bindings, VMM, virtqueues, devices, NAT, and virtiofs.
5. No graphics devices, no framebuffer, no GPU.
6. Performance and efficiency win ties. Idle must cost nothing
   measurable; hot paths are custom and benchmarked.

Allowed third-party crates and the forbidden list are in
`docs/plan.md`. Adding a dependency outside the allowed list needs a
one-line justification in the PR and an update to that list.

## Repository map

```
crates/vmuc-hv       Hypervisor.framework FFI (hand-written) + safe wrappers; stubs off macOS
crates/vmuc-vmm      VM lifecycle, guest RAM + memory manager, vCPU loop, PSCI, GIC wiring, FDT, PL011/PL031
crates/vmuc-virtio   virtqueue, mmio transport, device models (console, blk, net, vsock, rng, balloon, fs)
crates/vmuc-fs       virtiofs server (FUSE protocol, passthrough)
crates/vmuc-net      user-space NAT, DHCP/DNS, port forwarder
crates/vmuc-image    downloads, cache, qcow2 → raw, GPT, ISO9660, sparse files
crates/vmuc-config   YAML schema, layering, templates
crates/vmuc-proto    CLI↔VMM and host↔agent message types
crates/vmuc-agent    guest agent (aarch64-unknown-linux-musl, static)
crates/vmuc-tui      ratatui front end
crates/vmuc-cli      the `vmuc` binary (CLI, hidden `_vmm` and `_proxy` roles)
guest/kernel         kernel config, build script, QEMU boot test
guest/templates      instance templates and image lists
tests/e2e            Mac-only end-to-end tests
tests/bench          rerunnable probes behind every number in docs/benchmarks
docs/                feasibility, plan, architecture/, adr/, experiments/, benchmarks/, logs/
packaging/homebrew   formula
```

Per-crate `AGENTS.md` files are added when a crate accumulates rules of
its own; when one exists, read it before editing that crate.

## Two development environments

The primary dev box is x86 Linux and cannot run Hypervisor.framework. A
remote Apple silicon Mac (`$VMUC_MAC`, ssh) is used for everything that
calls `hv_*`.

- On Linux: `make check` (fmt, clippy `-D warnings`, tests,
  `cargo check --target aarch64-apple-darwin --workspace`), `make kernel`
  (cross-build and QEMU boot test), `make agent`.
- On the Mac via `make mac-test` / `make mac-e2e` / `make bench NAME=`
  from the Linux box, or `make e2e` / `make bench-local NAME=` on the Mac
  itself.
- `vmuc-hv` must compile on Linux as stubs so clippy covers every call
  site. Device models take `&mut [u8]` guest memory and a queue, never a
  VM handle, so they are unit-tested on Linux.
- QEMU on the Linux box is a *test harness* for the guest kernel, rootfs,
  cloud-init seed and agent. It is never a runtime dependency and never
  an abstraction target in the code.
- Do not claim something works on HV unless it ran on the Mac. Say
  "unit-tested on Linux, not yet run on HV" when that is the state.

## Validation ladder (cheapest first, run before every push)

1. `cargo fmt --check`
2. `cargo clippy --workspace --all-targets -- -D warnings`
3. `cargo test --workspace`
4. `cargo check --target aarch64-apple-darwin --workspace`
5. If `guest/` or the agent changed: `make kernel` (QEMU boot test) and
   clippy for `aarch64-unknown-linux-musl`.
6. If `vmuc-hv`, `vmuc-vmm`, `vmuc-virtio`, or the boot path changed:
   `make mac-e2e`.
7. If a hot path changed: the matching `make bench NAME=` with the row
   appended to `docs/benchmarks/<name>.md`.

## Hypervisor.framework rules (learned the hard way elsewhere, don't relearn)

- One VM per process. Each instance is its own `vmuc _vmm` process.
- A vCPU is `!Send`: create, run and destroy it on one dedicated OS
  thread. Create each vCPU **once** and reuse it across PSCI CPU_OFF /
  CPU_ON; recreating deadlocks on `hv_vcpu_create`'s global lock.
- Exit handling PC rules: data abort (MMIO) → the VMM advances PC by 4;
  HVC → HV has already advanced PC, do not touch it; SMC → HV has *not*
  (the preferred return address is the SMC itself): the VMM advances PC
  by 4 after handling it; unknown system register trap → read-as-zero /
  write-ignored and PC += 4 (Linux writes `OSDLR_EL1` early and hangs
  otherwise); refuse data aborts with ISV=0 and skip the instruction.
  Measured, not assumed: `crates/vmuc-hv/tests/contract.rs`
  (docs/experiments/2026-10-11-hv-contract-m4-pro.md); rerun it after
  every macOS update.
- The GIC redistributor region is sized for the framework's maximum
  vCPUs (32 MiB for 64 on an M4 Pro), not the ones created; it lives at
  `0x0C00_0000`, clear of the MMIO devices. With the in-kernel GIC, WFI
  idles inside `hv_vcpu_run`: no WFI exit handling on the hot path.
- Interrupts go through Apple's in-kernel GICv3 (`hv_gic_*`). Asserting
  an SPI with `hv_gic_set_spi` **is** the whole wake; never follow it
  with `hv_vcpus_exit`. `hv_vcpus_exit` is for stop and pause only.
- An `hv_vcpus_exit` can be used up by an exit already on its way (an
  MMIO access, say): the vCPU handles that exit and enters the guest
  again. So a vCPU checks the stop and pause flags before every
  `hv_vcpu_run`, and whoever stops or pauses keeps kicking (every 2 ms)
  until every vCPU has left (docs/logs/2026-10-13-lost-vcpu-cancels.md).
- SPIs are level-triggered; the device deasserts on ACK.
- Teardown order is a contract: join vCPU threads → join every worker
  that holds a guest-memory reference → destroy the GIC →
  `hv_vm_destroy` → unmap guest RAM. Violating it is a SIGSEGV.
- So a worker that teardown joins never makes a host call that can block
  indefinitely: a file under `~/Desktop` waits for a privacy (TCC)
  prompt, an iCloud file for its download, a FIFO for a writer. Such
  calls go to threads that hold no guest memory and are not joined
  (virtio-fs runs requests on a pool for this reason).
- `hv_vm_map` refuses a map over a live mapping: unmap first. Once a
  range is unmapped, any host range may go there, file-backed or
  anonymous, also into a hole of a larger mapping (macOS 26.6.2,
  `crates/vmuc-hv/tests/dax.rs`; unverified on macOS 15, so the memory
  manager still maps the same host address back). It also refuses
  read-only host mappings: DAX maps files shared from a read-write
  descriptor, read-only in stage 2.
- Every `unsafe` block has a `// SAFETY:` comment naming the invariant.

## Memory manager invariants

- The only sequence that returns guest memory to macOS is
  `hv_vm_unmap(range)` → fresh ledger-tagged granules over the same host
  address (`vmuc_hv::guest_memory::refresh`) → `hv_vm_map(range)` →
  acknowledge the report. Guest RAM comes only from
  `guest_memory::allocate`: plain anonymous memory is billed twice for
  every page the host and the guest both touch (ADR 0003). `madvise`,
  `MADV_ZERO` and `hv_vm_unmap` alone release nothing, whatever a
  host-side `memset` calibration suggests.
- Host pages are 16 KiB and release granules 128 KiB. Align reported
  ranges **inward** to the granule; anything smaller stays billed.
- Free page reporting is the only release path. The balloon device
  exists for reporting and the guest's stats; vmuc never sets a balloon
  target (`num_pages` stays 0).
- Reclaim never runs on a vCPU thread.
- A failed remap leaves a hole in guest RAM. Retry once, then stop the
  VM loudly. Never acknowledge a report whose range is not mapped back.
- There are no host-driven balloon targets (ADR 0004): in E1 they never
  returned more than dropping caches, collapsed an idle guest to its
  floor, oscillated under pressure and starved bursts of allocation.
  The pressure responder drops caches and compacts, nothing more. A new
  host-driven shrink of any kind needs a `docs/experiments/` entry in
  which it beats that on `tests/bench/mem-pressure`.
- Guest kernel keeps `INIT_ON_FREE_DEFAULT_ON=n` and `PAGE_POISONING=n`;
  both would re-dirty pages the guest is about to report.

## Code standards

- `cargo fmt` and `cargo clippy -- -D warnings` clean. Edition 2024.
- `thiserror` for library errors, `anyhow` only in `vmuc-cli`. Convert
  errors with `From` impls, not ad-hoc mapping helpers.
- Hot paths: no `Arc<Mutex<T>>` on the data path; lock-free or
  single-owner threads with channels. Batch virtqueue pop/push; not
  batching causes excessive VM exits.
- No async runtime inside `vmuc-vmm`, `vmuc-virtio`, `vmuc-net`,
  `vmuc-fs`. Dedicated threads plus `mio` event loops.
- `// ====` section dividers mean a file should be split into a module
  directory. Do not add them.
- Prefer extension traits on types over free helper functions.
- Comments in English. Public API changes update docs in the same
  change.
- No hacky workarounds. If one seems unavoidable, stop and ask.
- Platform pitfalls: `mode_t` is `u16` on macOS; xattr parameter order
  differs from Linux; `fallocate` is absent (use `F_PREALLOCATE` /
  `F_PUNCHHOLE`); unix socket paths are limited to 104 bytes, so keep
  `~/.vmuc/<name>/` short and reject names that would overflow;
  `O_NOFOLLOW | O_NOFOLLOW_ANY` together is EINVAL.
- Unix datagram sockets on macOS hold 4 KiB and cap a datagram at 2 KiB
  by default (size both ends explicitly), and a send into a full one
  fails with ENOBUFS instead of waiting. A blocked `recv` never learns
  that the peer closed; the ECONNRESET the peer leaves goes to whichever
  call looks first, so detect a vanished peer with a receive timeout plus
  `getpeername` (ENOTCONN).

## Testing

- Tests are expected for every code change that has branching logic.
  Don't test the language or a dependency.
- Device models are tested against byte buffers on Linux.
- `tests/e2e` boots real VMs and runs only on the Mac.
- A number without a checked-in probe is not a result.

## Documentation discipline

- A question answered with data → `docs/experiments/YYYY-MM-DD-<slug>.md`
  (question → method → verdict).
- A measurement that will be repeated → a dated row in
  `docs/benchmarks/<subject>.md`.
- A root-cause hunt, incident, or notable change → `docs/logs/YYYY-MM-DD-<slug>.md`.
- Retiring a mechanism, choosing between designs, or setting a rule a
  later change must not undo silently → `docs/adr/NNNN-<slug>.md`, which
  cites the entries and does not repeat their tables.
- `docs/plan.md` "Locked decisions" is updated in the same commit as any
  change that alters a decision.

## Change discipline

- Branch `type/short-description`; commit messages `type(scope): summary`
  (`feat(vmm): PSCI CPU_ON`, `fix(net): splice half-close`).
- Atomic commits that compile and pass the ladder. Aim for ~200 changed
  lines, hard limit 400, excluding generated files.
- `cargo add` / `cargo remove` for dependency changes; review the
  lockfile diff.
- Never commit secrets, signing identities, or `$VMUC_MAC` hostnames.

## Licensing and attribution

- Everything is Apache-2.0. New files carry an SPDX header:
  `// SPDX-License-Identifier: Apache-2.0`.
- Code ported from ArcBox (MIT OR Apache-2.0) keeps a comment naming the
  source file and gets an entry in `NOTICE`. Port specific, understood
  pieces; do not vendor crates.
- Lima (Apache-2.0, Go) is a design reference only; nothing is copied.

## Performance targets (v0.1; measure, don't assert)

| Metric | Target | Reference |
|--------|--------|-----------|
| `vmuc start` → ssh prompt (Ubuntu cloud image, warm cache) | < 3 s | Lima vz ≈ 10 s+; ArcBox HV daemon-ready 11 s, target 1.5 s |
| Kernel to `init` on the serial console | < 400 ms | |
| Idle CPU, 4 vCPUs, Ubuntu idle | < 0.5% | ArcBox 6.7% with 18 vCPUs at HZ=1000 |
| Host footprint, idle Ubuntu guest after first reclaim | < 350 MiB | VZ 16 GB idle VM: 718 MB and growing |
| Freed guest memory back to host | ≥ 90% within 10 s | ArcBox 10 s; mvm 10.9 s |
| Reclaim-induced vCPU stall | < 5 ms | |
| Net single flow host↔guest | > 10 Gbps | ArcBox HV 22.7 / 15 Gbps; VZ ≈ 10 |
| vsock RPC p50 | < 0.3 ms | ArcBox 0.30 ms |
| virtiofs | ≥ VZ virtiofs on the same host | measure Lima vz first |

## macOS specifics

- The binary needs `com.apple.security.hypervisor`. Ad-hoc signing
  works: `codesign --force --sign - --entitlements vmuc.entitlements target/debug/vmuc`.
  `make mac-test` does this after every build.
- Never request `com.apple.vm.networking`; it is restricted and an
  ad-hoc signed binary claiming it is killed on launch. vmnet access
  lives only in `vmuc-vmnet-helper`, which runs as root via sudoers,
  relays frames, and does nothing else.
- The helper must exit with its VMM. A stale vmenet interface left on
  the shared bridge also strips TSO from every other VM's host traffic
  (a bridge offers only the offloads all its members have). From macOS
  15.4 vmnet reads and writes `virtio_net_hdr` itself
  (`vmnet_enable_virtio_header_key`, looked up at run time), which is
  what makes TSO through the helper possible.
- A helper change only takes effect once the user reinstalls the
  root-owned copy (`vmuc sudoers` prints the command); batch helper
  changes and say so.
- GitHub-hosted macOS runners cannot run HV. End-to-end CI runs on a
  self-hosted Apple silicon runner; unit tests and builds run on
  `macos-15` and `macos-26`.
- Test on both macOS 15 and 26 before a release; the GIC API surface
  moved between SDKs.
