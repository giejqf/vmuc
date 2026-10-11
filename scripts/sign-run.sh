#!/bin/sh
# SPDX-License-Identifier: Apache-2.0
#
# Cargo runner for aarch64-apple-darwin: ad-hoc sign the binary with the
# hypervisor entitlement, then exec it. Lets `cargo test` and `cargo run`
# exercise Hypervisor.framework without a separate signing step.
set -e
here=$(cd "$(dirname "$0")/.." && pwd)
codesign --force --sign - --entitlements "$here/vmuc.entitlements" "$1" 2>/dev/null
exec "$@"
