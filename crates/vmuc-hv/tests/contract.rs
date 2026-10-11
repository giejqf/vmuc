// SPDX-License-Identifier: Apache-2.0
//! Hypervisor.framework contract probes.
//!
//! The exit-handling rules in AGENTS.md decide whether a guest boots or
//! wedges silently, and they are Apple's behaviour, not ours. This test
//! runs a page of hand-assembled instructions on a real vCPU (with the
//! in-kernel GIC, as the VMM uses it) and checks each rule, so a macOS
//! update that changes one fails here instead of in a boot.
//!
//! Needs the hypervisor entitlement (the cargo runner signs test
//! binaries on macOS) and real hardware; it passes with a note where
//! Hypervisor.framework is unavailable (Linux, hosted CI runners).

use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use vmuc_hv::{
    Gic, GicConfig, HvError, HvVcpu, HvVm, MemoryPermission, ReservedIntid, VcpuExit, reg, sys_reg,
};

const RAM: u64 = 0x4000_0000;
const RAM_SIZE: usize = 0x4000;
/// Unmapped, so accesses become data aborts.
const MMIO: u64 = 0x0900_0000;
/// EL1h with D, A, I and F masked.
const CPSR_EL1H_MASKED: u64 = 0x3c5;

const HVC_0: u32 = 0xd400_0002;
const SMC_0: u32 = 0xd400_0003;
const BRK_0: u32 = 0xd420_0000;
const WFI: u32 = 0xd503_207f;
/// `str w1, [x2]`
const STR_W1_X2: u32 = 0xb900_0041;
/// `ldr w3, [x2]`
const LDR_W3_X2: u32 = 0xb940_0043;
/// `msr osdlr_el1, x0` (op0=2 op1=0 CRn=1 CRm=3 op2=4)
const MSR_OSDLR_X0: u32 = 0xd510_1380;

/// Each probe: offset in the code page, the instruction under test.
const PROBES: [(u64, u32); 6] = [
    (0x000, HVC_0),
    (0x100, SMC_0),
    (0x200, STR_W1_X2),
    (0x300, LDR_W3_X2),
    (0x400, MSR_OSDLR_X0),
    (0x500, WFI),
];

#[derive(Debug)]
struct Observed {
    exit: String,
    class: Option<u8>,
    pc_after: u64,
}

fn describe(exit: &VcpuExit) -> (String, Option<u8>) {
    match exit {
        VcpuExit::Exception { syndrome, class } => {
            (format!("{class:?}"), Some(((syndrome >> 26) & 0x3f) as u8))
        }
        other => (format!("{other:?}"), None),
    }
}

/// GIC geometry and reserved interrupts as this host reports them.
#[derive(Debug)]
struct Geometry {
    max_vcpus: u32,
    gicd: (usize, usize),
    gicr: (usize, usize),
    vtimer: u32,
    ptimer: u32,
}

fn run_probes() -> Result<(Geometry, Vec<Observed>), HvError> {
    let vm = HvVm::new()?;
    let geometry = Geometry {
        max_vcpus: HvVm::max_vcpu_count()?,
        gicd: Gic::distributor_geometry()?,
        gicr: Gic::redistributor_geometry()?,
        vtimer: Gic::reserved_intid(ReservedIntid::El1VirtualTimer)?,
        ptimer: Gic::reserved_intid(ReservedIntid::El1PhysicalTimer)?,
    };
    eprintln!("geometry: {geometry:x?}");
    let _gic = Gic::new(GicConfig {
        distributor_base: 0x0800_0000,
        redistributor_base: 0x0c00_0000,
    })?;

    // SAFETY: fresh anonymous mapping, checked below.
    let mem = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            RAM_SIZE,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_ANON | libc::MAP_PRIVATE,
            -1,
            0,
        )
    };
    assert_ne!(mem, libc::MAP_FAILED);
    let code = mem.cast::<u32>();
    for (off, insn) in PROBES {
        // SAFETY: offsets are within the 16 KiB page.
        unsafe {
            code.add(off as usize / 4).write(insn);
            code.add(off as usize / 4 + 1).write(BRK_0);
        }
    }
    // SAFETY: `mem` stays mapped until after `hv_vm_destroy` (drop of `vm`).
    unsafe { vm.map_memory(mem.cast(), RAM, RAM_SIZE, MemoryPermission::ALL)? };

    let (id_tx, id_rx) = mpsc::channel();
    let worker = thread::spawn(move || -> Result<Vec<Observed>, HvError> {
        let vcpu = HvVcpu::new()?;
        vcpu.set_sys_reg(sys_reg::HV_SYS_REG_MPIDR_EL1, 0)?;
        id_tx.send(vcpu.id()).ok();
        let mut seen = Vec::new();
        for (off, insn) in PROBES {
            vcpu.set_reg(reg::HV_REG_PC, RAM + off)?;
            vcpu.set_reg(reg::HV_REG_CPSR, CPSR_EL1H_MASKED)?;
            vcpu.set_reg(reg::HV_REG_X0, 0)?;
            vcpu.set_reg(reg::HV_REG_X1, 0x1234_5678)?;
            vcpu.set_reg(reg::HV_REG_X2, MMIO)?;
            let started = Instant::now();
            let exit = vcpu.run()?;
            let (exit, class) = describe(&exit);
            let pc_after = vcpu.get_reg(reg::HV_REG_PC)? - (RAM + off);
            eprintln!(
                "probe {insn:#010x}: {exit} pc+{pc_after} after {:?}",
                started.elapsed()
            );
            seen.push(Observed {
                exit,
                class,
                pc_after,
            });
        }
        Ok(seen)
    });

    // The WFI probe is last: if the framework idles inside hv_vcpu_run
    // with the GIC present, only hv_vcpus_exit gets it out.
    let id = id_rx
        .recv()
        .expect("vCPU thread died before creating its vCPU");
    let kicker = thread::spawn(move || {
        thread::sleep(Duration::from_millis(300));
        HvVcpu::force_exit(&[id]).expect("hv_vcpus_exit");
    });
    let seen = worker.join().expect("vCPU thread panicked")?;
    kicker.join().expect("kicker panicked");
    drop(vm);
    // SAFETY: the VM is destroyed, so nothing maps `mem` any more.
    unsafe { libc::munmap(mem, RAM_SIZE) };
    Ok((geometry, seen))
}

#[test]
fn exit_handling_rules_hold() {
    let (geometry, seen) = match run_probes() {
        Err(HvError::Unsupported | HvError::Denied) => {
            eprintln!("Hypervisor.framework unavailable here; contract probes skipped");
            return;
        }
        other => other.expect("probe run failed"),
    };
    // The VMM's guest physical map assumes these (vmuc-vmm layout.rs):
    // a 64 KiB distributor and a redistributor region that fits in the
    // 64 MiB window at 0x0c00_0000, and the architectural timer PPIs.
    assert_eq!(geometry.gicd.0, 0x1_0000, "{geometry:x?}");
    assert!(geometry.gicr.0 <= 0x400_0000, "{geometry:x?}");
    assert!(0x0c00_0000 % geometry.gicr.1 == 0, "{geometry:x?}");
    assert_eq!((geometry.vtimer, geometry.ptimer), (27, 30));
    assert!(geometry.max_vcpus >= 4, "{geometry:x?}");

    let [hvc, smc, str_, ldr, msr, wfi] = &seen[..] else {
        panic!("expected six observations, got {seen:?}");
    };

    // HVC: the framework advances PC past the HVC itself.
    assert!(hvc.exit.starts_with("HypercallHvc"), "{hvc:?}");
    assert_eq!(hvc.pc_after, 4, "HVC must leave PC advanced");

    // SMC: trapped, preferred return address is the SMC itself, so the
    // VMM must advance PC (ARM ARM; QEMU's HVF backend does the same).
    assert!(smc.exit.starts_with("SmcCall"), "{smc:?}");
    assert_eq!(smc.pc_after, 0, "SMC must leave PC on the SMC");

    // Data aborts (MMIO): PC stays on the faulting instruction.
    for da in [str_, ldr] {
        assert_eq!(da.class, Some(0x24), "{da:?}");
        assert_eq!(da.pc_after, 0, "data abort must leave PC unadvanced");
    }
    assert!(str_.exit.contains("is_write: true"), "{str_:?}");
    assert!(ldr.exit.contains("is_write: false"), "{ldr:?}");

    // OSDLR_EL1 writes trap to the VMM (EC 0x18) with PC unadvanced.
    assert!(
        msr.exit.starts_with("SystemRegister"),
        "OSDLR_EL1 write did not trap: {msr:?}"
    );
    assert_eq!(msr.pc_after, 0);

    // WFI with the in-kernel GIC idles inside hv_vcpu_run: nothing comes
    // back until an interrupt or hv_vcpus_exit, and the WFI is retired.
    assert_eq!(wfi.exit, "Canceled", "{wfi:?}");
    assert_eq!(wfi.pc_after, 4, "{wfi:?}");
}
