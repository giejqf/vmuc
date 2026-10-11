// SPDX-License-Identifier: Apache-2.0
//! `vmuc _boot`: direct-boot a kernel (and initrd) in the foreground with
//! the PL011 on the terminal. The bring-up and benchmark path; instances
//! go through `vmuc start` and `vmuc _vmm`.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;

use anyhow::Context;
use vmuc_vmm::devices::pl011::Pl011;
use vmuc_vmm::machine::Machine;
use vmuc_vmm::{ConsoleOut, VmBuilder, VmConfig, VmExit};

use crate::terminal::{RawMode, pump_stdin};

pub struct BootArgs {
    pub kernel: PathBuf,
    pub initrd: Option<PathBuf>,
    pub cmdline: String,
    pub cpus: u32,
    pub memory: u64,
}

/// Whatever the stdin pump should talk to; swapped on every reboot.
#[derive(Default)]
struct Target {
    uart: Option<Arc<Pl011>>,
    machine: Option<Arc<Machine>>,
}

/// Boots until the guest powers off or the user quits; a guest reset
/// boots a fresh VM in the same process. Returns the final exit.
pub fn run(args: &BootArgs) -> anyhow::Result<VmExit> {
    let kernel = std::fs::read(&args.kernel).with_context(|| args.kernel.display().to_string())?;
    let initrd = args
        .initrd
        .as_ref()
        .map(|p| std::fs::read(p).with_context(|| p.display().to_string()))
        .transpose()?;

    let (out, writer) = ConsoleOut::spawn(vec![Box::new(std::io::stdout())]);
    let target = Arc::new(Mutex::new(Target::default()));
    let _raw = RawMode::enable();
    {
        let (feed, quit) = (Arc::clone(&target), Arc::clone(&target));
        std::thread::Builder::new()
            .name("stdin".into())
            .spawn(move || {
                pump_stdin(
                    |bytes| {
                        let t = feed.lock().unwrap_or_else(PoisonError::into_inner);
                        if let Some(uart) = &t.uart {
                            uart.receive(bytes);
                        }
                    },
                    || {
                        let t = quit.lock().unwrap_or_else(PoisonError::into_inner);
                        if let Some(m) = &t.machine {
                            m.request_exit(VmExit::Stopped);
                        }
                    },
                );
            })?;
    }

    let exit = loop {
        let started = Instant::now();
        let mut builder = VmBuilder::new(VmConfig {
            cpus: args.cpus,
            ram_size: args.memory,
            cmdline: args.cmdline.clone(),
        })?;
        let uart = builder.add_platform_devices(out.clone())?;
        let vm = builder.boot(&kernel, initrd.as_deref())?;
        tracing::info!(setup = ?started.elapsed(), "VM started");
        *target.lock().unwrap_or_else(PoisonError::into_inner) = Target {
            uart: Some(uart),
            machine: Some(Arc::clone(vm.machine())),
        };
        let exit = vm.wait();
        *target.lock().unwrap_or_else(PoisonError::into_inner) = Target::default();
        if exit != VmExit::Reset {
            break exit;
        }
        out.push(b"\r\n[vmuc: guest reset, rebooting]\r\n");
    };
    out.close();
    let _ = writer.join();
    Ok(exit)
}
