# 2026-10-13 — Lost vCPU cancels, folded FSEvents, a 30 s networkd wait

Three root causes found while validating E5 (snapshot/restore) and
re-running E4's end-to-end test.

## `hv_vcpus_exit`'s cancel can be used up

**Symptom.** The new snapshot e2e hung in its pause once (the watchdog
stopped it after 30 s), and about one run in eight took 30 s instead
of 7.9 s, every step done by 6.7 s. The rare "host stop is prompt" flake
in the boot e2e (stop not within 30 s) looked the same.

**Cause.** A vCPU whose `hv_vcpu_run` returns for an MMIO exit at the
moment the controller calls `hv_vcpus_exit` does not get a second,
canceled exit: it handles the MMIO, re-enters the guest and idles in
WFI. The vCPU loop only looked at the stop flag on a canceled exit, so
the pause waited, or teardown's join waited until something else kicked
the vCPU (in the slow runs, the previous VM's 30 s watchdog, whose
`hv_vcpus_exit` reached the next VM's vCPUs through reused handle
numbers).

**Fix.** vCPUs check the stop and pause flags before every
`hv_vcpu_run`; a pause re-kicks the vCPUs that have not reported every
2 ms; teardown re-kicks every 2 ms until each vCPU thread has left. vCPU
handles are kept by index, so a re-kick reaches the right ones.
Snapshot e2e 8 of 8 at 7.9 s, boot e2e 10 of 10, full suite green.

## FSEvents folds a file's events across hundreds of milliseconds

**Symptom.** E4's inotify e2e, green when E4 landed, failed every time:
the guest saw `CREATE host.txt` but not the `MODIFY` from a second write
300 ms later.

**Cause.** A raw FSEvents dump from a second process showed one event
for `host.txt`, flagged created, modified and inode-metadata: FSEvents
had folded both writes into the creation's event. The watcher replayed a
created file as CREATE alone.

**Fix.** A file created with data is replayed as created, then
modified (as a local create-and-write raises CREATE, MODIFY,
CLOSE_WRITE), and a path created earlier in a batch counts as known.

## `systemd-networkd-wait-online --ipv4` waits for IPv6 too

**Symptom.** The same e2e took 41 s instead of 15: `cloud-final` spent
30 s and warned that eth0 had no IPv4 address, although the agent had
reported one 1.2 s after boot.

**Cause.** networkd showed eth0 as "routable (configuring)": the DHCPv4
lease was there, but the link never finished configuring (this LAN
advertises DHCPv6 that nobody answers), and wait-online waits for the
whole link even with `--ipv4`.

**Fix.** boot.sh polls for eth0's global IPv4 address and default route
itself, at most 30 s as before.

Also found: main's Linux CI was red after E4 (clippy on a glibc-only
lossless cast; a test that relied on inode numbers not being reused,
which Linux does at once). Both fixed on main; Linux unit tests now also
run in a Linux guest before a push, since musl cannot build vmuc-fs
(no `renameat2`).
