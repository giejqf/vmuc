# SMSTOP and the guest's syscalls

With SME present, Linux issues an SMSTOP on every syscall entry and
every `kernel_neon_begin()`; on Apple M4 one slows the core for
microseconds even when it has nothing to stop. Each run times the
guest's syscall-heavy work (and gzip, a compute-bound control) and,
where SME is visible, a block of scalar work after each way of leaving
streaming mode on the host and in the guest. Probe:
`tests/bench/smstop/run.py` (an Ubuntu instance, 4 vCPUs, 4 GiB);
`VMUC_KERNEL` and `VMUC_KERNEL_CMDLINE` select the kernel and its
parameters. Each run appends a section.


### 2026-10-14, Apple M4 Pro, macOS 26.6.2, vmuc 5157264, guest kernel 6.18.55-vmuc, guest SME on

Guest work, ms (best of five):

| find-usr | dd-4k | tar-usr-lib | exec-1000 | python-50 | gzip-64m |
|---|---|---|---|---|---|
| 48.7 | 95.6 | 22.1 | 123.8 | 324.6 | 1022.8 |

SVCR after a syscall made in streaming mode: 0 (SM must be cleared); with ZA on: 2 (ZA kept, 2).

A block of scalar work after each mode, ns (best of seven):

| | steps | plain | smstop | smstop-sm | msr-svcr | checked |
|---|---|---|---|---|---|---|
| host | 256 | 156 | 224 | 224 | 156 | 157 |
| host | 1024 | 612 | 867 | 867 | 611 | 612 |
| host | 4096 | 2435 | 3440 | 3439 | 2436 | 2441 |
| host | 16384 | 9739 | 13176 | 13180 | 9733 | 9744 |
| host | 65536 | 38901 | 47546 | 47576 | 38908 | 38890 |

| | steps | plain | smstop | smstop-sm | msr-svcr | checked |
|---|---|---|---|---|---|---|
| guest | 256 | 157 | 222 | 223 | 158 | 157 |
| guest | 1024 | 615 | 868 | 868 | 615 | 616 |
| guest | 4096 | 2462 | 3441 | 3441 | 2449 | 2451 |
| guest | 16384 | 9784 | 13196 | 13188 | 9746 | 9763 |
| guest | 65536 | 38988 | 47533 | 47507 | 38951 | 39045 |

### 2026-10-14, Apple M4 Pro, macOS 26.6.2, vmuc 5157264, guest kernel 6.18.55-vmuc arm64.nosme, guest SME off

Guest work, ms (best of five):

| find-usr | dd-4k | tar-usr-lib | exec-1000 | python-50 | gzip-64m |
|---|---|---|---|---|---|
| 36.3 | 66.7 | 17.2 | 102.6 | 300.2 | 1014.3 |

A block of scalar work after each mode, ns (best of seven):

| | steps | plain | smstop | smstop-sm | msr-svcr | checked |
|---|---|---|---|---|---|---|
| host | 256 | 156 | 223 | 223 | 156 | 157 |
| host | 1024 | 622 | 866 | 867 | 613 | 619 |
| host | 4096 | 2478 | 3432 | 3436 | 2441 | 2437 |
| host | 16384 | 9725 | 13150 | 13158 | 9721 | 9726 |
| host | 65536 | 38934 | 47403 | 47412 | 38882 | 38923 |

### 2026-10-14, Apple M4 Pro, macOS 26.6.2, vmuc 5157264, guest kernel 6.18.55-vmuc Image-smstop, guest SME on

Guest work, ms (best of five):

| find-usr | dd-4k | tar-usr-lib | exec-1000 | python-50 | gzip-64m |
|---|---|---|---|---|---|
| 36.6 | 67.0 | 16.6 | 104.0 | 300.2 | 1014.3 |

SVCR after a syscall made in streaming mode: 0 (SM must be cleared); with ZA on: 2 (ZA kept, 2).

A block of scalar work after each mode, ns (best of seven):

| | steps | plain | smstop | smstop-sm | msr-svcr | checked |
|---|---|---|---|---|---|---|
| host | 256 | 156 | 224 | 224 | 156 | 157 |
| host | 1024 | 616 | 866 | 867 | 612 | 613 |
| host | 4096 | 2454 | 3435 | 3436 | 2436 | 2451 |
| host | 16384 | 9731 | 13132 | 13181 | 9743 | 9832 |
| host | 65536 | 39354 | 47472 | 47517 | 38962 | 39386 |

| | steps | plain | smstop | smstop-sm | msr-svcr | checked |
|---|---|---|---|---|---|---|
| guest | 256 | 158 | 222 | 223 | 157 | 158 |
| guest | 1024 | 614 | 869 | 869 | 614 | 616 |
| guest | 4096 | 2468 | 3447 | 3447 | 2474 | 2511 |
| guest | 16384 | 10035 | 13176 | 13169 | 9946 | 10011 |
| guest | 65536 | 40080 | 47625 | 47622 | 39745 | 40008 |
