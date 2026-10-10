// SPDX-License-Identifier: Apache-2.0
//! vmuc guest agent.
//!
//! Runs as root inside the guest (systemd unit installed by cloud-init)
//! and serves [`vmuc_proto::AgentRequest`]s: one request per connection,
//! a thread per connection. The production listener is vsock port
//! [`vmuc_proto::AGENT_VSOCK_PORT`] accepting only the host CID; for the
//! QEMU harness and unit tests it can also listen on TCP or a unix
//! socket (`--listen tcp:0.0.0.0:7700`, `--listen unix:/run/vmuc.sock`).
//!
//! Linux-only. On other targets `main` just reports that, so the
//! workspace still cross-checks for `aarch64-apple-darwin`.

#[cfg(target_os = "linux")]
mod handlers;
#[cfg(target_os = "linux")]
mod listen;
#[cfg(target_os = "linux")]
mod serve;

#[cfg(target_os = "linux")]
fn main() -> anyhow::Result<()> {
    use std::io::Write;

    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut listen_spec = format!("vsock:{}", vmuc_proto::AGENT_VSOCK_PORT);
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--listen" => {
                i += 1;
                listen_spec.clone_from(
                    args.get(i)
                        .ok_or_else(|| anyhow::anyhow!("--listen needs a value"))?,
                );
            }
            "--version" | "-V" => {
                println!("vmuc-agent {}", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            "--help" | "-h" => {
                println!(
                    "vmuc-agent [--listen vsock:PORT|tcp:ADDR:PORT|unix:PATH]\n\
                     Guest side of vmuc. Default: vsock:{}",
                    vmuc_proto::AGENT_VSOCK_PORT
                );
                return Ok(());
            }
            other => anyhow::bail!("unknown argument {other}"),
        }
        i += 1;
    }

    let listener = listen::Listener::bind(&listen_spec)?;
    let _ = writeln!(
        std::io::stderr(),
        "vmuc-agent {} listening on {listen_spec}",
        env!("CARGO_PKG_VERSION")
    );
    serve::run(&listener)
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("vmuc-agent runs inside the Linux guest only");
    std::process::exit(2);
}
