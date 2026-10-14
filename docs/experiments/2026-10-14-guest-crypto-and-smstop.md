# 2026-10-14 — Crypto instructions in the guest, and what SMSTOP costs

**Questions.** Does the guest get the host CPU's crypto instructions,
and are they used properly: by the guest's programs, by its kernel, at
the host's speed?

**Method.** `tests/bench/cpu-crypto` ([cpu-crypto](../benchmarks/cpu-crypto.md)):
one file of instruction kernels (AES rounds, PMULL, SHA-1, SHA-256,
SHA-512, the SHA-3 instructions, CRC32C, a NEON add as the clock
control, and whether PSTATE.DIT can be set) built for macOS and for
Linux from the same source and run on the host and in the guest; the
guest's `/proc/cpuinfo` features and the drivers its kernel picked
(`/proc/crypto`); the kernel's drivers timed through AF_ALG, with
driver names pinning the CE, NEON and generic implementations; OpenSSL's
`speed` in the guest (Ubuntu's 3.0.13) and on the host (Homebrew's
4.0.3, also restricted to the paths it picks on Linux). Then
`tests/bench/smstop` ([smstop](../benchmarks/smstop.md)): a block of
scalar work after each way of leaving streaming mode, on the host and in
the guest, and the guest's syscall-heavy work, each on the stock kernel,
with `arm64.nosme`, and with patch 0003. Ubuntu 24.04, 4 vCPUs, 4 GiB,
guest kernel 6.18.55; M4 Pro, macOS 26.6.2. Lima 2.2.1 on VZ for the
features VZ shows.

**Result: the instructions.**

- vmuc sets no ID registers; Hypervisor.framework's defaults give the
  guest every crypto feature the M4 has: aes, pmull, sha1, sha2, sha3,
  sha512, crc32, dit. The M4 has no SM3, SM4 or RNDR, so neither has
  the guest. Read by hand: MIDR is `0x610f0000` (Apple, part 0), and a
  VZ guest (Lima) shows the same crypto features and the same MIDR.
- The same instruction kernels run at the host's rate in the guest
  (within 2 %, the NEON control included), and DIT can be set there.
- OpenSSL in the guest finds them (AT_HWCAP) and matches the host's
  OpenSSL on AES-XTS, SHA-256, SHA-512, SHA3-256 and ChaCha20-Poly1305.
  AES-128-GCM is 8.5 GB/s against 10.9–11.1: OpenSSL's 8-way unrolled
  GCM is enabled by brand string on macOS and on Linux only for the core
  IDs it lists, no Apple core among them (master, `crypto/armcap.c`).
  The host's OpenSSL restricted to that choice runs 8.2–8.5 GB/s, as the
  guest does. Passing the host's MIDR through would not change it.

**Result: the kernel.**

- The kernel picks the CE drivers (aes-ce, xts-aes-ce, gcm-aes-ce,
  sha3-256-ce; SHA-256, SHA-512 and CRC32C through the library, which
  uses the instructions), every one past its self-test. AES-XTS runs
  11–15× faster on xts-aes-ce than on the NEON or generic drivers.
- But with SME present, Linux issues SMSTOP SM on every syscall entry
  and SMSTOP on every `kernel_neon_begin()`, with or without anything
  to stop (6.18 and master alike). On an M4 one such SMSTOP slows the
  core for some 20 µs by about 40 %: 1.0 µs added to a 2.4 µs block of
  work after it, 8.6 µs to a 39 µs block. SMSTOP SM costs the same; a
  register write of 0 to SVCR and an SMSTOP behind a read of SVCR cost
  nothing. The host and the guest measure the same, block for block: it
  is the core, not the hypervisor (which does not trap SMSTOP either).

| Guest | find /usr | dd 4 KiB ×200k | tar /usr/lib | exec ×1000 | python ×50 | gzip (control) |
|---|---:|---:|---:|---:|---:|---:|
| stock kernel, ms | 48.7 | 95.6 | 22.1 | 123.8 | 324.6 | 1022.8 |
| `arm64.nosme` | 36.3 | 66.7 | 17.2 | 102.6 | 300.2 | 1014.3 |
| patch 0003 | 36.6 | 67.0 | 16.6 | 104.0 | 300.2 | 1014.3 |

| Kernel through AF_ALG, GB/s | xts-aes-ce | gcm-aes-ce | ctr-aes-ce | sha256 | sha512 | sha3-256-ce | crc32c |
|---|---:|---:|---:|---:|---:|---:|---:|
| stock kernel | 4.10 | 4.11 | 6.50 | 2.30 | 1.32 | 0.80 | 17.45 |
| `arm64.nosme` | 5.46 | 5.43 | 8.51 | 3.26 | 1.86 | 1.03 | 23.32 |
| patch 0003 | 5.50 | 5.43 | 8.50 | 3.26 | 1.86 | 1.03 | 23.30 |

The generic drivers, which use no NEON, are the same on all three. With
the patch the kernel's SHA-256 and SHA-512 reach OpenSSL's rate; its
AES-XTS stays at 5.5 against OpenSSL's 13.2 GB/s, the kernel's own
code (not measured further).

- With the patch SME stays visible and works: a syscall made in
  streaming mode still returns with SM cleared, one made with ZA on
  keeps ZA (`smstop --abi`, stock and patched alike).

**Not built.** The kernel has no `sha1` in its crypto API
(`CONFIG_CRYPTO_SHA1`), no ChaCha20-Poly1305 AEAD, CMAC, POLYVAL or
HCTR2. IPsec is built (`XFRM_USER`, ESP), so suites that name hmac(sha1)
or rfc7539esp would fail to install; the AES-GCM ones work. Not changed
here.

**Verdict.**

1. The host's crypto instructions reach the guest unchanged and run at
   the host's speed, in its programs and its kernel. Nothing to change
   in the VMM.
2. AES-GCM in Linux OpenSSL is 20 % below macOS OpenSSL on this CPU, by
   OpenSSL's own choice of code path; not vmuc's.
3. SME's unconditional SMSTOP made the guest's kernel crypto 25–30 %
   slower and its syscall-heavy work 8–43 %. Adopted: guest kernel patch
   0003 ([ADR 0008](../adr/0008-guest-kernel-smstop-only-when-needed.md)),
   which recovers all of it with SME still available.
