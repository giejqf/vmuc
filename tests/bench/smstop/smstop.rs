// SPDX-License-Identifier: Apache-2.0
//! The time a block of work takes after each way of leaving streaming mode,
//! with PSTATE.SM and PSTATE.ZA already clear (what Linux does on every
//! syscall entry and kernel_neon_begin() when SME is present). `run.py`
//! builds this one file for the host and for the guest and runs it only
//! where the CPU reports SME (the `sme` target feature is unstable, so the
//! SME instructions are spelled as encodings). Prints one line per block
//! size: the size in loop steps, then ns per block for each mode. With
//! `--abi` (Linux only) it prints SVCR after a syscall made in streaming
//! mode and after one made with ZA on: the kernel must clear SM and keep ZA.

use std::arch::asm;
use std::hint::black_box;
use std::time::Instant;

const MODES: [&str; 5] = ["plain", "smstop", "smstop-sm", "msr-svcr", "checked"];

/// Scalar work: eight multiply chains held in registers, so both targets
/// compile it alike.
#[inline(never)]
fn work(n: u64) -> u64 {
    let k = black_box(0x9e37_79b9_7f4a_7c15_u64);
    let mut a = [1u64, 2, 3, 4, 5, 6, 7, 8];
    for _ in 0..n {
        for x in &mut a {
            *x = x.wrapping_mul(k);
        }
    }
    a.iter().fold(0, |s, x| s ^ x)
}

/// One of MODES before the block: SMSTOP, SMSTOP SM, a register write of 0
/// to SVCR (S3_3_C4_C2_2), and SMSTOP SM only when SVCR.SM is set.
fn before(mode: usize) {
    // SAFETY: SME is present (run.py) and SM and ZA are clear (main), so
    // each of these leaves them so and touches no other state.
    unsafe {
        match mode {
            1 => asm!(".inst 0xd503467f"),
            2 => asm!(".inst 0xd503427f"),
            3 => asm!("msr S3_3_C4_C2_2, xzr"),
            4 => {
                let svcr: u64;
                asm!("mrs {0}, S3_3_C4_C2_2", out(reg) svcr);
                if svcr & 1 != 0 {
                    asm!(".inst 0xd503427f");
                }
            }
            _ => {}
        }
    }
}

fn time(steps: u64, mode: usize) -> f64 {
    let blocks = (50_000_000 / steps).max(200);
    (0..7)
        .map(|_| {
            let t = Instant::now();
            for _ in 0..blocks {
                before(mode);
                black_box(work(black_box(steps)));
            }
            t.elapsed().as_nanos() as f64 / blocks as f64
        })
        .fold(f64::MAX, f64::min)
}

/// SVCR after getpid called in streaming mode, then with ZA on.
#[cfg(target_os = "linux")]
fn abi() {
    let (sm, za): (u64, u64);
    // SAFETY: SME is present and enabled for the task (main). Nothing
    // between SMSTART and SMSTOP touches vector state.
    unsafe {
        asm!(
            ".inst 0xd503437f", // smstart sm
            "mov x8, #172",     // getpid
            "svc #0",
            "mrs {sm}, S3_3_C4_C2_2",
            ".inst 0xd503467f", // smstop
            ".inst 0xd503457f", // smstart za
            "mov x8, #172",
            "svc #0",
            "mrs {za}, S3_3_C4_C2_2",
            ".inst 0xd503467f",
            sm = out(reg) sm,
            za = out(reg) za,
            out("x0") _,
            out("x8") _,
        );
    }
    println!("svcr-after-syscall from-sm {sm} from-za {za}");
}

fn main() {
    // SAFETY: SME is present (run.py). SMSTART then SMSTOP: the first SME
    // use lets Linux enable it for the task, and SM and ZA end up clear.
    unsafe { asm!(".inst 0xd503477f", ".inst 0xd503467f") };
    #[cfg(target_os = "linux")]
    if std::env::args().any(|a| a == "--abi") {
        abi();
        return;
    }
    println!("steps {}", MODES.join(" "));
    for steps in [256, 1024, 4096, 16384, 65536] {
        let ns: Vec<String> = (0..MODES.len())
            .map(|m| format!("{:.0}", time(steps, m)))
            .collect();
        println!("{steps} {}", ns.join(" "));
    }
}
