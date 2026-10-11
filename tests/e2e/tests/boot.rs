// SPDX-License-Identifier: Apache-2.0
//! M2: the guest kernel boots to a busybox shell on Hypervisor.framework,
//! brings up secondary vCPUs through PSCI, and powers off, resets and
//! stops cleanly. Needs Apple silicon; see the crate docs.

use std::time::{Duration, Instant};

use vmuc_e2e::{Capture, artifact, vm_lock, watchdog};
use vmuc_vmm::{VmBuilder, VmConfig, VmExit};

const CMDLINE: &str = "console=ttyAMA0 panic=-1 quiet";

fn config(cpus: u32, test: &str) -> VmConfig {
    VmConfig {
        cpus,
        ram_size: 512 << 20,
        cmdline: format!("{CMDLINE} vmuc.test={test}"),
    }
}

#[test]
#[ignore = "boots a VM: Apple silicon only"]
fn smp_boot_to_init_and_power_off() {
    let _vm = vm_lock();
    let (kernel, initrd) = (artifact("Image"), artifact("busybox-initramfs.cpio"));
    let cap = Capture::default();
    let (out, writer) = cap.console();
    let started = Instant::now();
    let mut b = VmBuilder::new(config(4, "poweroff")).unwrap();
    b.add_platform_devices(out.clone()).unwrap();
    let vm = b.boot(&kernel, Some(&initrd)).unwrap();
    watchdog(vm.machine(), Duration::from_secs(20));
    assert_eq!(vm.wait(), VmExit::PowerOff);
    let elapsed = started.elapsed();
    out.close();
    writer.join().unwrap();
    let line = cap.wait_for("VMUC_INIT_OK", Duration::ZERO);
    assert!(line.contains("cpus=4"), "{line}");
    assert!(line.contains("kernel=6."), "{line}");
    eprintln!("{line} (power-on to power-off {elapsed:?})");
}

#[test]
#[ignore = "boots a VM: Apple silicon only"]
fn reset_exits_and_a_fresh_vm_boots_in_the_same_process() {
    let _vm = vm_lock();
    let (kernel, initrd) = (artifact("Image"), artifact("busybox-initramfs.cpio"));
    for (test, want) in [("reboot", VmExit::Reset), ("poweroff", VmExit::PowerOff)] {
        let cap = Capture::default();
        let (out, writer) = cap.console();
        let mut b = VmBuilder::new(config(2, test)).unwrap();
        b.add_platform_devices(out.clone()).unwrap();
        let vm = b.boot(&kernel, Some(&initrd)).unwrap();
        watchdog(vm.machine(), Duration::from_secs(20));
        assert_eq!(vm.wait(), want);
        out.close();
        writer.join().unwrap();
        cap.wait_for("VMUC_INIT_OK", Duration::ZERO);
    }
}

#[test]
#[ignore = "boots a VM: Apple silicon only"]
fn console_input_reaches_the_shell_and_host_stop_is_prompt() {
    let _vm = vm_lock();
    let (kernel, initrd) = (artifact("Image"), artifact("busybox-initramfs.cpio"));
    let cap = Capture::default();
    let (out, writer) = cap.console();
    let mut b = VmBuilder::new(VmConfig {
        cpus: 2,
        ram_size: 512 << 20,
        cmdline: CMDLINE.into(),
    })
    .unwrap();
    let uart = b.add_platform_devices(out.clone()).unwrap();
    let vm = b.boot(&kernel, Some(&initrd)).unwrap();
    watchdog(vm.machine(), Duration::from_secs(30));
    cap.wait_for("busybox shell", Duration::from_secs(10));
    uart.receive(b"echo ANSWER=$((6*7))\r");
    // The echoed command line shows `$((6*7))`; only the shell's
    // answer contains the digits.
    cap.wait_for("ANSWER=42", Duration::from_secs(5));
    let machine = std::sync::Arc::clone(vm.machine());
    let asked = Instant::now();
    machine.request_exit(VmExit::Stopped);
    assert_eq!(vm.wait(), VmExit::Stopped);
    assert!(
        asked.elapsed() < Duration::from_secs(1),
        "stop took {:?}",
        asked.elapsed()
    );
    out.close();
    writer.join().unwrap();
}
