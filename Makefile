# vmuc developer entry points. Keep targets cheap-first; AGENTS.md
# documents the validation ladder they implement.

SHELL := /bin/bash
.DEFAULT_GOAL := check

CARGO ?= cargo
VMUC_MAC ?= $(shell cat .vmuc-mac 2>/dev/null)
MAC_DIR ?= ~/vmuc

.PHONY: fmt clippy test check-darwin check kernel kernel-test agent busybox \
        e2e bench-local mac-sync mac-test mac-e2e bench sign clean

fmt:
	$(CARGO) fmt --all --check

clippy:
	$(CARGO) clippy --workspace --all-targets -- -D warnings

test:
	$(CARGO) test --workspace

check-darwin:
	$(CARGO) check --workspace --all-targets --target aarch64-apple-darwin

## The ladder, cheapest first.
check: fmt clippy test check-darwin

## Guest kernel: cross-build on Linux, boot under QEMU TCG.
kernel:
	guest/kernel/build.sh

kernel-test: kernel
	guest/kernel/qemu-boot-test.sh

## Busybox initramfs for HV bring-up and boot benchmarks (macOS or Linux).
busybox:
	guest/kernel/initramfs/busybox.sh

## On an Apple silicon Mac: boot real VMs. The cargo runner signs test
## binaries with the hypervisor entitlement.
e2e:
	$(CARGO) test -p vmuc-e2e -- --ignored --test-threads=1

bench-local:
	@test -n "$(NAME)" || { echo "usage: make bench-local NAME=<tests/bench/NAME>"; exit 1; }
	tests/bench/$(NAME)/run.sh

## Guest agent: static musl binary for arm64.
agent:
	$(CARGO) build -p vmuc-agent --release --target aarch64-unknown-linux-musl

## Remote Mac targets. Set VMUC_MAC=user@host or write it to .vmuc-mac.
mac-sync:
	@test -n "$(VMUC_MAC)" || { echo "VMUC_MAC is not set"; exit 1; }
	rsync -az --delete --exclude target --exclude .git --exclude guest/kernel/build \
	      --exclude guest/kernel/linux-* ./ $(VMUC_MAC):$(MAC_DIR)/

mac-test: mac-sync
	ssh $(VMUC_MAC) 'cd $(MAC_DIR) && source ~/.cargo/env && cargo test --workspace && make sign'

mac-e2e: mac-sync
	ssh $(VMUC_MAC) 'cd $(MAC_DIR) && source ~/.cargo/env && make e2e'

bench: mac-sync
	@test -n "$(NAME)" || { echo "usage: make bench NAME=<tests/bench/NAME>"; exit 1; }
	ssh $(VMUC_MAC) 'cd $(MAC_DIR) && source ~/.cargo/env && tests/bench/$(NAME)/run.sh'

## Ad-hoc sign the binary with the hypervisor entitlement (macOS only).
sign:
	codesign --force --sign - --entitlements vmuc.entitlements target/debug/vmuc 2>/dev/null || true
	codesign --force --sign - --entitlements vmuc.entitlements target/release/vmuc 2>/dev/null || true

clean:
	$(CARGO) clean
	rm -rf guest/kernel/build
