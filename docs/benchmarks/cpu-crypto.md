# Crypto instructions in the guest

Whether the guest gets the host CPU's crypto instructions and uses them:
the same instruction kernels on the host and in the guest, the guest's
CPU features and the drivers its kernel picks, the kernel's drivers
through AF_ALG, and OpenSSL's `speed` on both. Probe:
`tests/bench/cpu-crypto/run.py` (an Ubuntu instance, 4 vCPUs, 4 GiB);
`VMUC_KERNEL` and `VMUC_KERNEL_CMDLINE` select the kernel and its
parameters. Each run appends a section.


### 2026-10-14, Apple M4 Pro, macOS 26.6.2, vmuc 5157264, guest kernel 6.18.55-vmuc

Guest CPU features: aes, pmull, sha1, sha2, sha3, sha512, crc32, dit, sm3 (absent), sm4 (absent), rng (absent).

Kernel's choice: aes → aes-ce, xts(aes) → xts-aes-ce, gcm(aes) → gcm-aes-ce, sha256 → sha256-lib, sha512 → sha512-lib, sha3-256 → sha3-256-ce, crc32c → crc32c-lib.

Instructions, Gop/s (same source on both; `dit` is 1 when PSTATE.DIT could be set):

|  | neon-add | aes | pmull | sha1 | sha256 | sha512 | sha3 | crc32c | dit |
|---|---|---|---|---|---|---|---|---|---|
| host | 17.72 | 11.61 | 12.01 | 1.12 | 2.25 | 2.25 | 3.81 | 4.50 | 1 |
| guest | 17.72 | 11.05 | 11.93 | 1.12 | 2.23 | 2.23 | 3.73 | 4.47 | 1 |

Guest kernel drivers through AF_ALG, GB/s:

| xts-aes-ce | xts-aes-neonbs | xts(ecb(aes-generic)) | ctr-aes-ce | gcm-aes-ce | gcm_base(ctr(aes-generic),ghash-generic) | sha256 | sha512 | sha3-256-ce | sha3-256-generic | crc32c |
|---|---|---|---|---|---|---|---|---|---|---|
| 4.10 | 0.37 | 0.35 | 6.50 | 4.11 | 0.25 | 2.30 | 1.32 | 0.80 | 0.69 | 17.45 |

OpenSSL `speed`, 16 KiB, GB/s:

|  | version | aes-128-gcm | aes-256-xts | sha256 | sha512 | sha3-256 | chacha20-poly1305 |
|---|---|---|---|---|---|---|---|
| host | 4.0.3 | 11.06 | 13.38 | 3.44 | 1.92 | 1.11 | 2.35 |
| host, `OPENSSL_armcap=0x87d` | 4.0.3 | 8.49 | 13.21 | 3.40 | 1.91 | 1.10 | 2.33 |
| guest | 3.0.13 | 8.53 | 13.28 | 3.41 | 1.91 | 1.09 | 2.31 |

### 2026-10-14, Apple M4 Pro, macOS 26.6.2, vmuc 5157264, guest kernel 6.18.55-vmuc arm64.nosme

Guest CPU features: aes, pmull, sha1, sha2, sha3, sha512, crc32, dit, sm3 (absent), sm4 (absent), rng (absent).

Kernel's choice: aes → aes-ce, xts(aes) → xts-aes-ce, gcm(aes) → gcm-aes-ce, sha256 → sha256-lib, sha512 → sha512-lib, sha3-256 → sha3-256-ce, crc32c → crc32c-lib.

Instructions, Gop/s (same source on both; `dit` is 1 when PSTATE.DIT could be set):

|  | neon-add | aes | pmull | sha1 | sha256 | sha512 | sha3 | crc32c | dit |
|---|---|---|---|---|---|---|---|---|---|
| host | 17.56 | 11.57 | 11.99 | 1.12 | 2.25 | 2.25 | 3.77 | 4.49 | 1 |
| guest | 17.51 | 11.09 | 11.94 | 1.12 | 2.23 | 2.23 | 3.69 | 4.46 | 1 |

Guest kernel drivers through AF_ALG, GB/s:

| xts-aes-ce | xts-aes-neonbs | xts(ecb(aes-generic)) | ctr-aes-ce | gcm-aes-ce | gcm_base(ctr(aes-generic),ghash-generic) | sha256 | sha512 | sha3-256-ce | sha3-256-generic | crc32c |
|---|---|---|---|---|---|---|---|---|---|---|
| 5.46 | 0.50 | 0.36 | 8.51 | 5.43 | 0.25 | 3.26 | 1.86 | 1.03 | 0.70 | 23.32 |

OpenSSL `speed`, 16 KiB, GB/s:

|  | version | aes-128-gcm | aes-256-xts | sha256 | sha512 | sha3-256 | chacha20-poly1305 |
|---|---|---|---|---|---|---|---|
| host | 4.0.3 | 10.89 | 13.24 | 3.41 | 1.91 | 1.10 | 2.33 |
| host, `OPENSSL_armcap=0x87d` | 4.0.3 | 8.34 | 13.24 | 3.42 | 1.93 | 1.09 | 2.33 |
| guest | 3.0.13 | 8.46 | 13.18 | 3.41 | 1.91 | 1.09 | 2.30 |

### 2026-10-14, Apple M4 Pro, macOS 26.6.2, vmuc 5157264, guest kernel 6.18.55-vmuc Image-smstop

Guest CPU features: aes, pmull, sha1, sha2, sha3, sha512, crc32, dit, sm3 (absent), sm4 (absent), rng (absent).

Kernel's choice: aes → aes-ce, xts(aes) → xts-aes-ce, gcm(aes) → gcm-aes-ce, sha256 → sha256-lib, sha512 → sha512-lib, sha3-256 → sha3-256-ce, crc32c → crc32c-lib.

Instructions, Gop/s (same source on both; `dit` is 1 when PSTATE.DIT could be set):

|  | neon-add | aes | pmull | sha1 | sha256 | sha512 | sha3 | crc32c | dit |
|---|---|---|---|---|---|---|---|---|---|
| host | 17.57 | 11.40 | 11.98 | 1.12 | 2.25 | 2.25 | 3.75 | 4.50 | 1 |
| guest | 17.09 | 11.15 | 11.91 | 1.12 | 2.24 | 2.24 | 3.74 | 4.48 | 1 |

Guest kernel drivers through AF_ALG, GB/s:

| xts-aes-ce | xts-aes-neonbs | xts(ecb(aes-generic)) | ctr-aes-ce | gcm-aes-ce | gcm_base(ctr(aes-generic),ghash-generic) | sha256 | sha512 | sha3-256-ce | sha3-256-generic | crc32c |
|---|---|---|---|---|---|---|---|---|---|---|
| 5.50 | 0.50 | 0.36 | 8.50 | 5.43 | 0.25 | 3.26 | 1.86 | 1.03 | 0.71 | 23.30 |

OpenSSL `speed`, 16 KiB, GB/s:

|  | version | aes-128-gcm | aes-256-xts | sha256 | sha512 | sha3-256 | chacha20-poly1305 |
|---|---|---|---|---|---|---|---|
| host | 4.0.3 | 10.87 | 13.22 | 3.41 | 1.93 | 1.10 | 2.30 |
| host, `OPENSSL_armcap=0x87d` | 4.0.3 | 8.24 | 13.19 | 3.38 | 1.91 | 1.09 | 2.29 |
| guest | 3.0.13 | 8.45 | 13.16 | 3.40 | 1.91 | 1.09 | 2.28 |
