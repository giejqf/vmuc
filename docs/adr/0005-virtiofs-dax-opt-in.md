# ADR 0005 — virtiofs DAX, per share, opt-in

**Status:** accepted, 2026-10-13. It changes the "File sharing" row of
plan.md's locked decisions ("no DAX in v0.1"; updated). Evidence:
[E2](../experiments/2026-10-13-e2-virtiofs-dax.md).

## Context

Without DAX every file the guest reads from a share is copied into
guest RAM: billed to the VM until the guest drops it, and held twice
while it lasts (guest page cache and macOS's file cache). E2 built DAX
on Hypervisor.framework and measured it: what the guest reads stays in
macOS's file cache (2 GiB read, +0 MiB of footprint against +1820),
cold reads are as fast as the copy path with read-ahead, and small files
and metadata are unchanged within a few percent.

DAX also moves file access onto vCPUs: a guest touching a mapped page
faults inside `hv_vcpu_run` until macOS has the page. For local disks
that is a page-cache miss; for a dataless iCloud placeholder (excluded)
or a slow or hung network filesystem under a share (not excluded) it
can stall a vCPU, and a stalled vCPU delays teardown. And a page past a
host truncation reads zero rather than raising SIGBUS in the guest.

## Decision

- `mounts[].dax` (default `false`) gives a share a 1 GiB DAX window,
  exposed as the virtio-fs device's shared memory region 0, and mounts it
  with `dax=inode`.
- The server offers DAX per file: regular files this user can write
  (mapped shared, so host and guest stay coherent) that are not dataless.
  Everything else takes the copy path.
- Each range is mapped with its own `hv_vm_map`, never over a live
  mapping, with `MADV_WILLNEED` read-ahead.

## Consequences

- Shares that hold large data a guest reads (datasets, build outputs,
  images) can cost the VM nothing to read; the default stays as before.
- Making DAX the default is a separate decision: it needs a guard against
  slow host filesystems (or a list of local volumes). The path is covered
  end to end (`tests/e2e/tests/dax.rs`).
- plan.md's "File sharing" row reads "own virtiofs; DAX per share,
  opt-in".
