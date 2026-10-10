// SPDX-License-Identifier: Apache-2.0
//! vmuc guest agent. Runs as a systemd unit inside the guest and talks to
//! the host over vsock. Skeleton; M1 fills it in.

fn main() {
    println!("vmuc-agent {} (skeleton)", env!("CARGO_PKG_VERSION"));
}
