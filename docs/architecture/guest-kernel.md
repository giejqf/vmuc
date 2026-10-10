# Guest kernel

vmuc ships its own arm64 Linux kernel. The VMM loads the uncompressed
`Image` straight into guest RAM next to a device tree it generates, with
no firmware, bootloader, EFI or ACPI in between. Everything lives under
`guest/kernel/`; `make kernel` builds it and `make kernel-test` boots it
under QEMU on the Linux dev box.

```
guest/kernel/
  VERSION               kernel version (line 1) and tarball sha256 (line 2)
  vmuc.config           kconfig fragment merged onto arch/arm64 defconfig
  build.sh              download, verify, configure, cross-build, publish to out/
  initramfs/init.c      static C init used only by the QEMU boot test
  initramfs/build.sh    packs it into out/boottest-initramfs.cpio.gz
  qemu-boot-test.sh     boots out/Image under qemu-system-aarch64 -M virt
  build/                scratch: tarball, source tree, objects (git-ignored)
  out/                  Image, config, version.txt, boot-test logs
```

## Why direct boot, why no modules

**Direct boot.** The VMM already knows the machine it builds (GICv3,
arch timer, PL011, PL031, virtio-mmio at fixed addresses), so it
describes that machine in an FDT and jumps to the kernel entry point.
There is no disk to scan for a kernel, no UEFI image to run, no
`grub.cfg` to parse, and the kernel does not need an EFI stub, ACPI
tables or PCI enumeration. This is the shortest path from `vmuc start`
to `init`, which matters for the < 400 ms kernel-to-init target, and it
means the guest image's own kernel (and its initramfs with hundreds of
modules) is never used.

**No modules.** The hardware set is closed: whatever the VMM can present
is known at build time, so every driver is built in and `CONFIG_MODULES`
is off. Consequences:

- The kernel is a single file with a sha256; there is no `/lib/modules`
  to ship, no version-matching between kernel and root filesystem, and
  cloud images from Ubuntu or Alpine boot unmodified on it.
- Nothing is loaded at runtime, so no `modprobe` latency during boot and
  no surprise code paths (`virtio_mmio.device=` works without an
  initramfs step).
- A kconfig subtlety: with `CONFIG_MODULES=n`, every `=m` in defconfig
  becomes `=y`. The fragment therefore switches whole subsystems off
  rather than trusting defconfig to leave them as modules.

## Version

`guest/kernel/VERSION` names the newest patch release of the newest
longterm series on kernel.org at the time of the last bump (6.18.y
while this is written). The LTS series gives two-plus years of security
backports without the churn of moving between major versions; bumping
within the series is a two-line change (below).

## Configuration groups

`vmuc.config` is a fragment applied with `scripts/kconfig/merge_config.sh`
onto `arch/arm64/configs/defconfig`. Disables are written as
`# CONFIG_X is not set` so both merge_config's own check and
`build.sh`'s verification pass can compare them. The groups, in the
order they appear in the fragment:

| Group | What | Why |
|-------|------|-----|
| Identity | `LOCALVERSION="-vmuc"`, `IKCONFIG_PROC` | `uname -r` says `6.18.55-vmuc`; `/proc/config.gz` lets `vmuc info` and Docker's `check-config.sh` inspect the running kernel. |
| Platform | `ACPI=n`, `PCI=n`, `EFI=n`, every `ARCH_*` SoC off, SCMI/SCPI/SDEI/TEE off, `ARM_PSCI_FW=y` | One device-tree machine. Dropping the SoC families removes most of defconfig's drivers, clocks, pinctrls and PHYs in one stroke. PSCI is how the VMM boots secondary CPUs and powers off. |
| CPU | `SMP`, `NR_CPUS=64`, `HOTPLUG_CPU`, 4 KiB pages, `VA_BITS_48`, `HZ_250`, `NO_HZ_IDLE`, `NUMA=n`, `COMPAT=n`, `CPU_IDLE=n`, `CPU_FREQ=n`, `PARAVIRT=n`, `KVM=n`, `XEN=n`, no hibernate/suspend/kexec/crash dump | 4 KiB guest pages are the unit the balloon and page reporting work in while the host uses 16 KiB pages. HZ=250 with tickless idle keeps an idle 4-vCPU guest under the 0.5 % CPU target (ArcBox burned 6.7 % at HZ=1000). Idle is plain WFI trapped by the VMM; cpuidle governors would only add latency. Apple cores have no AArch32 EL0, so `COMPAT` is dead weight. The guest is neither a hypervisor nor a KVM/Xen guest. |
| Memory | `PAGE_REPORTING=y`, `VIRTIO_BALLOON=y`, `BALLOON_COMPACTION`, `TRANSPARENT_HUGEPAGE=y` (madvise), `INIT_ON_FREE_DEFAULT_ON=n`, `INIT_ON_ALLOC_DEFAULT_ON=n`, `PAGE_POISONING=n`, `CMA=n`, `KSM=n`, `ZSWAP=n`, `MEMCG`, `PSI=y`, `USERFAULTFD`, `MEMORY_HOTPLUG`/`HOTREMOVE`/`ZONE_DEVICE`/`FS_DAX` | This is the product. Free page reporting is the bulk path that returns freed guest memory to macOS; the balloon driver is what registers it (and carries inflate/deflate and stats). THP keeps `pageblock_order` at 9 so reports arrive as 2 MiB runs, which the host can unmap whole. The two "init/poison" options would write to every freed page, re-dirtying memory the guest is about to report, so they are pinned off (an AGENTS.md invariant). CMA would reserve RAM for DMA nobody needs. PSI feeds the host-pressure loop. The hotplug/ZONE_DEVICE/DAX trio is only there because `FUSE_DAX` (virtiofs DAX window) depends on it. |
| Console, RTC, random | `SERIAL_AMBA_PL011` + console + earlycon, `VIRTIO_CONSOLE` (hvc), `RTC_DRV_PL031` + `HCTOSYS`, `HW_RANDOM_VIRTIO`, `VT=n`, `INPUT=n`, `EXPERT=y` | PL011 is the early/serial console, hvc0 the virtio one `vmuc shell` attaches to. PL031 gives the guest wall-clock time at boot. The VMM passes `rng-seed` in the FDT (trusted by default since 6.2; the old `RANDOM_TRUST_BOOTLOADER` knob no longer exists) and virtio-rng keeps the pool fed. There is no display or keyboard, so no virtual terminal or input layer; `VT` is only switchable under `EXPERT`, which is why `EXPERT=y` and the core syscalls it exposes are pinned on explicitly. |
| virtio | `VIRTIO_MMIO` + `_CMDLINE_DEVICES`, blk, net, console, balloon, rng, `VSOCKETS`/`VIRTIO_VSOCKETS`, `VIRTIO_FS` + `FUSE_FS` + `FUSE_DAX`; `VIRTIO_PCI=n`, vhost/9p/iommu/mem/pmem off | The VMM's transport is virtio-mmio v2 only. `virtio_mmio.device=` on the command line is a debugging aid and lets tests add devices without a DT change. vsock carries the agent protocol; virtiofs the shared directories. |
| Block, DM, initrd | `BLK_DEV_INITRD` (gzip/xz/zstd), `LOOP`, `BLK_DEV_DM` with crypt, verity, snapshot, thin, zero; `BLK_CGROUP`, BFQ/Kyber; `SCSI`, `ATA`, `MMC`, `MTD`, `NVME`, `NBD` off | Loop and dm-* back image tooling, encrypted volumes and containerd snapshotters; the initrd path is what the boot test and future rescue flows use. No physical storage buses exist. |
| Filesystems | ext4, btrfs, xfs, erofs (lz4/lzma/zstd), overlayfs (redirect, index, metacopy), iso9660 + joliet + zisofs (Rock Ridge is unconditional), vfat/msdos + NLS utf8, squashfs (all decompressors), tmpfs + acl + xattr, devtmpfs, hugetlbfs, autofs, fanotify, quota, binfmt_misc, NFS v3/v4.x client | Cloud images are ext4 or btrfs, cidata is iso9660 or vfat, snapshotters want erofs/overlay/squashfs, Docker wants overlay with redirect_dir and ACL/xattr everywhere. hugetlbfs is there for the cgroup hugetlb controller. No NFS server, CIFS, ceph, f2fs. |
| cgroups, namespaces, security | cgroup v2 with every controller (cpu, cpuset, memory, io, pids, freezer, hugetlb, device, perf, bpf, misc, net_prio/classid), all namespaces, `CHECKPOINT_RESTORE`, `SECCOMP_FILTER`, `SECURITY` + network hooks, keys, audit | Docker, nerdctl, systemd and rootless containers need the full set. No LSM policy (AppArmor/SELinux) is compiled; add one in its own change if a template needs it. |
| BPF and tracing | `BPF_SYSCALL`, JIT always on, unprivileged BPF off, `DEBUG_INFO_BTF`, `FTRACE_SYSCALLS`, kprobes/uprobes, `BPF_EVENTS`, `FUNCTION_TRACER`, tc/xt/cgroup BPF hooks, XDP sockets | The agent's listening-socket watcher attaches to the `syscalls/sys_exit_bind` tracepoint; that needs syscall tracepoints and BPF events, and CO-RE loaders need BTF. BTF forces DWARF generation at build time (`DEBUG_INFO_NONE` cannot coexist with it), but `Image` is an objcopy of vmlinux that keeps only the `.BTF` section, so the shipped kernel carries no DWARF. |
| Networking | IPv4/IPv6 with policy routing, diag sockets, XFRM/ESP, cubic + BBR, fq_codel default qdisc, tc BPF/cgroup classifiers, VLAN, bridge (VLAN filtering, IGMP snooping), veth, macvlan, ipvlan, tun, dummy, vxlan, wireguard; `ETHERNET=n`, no PHY, wireless, bluetooth, CAN, DSA | virtio-net is the only NIC. The virtual-NIC set is what Docker's bridge/overlay/macvlan drivers and nerdctl's CNI plugins create. |
| Netfilter | nf_tables (inet/netdev/bridge families, nat, masq, redir, compat, fib, socket, tproxy, flow offload), conntrack + netlink + ftp/tftp helpers, legacy iptables/ip6tables (`NETFILTER_XTABLES_LEGACY`) with filter/nat/mangle/raw and the xt matches Docker's `check-config.sh` lists, `BRIDGE_NETFILTER`, IPVS (rr) | Both iptables-legacy and iptables-nft work, so any Docker/moby version and firewalld-free distros are covered. |
| Crypto | AES, XTS, ESSIV, SHA-2, HMAC, GCM, AF_ALG user APIs, arm64 CE AES/GHASH | dm-crypt/cryptsetup and WireGuard; the CE drivers are what Apple cores accelerate. Hardware crypto engines are off. |
| Never-present hardware | DRM, FB, backlight, logo, sound, USB, IOMMU, I2C, SPI, SPMI, PWM, HWMON, thermal, watchdog, regulator, LEDs, IIO, mailbox, remoteproc, rpmsg, soundwire, extcon, interconnect, FPGA, media, DMA engines, UIO, VFIO, pinctrl, GPIO, SMMU, EDAC, RAS, staging, PTP, ChromeOS/Surface, PCMCIA | Hard requirement 5 (no graphics) and the general rule: nothing the VMM cannot present. |
| defconfig leftovers | HSR, IP_PNP, firmware loaders, GNSS, SRAM, WWAN, serio, SoC UARTs, IPMI, TPM, PMICs, Cadence PHYs, system PMUs, counters, CUSE, memtest | Each is `=y` or `=m` in defconfig without a platform dependency, so nothing in the groups above removes it. Found with `make savedefconfig` after the merge. |

Two things the fragment deliberately does **not** change: the
preemption model stays at defconfig's `PREEMPT=y` (a server-style
`PREEMPT_VOLUNTARY` would be a benchmarked decision, not a config edit),
and `CONFIG_CMDLINE` is empty because the VMM owns the command line.

### Verification

`build.sh` refuses to build if any line of the fragment did not survive
the merge: every `CONFIG_X=v` must appear verbatim in the final
`.config`, and every `# CONFIG_X is not set` must not be contradicted
by `CONFIG_X=y`. This catches the usual silent failures (an option with
an unmet dependency is dropped without a word, a `select` elsewhere
forces one back on). When it fires, the message names the line and the
actual value; `find . -name 'Kconfig*' | xargs awk '/^(menu)?config
/{c=$2} /select SYMBOL/{print c}'` in the source tree finds the
selector. Three such selectors are worth knowing: `POWER_RESET` and
`POWER_SUPPLY` are selected by `ARM64` itself (empty classes here), and
`MFD_SYSCON` is selected by the PL031 driver.

## Building

```
make kernel                     # == guest/kernel/build.sh
CLEAN=1 guest/kernel/build.sh   # wipe build/ and out/ first
JOBS=8 guest/kernel/build.sh    # default is nproc
```

`build.sh` downloads the tarball into `guest/kernel/build/` (skipped
when present and the sha256 matches), extracts once, merges the
fragment, verifies it, builds `arch/arm64/boot/Image` with
`ARCH=arm64 CROSS_COMPILE=aarch64-linux-gnu-`, and copies `Image`,
the final `.config` and a `version.txt` (version, `uname -r` release,
tarball and Image sha256, size) into `guest/kernel/out/`. Builds are
reproducible: `KBUILD_BUILD_USER=vmuc`, `KBUILD_BUILD_HOST=vmuc`, and
`KBUILD_BUILD_TIMESTAMP` derived from the release's own file mtime, so
two machines building the same `VERSION` produce the same `uname -v`,
and `JOBS=1` is passed to make so `scripts/Makefile.btf` runs pahole
single-threaded: with `-jN`, pahole 1.25 emits BTF in a
scheduling-dependent order and two otherwise identical builds differed
in `.BTF` and the type ids patched into `.BTF_ids`. With that pinned,
two `CLEAN=1` builds on the same toolchain give the same `Image`
sha256. Re-running is incremental; the merge is redone only when the
fragment changed.

Dependencies on Ubuntu 24.04: `gcc-aarch64-linux-gnu`, `flex`,
`bison`, `bc`, `libssl-dev`, `libelf-dev`, `dwarves` (pahole, for
BTF), `qemu-system-arm` (for the test). A full build takes about 3.5
minutes on 20 cores (45 CPU-minutes); `Image` is about 30 MiB, of
which `.text` is 14.5 MiB, `.BTF` 4.8 MiB and `.rodata` 3.6 MiB.
`build.sh` ends by running `initramfs/build.sh`, so one invocation
leaves everything `qemu-boot-test.sh` needs in `out/`.

## Testing

```
make kernel-test                # build, then guest/kernel/qemu-boot-test.sh
guest/kernel/initramfs/build.sh # rebuild only the boot-test initramfs
```

`qemu-boot-test.sh` starts
`qemu-system-aarch64 -M virt -cpu cortex-a72 -smp 2 -m 1024 -nographic
-no-reboot` under TCG with device tree only, `-global
virtio-mmio.force-legacy=false` (virtio-mmio v2, the same transport the
VMM implements), and virtio-mmio blk (a 64 MiB empty raw file), net
(user-mode), rng, balloon with `free-page-reporting=on`, and a
virtio-serial port. The kernel gets `console=ttyAMA0
earlycon=pl011,0x9000000 panic=-1 vmuc.expect=net,blk,console,rng,balloon`
and the initramfs from `initramfs/init.c`. (6.18 has no
`page_reporting.page_reporting_delay_ms` parameter, only
`page_reporting_order`; the reporting delay is a hard-coded 2 s, so
the command line does not try to set one.)

That init is a static C program of a few hundred lines: it mounts proc,
sysfs and devtmpfs, prints `uname -r`, walks
`/sys/bus/virtio/devices/*/device` printing each device ID and the
driver bound to it, checks the IDs named in `vmuc.expect=` are present
and bound, checks `/sys/bus/virtio/drivers/virtio_balloon`,
`/sys/module/page_reporting/parameters/page_reporting_order` (visible
only when `PAGE_REPORTING` is compiled in),
`/sys/kernel/mm/transparent_hugepage/enabled` and `/proc/vmstat`,
writes `VMUC_HVC_OK` to `/dev/hvc0`, prints `VMUC_CHECK <name> ok|missing`
for each check and finally `VMUC_BOOT_OK` or `VMUC_BOOT_FAIL <reason>`,
then calls `reboot(RB_POWER_OFF)`. The script tails the log, kills QEMU
on a 300 s timeout (TCG on a GitHub runner is slow), prints the
wall-clock time from QEMU start to `VMUC_BOOT_OK`, and exits 0 only on
that marker. Console output lands in `out/boot-test.log`, the
virtconsole port in `out/boot-test-hvc0.log`.

### What the harness proves, and what it cannot

It proves the kernel links, boots on a GICv3 device-tree machine via
PSCI with two CPUs, probes modern virtio-mmio devices and binds the
blk, net, console, rng and balloon drivers, has page reporting and THP
compiled in, and exposes cgroup v2, all without a single module. It
proves the configuration will not regress silently, because a dropped
option shows up as a failed `VMUC_CHECK` or a failed `build.sh`
verification.

Under TCG on the 20-core dev box the whole thing takes about 4 s
from QEMU start to `VMUC_BOOT_OK` (2.4 s of guest time to `/init`).

It does not prove anything about Hypervisor.framework: QEMU's GIC,
timer, PSCI and virtio implementations are not ours, so vCPU exit
handling, interrupt delivery, the memory manager's unmap/remap
sequence and every `hv_*` contract in AGENTS.md remain "unit-tested on
Linux, not yet run on HV" until `make mac-e2e` says otherwise. It does
not measure performance: TCG is an interpreter on x86, so boot time
under it (tens of seconds) says nothing about the < 400 ms target, and
no throughput or idle numbers can be taken from it. It does not
exercise vsock, virtiofs, networking beyond link-up, or free page
reporting end to end (the balloon device is present and the driver
binds, but whether pages are reported and reclaimed is measured by
`tests/bench/mem-reclaim` on the Mac). It is a smoke test, run on
every change under `guest/kernel/` by `.github/workflows/kernel.yml`,
which also uploads `Image`, `config`, `version.txt` and the boot log
as the `vmuc-kernel-<version>-arm64` artifact.

## Bumping the version

1. `curl -s https://www.kernel.org/releases.json` and pick the newest
   patch of the newest longterm series (or the next patch of the
   current series for a routine security bump).
2. Fetch `https://cdn.kernel.org/pub/linux/kernel/v6.x/sha256sums.asc`
   and copy the tarball's sha256 into line 2 of `guest/kernel/VERSION`;
   line 1 is the version.
3. `make kernel-test`. If `build.sh` reports a fragment line that no
   longer applies (an option renamed or removed upstream), fix the
   fragment in the same change and note it in the commit message.
4. On a series change (6.18 → 6.24), re-run `make savedefconfig` in
   `guest/kernel/build/obj` and scan it for new defconfig leftovers.
5. Commit `VERSION`, the fragment if it changed, and the new
   `docs/logs/` entry if anything needed fixing. CI publishes the
   artifact; the release asset and `vmuc-image/src/assets.rs` sha256
   are updated when a release is cut.
