// SPDX-License-Identifier: Apache-2.0
//! The arm64 crypto instructions in loops of eight independent chains, so
//! each loop runs at the instruction's throughput, plus a NEON add as the
//! control for clock speed. `run.py` builds this one file for the host and
//! for the guest; std's runtime detection (sysctl on macOS, the kernel's
//! HWCAPs on Linux) decides what runs. Prints `name Gop/s` per kernel (best
//! of five; an op is one instruction, or one round where named) or `name
//! absent`, then whether PSTATE.DIT can be set.

use std::arch::aarch64::*;
use std::arch::{asm, is_aarch64_feature_detected};
use std::hint::black_box;
use std::time::Instant;

const ITERS: u64 = 50_000_000;
const CHAINS: u64 = 8;

/// Runs `body` over eight chains of `ITERS` steps.
macro_rules! chains {
    ($init:expr, |$x:ident| $step:expr) => {{
        let mut s = [$init; CHAINS as usize];
        for _ in 0..black_box(ITERS) {
            for $x in &mut s {
                *$x = $step;
            }
        }
        black_box(s);
    }};
}

#[target_feature(enable = "neon")]
fn neon_add() {
    let k = vdupq_n_u64(black_box(3));
    chains!(vdupq_n_u64(1), |x| vaddq_u64(*x, k));
}

/// One op is a round: AESE then AESMC, the pair the core fuses.
#[target_feature(enable = "aes")]
fn aes() {
    let k = vdupq_n_u8(black_box(0x5a));
    chains!(vdupq_n_u8(1), |x| vaesmcq_u8(vaeseq_u8(*x, k)));
}

#[target_feature(enable = "aes")]
fn pmull() {
    let k = vreinterpretq_p64_u64(vdupq_n_u64(black_box(0x87)));
    chains!(k, |x| vreinterpretq_p64_p128(vmull_high_p64(*x, k)));
}

#[target_feature(enable = "sha2")]
fn sha1() {
    let w = vdupq_n_u32(black_box(5));
    let e = black_box(7);
    chains!(vdupq_n_u32(1), |x| vsha1cq_u32(*x, e, w));
}

#[target_feature(enable = "sha2")]
fn sha256() {
    let y = vdupq_n_u32(black_box(3));
    let w = vdupq_n_u32(black_box(5));
    chains!(vdupq_n_u32(1), |x| vsha256hq_u32(*x, y, w));
}

#[target_feature(enable = "sha3")]
fn sha512() {
    let b = vdupq_n_u64(black_box(3));
    let c = vdupq_n_u64(black_box(5));
    chains!(vdupq_n_u64(1), |x| vsha512hq_u64(*x, b, c));
}

/// One op is the four SHA-3 instructions in a row: EOR3, RAX1, XAR, BCAX.
#[target_feature(enable = "sha3")]
fn sha3() {
    let a = vdupq_n_u64(black_box(3));
    let b = vdupq_n_u64(black_box(5));
    chains!(vdupq_n_u64(1), |x| {
        vbcaxq_u64(vxarq_u64::<7>(vrax1q_u64(veor3q_u64(*x, a, b), a), b), a, b)
    });
}

/// One op is CRC32CX, eight bytes.
#[target_feature(enable = "crc")]
fn crc32c() {
    let d = black_box(0x0123_4567_89ab_cdef);
    chains!(1u32, |x| __crc32cd(*x, d));
}

/// Sets PSTATE.DIT and reads it back.
#[target_feature(enable = "dit")]
fn dit() -> bool {
    let v: u64;
    // SAFETY: DIT only changes timing guarantees; it is cleared again.
    unsafe { asm!("msr DIT, #1", "mrs {v}, DIT", "msr DIT, #0", v = out(reg) v) };
    v >> 24 & 1 == 1
}

fn best(f: impl Fn()) -> f64 {
    let ops = (ITERS * CHAINS) as f64;
    (0..5)
        .map(|_| {
            let t = Instant::now();
            f();
            ops / t.elapsed().as_nanos() as f64
        })
        .fold(0.0, f64::max)
}

fn main() {
    let have = [
        ("neon-add", is_aarch64_feature_detected!("neon")),
        ("aes", is_aarch64_feature_detected!("aes")),
        ("pmull", is_aarch64_feature_detected!("aes")),
        ("sha1", is_aarch64_feature_detected!("sha2")),
        ("sha256", is_aarch64_feature_detected!("sha2")),
        ("sha512", is_aarch64_feature_detected!("sha3")),
        ("sha3", is_aarch64_feature_detected!("sha3")),
        ("crc32c", is_aarch64_feature_detected!("crc")),
    ];
    for (name, present) in have {
        if !present {
            println!("{name} absent");
            continue;
        }
        // SAFETY: each kernel runs only when its features were detected.
        let rate = unsafe {
            match name {
                "neon-add" => best(|| neon_add()),
                "aes" => best(|| aes()),
                "pmull" => best(|| pmull()),
                "sha1" => best(|| sha1()),
                "sha256" => best(|| sha256()),
                "sha512" => best(|| sha512()),
                "sha3" => best(|| sha3()),
                _ => best(|| crc32c()),
            }
        };
        println!("{name} {rate:.2}");
    }
    if is_aarch64_feature_detected!("dit") {
        // SAFETY: DIT was detected.
        println!("dit {}", u8::from(unsafe { dit() }));
    } else {
        println!("dit absent");
    }
}
