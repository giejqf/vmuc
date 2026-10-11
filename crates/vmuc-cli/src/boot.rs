// SPDX-License-Identifier: Apache-2.0
//! `vmuc _boot`: direct-boot a kernel (and initrd) in the foreground with
//! the PL011 on the terminal. The bring-up and benchmark path; instances
//! go through `vmuc start` and `vmuc _vmm`.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;

use anyhow::Context;
use vmuc_virtio::devices::blk::{Block, BlockConfig};
use vmuc_virtio::devices::console::{Console, ConsoleInput};
use vmuc_virtio::devices::rng::Rng;
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
    /// Block devices in slot order (`/dev/vda` first), `true` = read-only.
    pub disks: Vec<(PathBuf, bool)>,
    /// Add a virtio-console and send the keyboard there instead of the UART.
    pub hvc: bool,
}

/// Where keyboard input goes.
enum Keyboard {
    Uart(Arc<Pl011>),
    Hvc(ConsoleInput),
}

/// Whatever the stdin pump should talk to; swapped on every reboot.
#[derive(Default)]
struct Target {
    keyboard: Option<Keyboard>,
    machine: Option<Arc<Machine>>,
}

/// `PATH` or `PATH:ro`.
pub fn parse_disk(spec: &str) -> (PathBuf, bool) {
    match spec.strip_suffix(":ro") {
        Some(path) => (PathBuf::from(path), true),
        None => (PathBuf::from(spec), false),
    }
}

fn add_devices(
    b: &mut VmBuilder,
    args: &BootArgs,
    out: &ConsoleOut,
) -> anyhow::Result<Option<ConsoleInput>> {
    for (i, (path, read_only)) in args.disks.iter().enumerate() {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(!read_only)
            .open(path)
            .with_context(|| path.display().to_string())?;
        let cfg = BlockConfig {
            read_only: *read_only,
            serial: format!("vmuc-disk{i}"),
        };
        b.add_virtio(Box::new(Block::new(file, &cfg)?))?;
    }
    b.add_virtio(Box::new(Rng::new()?))?;
    if !args.hvc {
        return Ok(None);
    }
    let console = Console::new(Arc::new(out.clone()))?;
    let input = console.input();
    b.add_virtio(Box::new(console))?;
    Ok(Some(input))
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
                        match &t.keyboard {
                            Some(Keyboard::Uart(uart)) => uart.receive(bytes),
                            Some(Keyboard::Hvc(hvc)) => hvc.send(bytes),
                            None => {}
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
        let hvc = add_devices(&mut builder, args, &out)?;
        let vm = builder.boot(&kernel, initrd.as_deref())?;
        tracing::info!(setup = ?started.elapsed(), "VM started");
        *target.lock().unwrap_or_else(PoisonError::into_inner) = Target {
            keyboard: Some(hvc.map_or(Keyboard::Uart(uart), Keyboard::Hvc)),
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
