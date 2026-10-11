// SPDX-License-Identifier: Apache-2.0
//! Helpers for end-to-end tests: guest artifacts, a console capture,
//! and a watchdog so a hung guest fails the test instead of the run.
//!
//! The tests boot real VMs, so they need Apple silicon, the hypervisor
//! entitlement (the cargo runner signs test binaries) and the guest
//! artifacts: `guest/kernel/out/Image` (from `make kernel` or the kernel
//! workflow's artifact) and `guest/kernel/out/busybox-initramfs.cpio`
//! (`guest/kernel/initramfs/busybox.sh`). They are `#[ignore]`d; run them
//! with `cargo test -p vmuc-e2e -- --ignored --test-threads=1`.

use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use vmuc_vmm::machine::Machine;
use vmuc_vmm::{ConsoleOut, VmExit};

/// One VM per process: tests that boot hold this.
pub static VM_LOCK: Mutex<()> = Mutex::new(());

pub fn vm_lock() -> MutexGuard<'static, ()> {
    VM_LOCK.lock().unwrap_or_else(PoisonError::into_inner)
}

/// `guest/kernel/out/NAME`, or a panic explaining how to produce it.
pub fn artifact(name: &str) -> Vec<u8> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../guest/kernel/out")
        .join(name);
    std::fs::read(&path).unwrap_or_else(|e| {
        panic!(
            "{}: {e}. Build the kernel (make kernel, or download the kernel \
             workflow artifact) and run guest/kernel/initramfs/busybox.sh",
            path.display()
        )
    })
}

/// Everything the guest printed, with a way to wait for a pattern.
#[derive(Clone, Default)]
pub struct Capture {
    inner: Arc<(Mutex<Vec<u8>>, Condvar)>,
}

impl Write for Capture {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        let (buf, cv) = &*self.inner;
        buf.lock()
            .unwrap_or_else(PoisonError::into_inner)
            .extend_from_slice(b);
        cv.notify_all();
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Capture {
    /// A console writer feeding this capture.
    pub fn console(&self) -> (ConsoleOut, std::thread::JoinHandle<()>) {
        ConsoleOut::spawn(vec![Box::new(self.clone())])
    }

    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.inner.0.lock().unwrap_or_else(PoisonError::into_inner))
            .into_owned()
    }

    /// Waits until the output contains `needle`; panics with the output so
    /// far after `timeout`.
    pub fn wait_for(&self, needle: &str, timeout: Duration) -> String {
        let deadline = Instant::now() + timeout;
        let (buf, cv) = &*self.inner;
        let mut b = buf.lock().unwrap_or_else(PoisonError::into_inner);
        loop {
            let text = String::from_utf8_lossy(&b);
            if let Some(at) = text.find(needle) {
                let line_end = text[at..].find('\n').map_or(text.len(), |n| at + n);
                return text[at..line_end].to_owned();
            }
            let now = Instant::now();
            assert!(
                now < deadline,
                "timed out waiting for {needle:?}; console:\n{text}"
            );
            b = cv
                .wait_timeout(b, deadline - now)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }
}

/// Stops the VM as crashed if it is still running after `after`.
pub fn watchdog(machine: &Arc<Machine>, after: Duration) {
    let m = Arc::clone(machine);
    std::thread::spawn(move || {
        std::thread::sleep(after);
        m.request_exit(VmExit::Crashed(format!(
            "watchdog: still running after {after:?}"
        )));
    });
}
