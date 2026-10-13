// SPDX-License-Identifier: Apache-2.0
//! E5: a busybox guest snapshotted while it ticks carries on in new VMs
//! restored from the snapshot, with guest RAM copied in and mapped
//! copy-on-write: the next tick follows, virtio-rng answers, and a
//! second CPU works. Needs Apple silicon; see the crate docs.

use std::sync::Arc;
use std::time::Duration;

use vmuc_e2e::{Capture, artifact, vm_lock, watchdog};
use vmuc_virtio::devices::rng::Rng;
use vmuc_vmm::snapshot::RamRestore;
use vmuc_vmm::{ConsoleOut, VmBuilder, VmConfig, VmExit};

/// The guest writes 16 MiB, reads /dev/hwrng at tick 30, and ticks on.
fn builder(out: &ConsoleOut) -> VmBuilder {
    let mut b = VmBuilder::new(VmConfig {
        cpus: 2,
        ram_size: 512 << 20,
        cmdline: "console=ttyAMA0 panic=-1 quiet vmuc.test=tick vmuc.fill=16 vmuc.refill=30".into(),
        dax_windows: 0,
    })
    .unwrap();
    b.add_platform_devices(out.clone()).unwrap();
    b.add_virtio(Box::new(Rng::new().unwrap())).unwrap();
    b.add_balloon().unwrap();
    b
}

#[test]
#[ignore = "boots a VM: Apple silicon only"]
fn a_snapshot_carries_on_in_a_new_vm() {
    let _vm = vm_lock();
    let (kernel, initrd) = (artifact("Image"), artifact("busybox-initramfs.cpio"));
    let dir = tempfile::tempdir().unwrap();

    let cap = Capture::default();
    let (out, writer) = cap.console();
    let vm = builder(&out).boot(&kernel, Some(&initrd)).unwrap();
    watchdog(vm.machine(), Duration::from_secs(30));
    cap.wait_for("VMUC_TICK 12 ", Duration::from_secs(20));
    let st = vm.snapshot(dir.path()).unwrap();
    drop(vm);
    out.close();
    writer.join().unwrap();
    let last: u32 = cap
        .text()
        .lines()
        .filter_map(|l| {
            l.strip_prefix("VMUC_TICK ")?
                .split(' ')
                .next()?
                .parse()
                .ok()
        })
        .max()
        .unwrap();
    assert!(st.ram_bytes >= 16 << 20, "{st:?}");
    assert!(last < 30, "snapshot after the refill tick: {last}");

    for ram in [RamRestore::Copy, RamRestore::Map] {
        let cap = Capture::default();
        let (out, writer) = cap.console();
        let vm = builder(&out).restore(dir.path(), ram).unwrap();
        let m = Arc::clone(vm.machine());
        watchdog(&m, Duration::from_secs(30));
        cap.wait_for(&format!("VMUC_TICK {} ", last + 1), Duration::from_secs(5));
        cap.wait_for("VMUC_REFILLED hwrng=16", Duration::from_secs(10));
        // Tick 40 prints each CPU's busy time; tick 41 ends that line.
        cap.wait_for("VMUC_TICK 41 ", Duration::from_secs(10));
        m.request_exit(VmExit::Stopped);
        assert_eq!(vm.wait(), VmExit::Stopped);
        out.close();
        writer.join().unwrap();
        let text = cap.text();
        let cpus = text.lines().rfind(|l| l.starts_with("VMUC_CPUS")).unwrap();
        let busy: Vec<u64> = cpus
            .split_whitespace()
            .skip(1)
            .filter_map(|n| n.parse().ok())
            .collect();
        assert!(
            busy.len() == 2 && busy.iter().all(|&j| j > 0),
            "{ram:?}: {cpus}"
        );
    }
}
