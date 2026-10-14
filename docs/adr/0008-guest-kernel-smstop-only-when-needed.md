# ADR 0008 — The guest kernel issues SMSTOP only when there is something to stop

**Status:** accepted, 2026-10-14. It adds guest kernel patch 0003
(plan.md: "as few patches as possible, each with its ADR"). Evidence:
[guest crypto and SMSTOP](../experiments/2026-10-14-guest-crypto-and-smstop.md).

## Context

Hypervisor.framework gives a vCPU the host's ID registers, SME and SME2
included on an M4, and vmuc's kernel (arm64 defconfig) supports SME. With
SME present Linux issues SMSTOP SM on every syscall entry and SMSTOP on
every `kernel_neon_begin()`, without looking whether streaming mode or
ZA is on. On an M4 each SMSTOP, even one with nothing to stop, slows the
core by about 40 % for some 20 µs, on the host as much as in the guest.
The guest's syscall-heavy work ran 8–43 % slower for it and the
kernel's NEON crypto 25–30 % slower; upstream (master, 2026-10) does
the same.

The alternatives: hide SME from the guest (`arm64.nosme`, or clearing
the field from the VMM), which recovers the same time but takes SME and
SME2 from the guest's programs; or leave it.

## Decision

- Guest kernel patch 0003 makes `sme_smstop_sm()` and `sme_smstop()`
  read SVCR and issue the SMSTOP only when the bits it clears are set.
  An SMSTOP that changes neither bit has no architectural effect, so a
  task in streaming mode or with ZA on is stopped exactly as before.
- SME stays visible to the guest; the VMM sets no ID registers.
- A kernel upgrade keeps the patch until upstream checks SVCR itself; a
  kernel without it is measured with `tests/bench/smstop` before it
  ships.

## Consequences

- The guest's syscall-heavy work runs as it does with SME hidden, and
  its programs keep SME and SME2.
- A guest kernel that is not vmuc's (an image that brings its own) pays
  the cost whenever it supports SME.
- A program that really uses streaming mode or ZA still pays one SMSTOP
  per syscall made while they are on, as the architecture needs.
