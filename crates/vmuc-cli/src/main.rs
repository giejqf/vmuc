// SPDX-License-Identifier: Apache-2.0
//! The `vmuc` binary. One executable, several roles: the user-facing CLI,
//! the per-instance VMM (`vmuc _vmm NAME`) and the ssh ProxyCommand
//! bridge (`vmuc _proxy NAME PORT`). The hidden roles exist because the
//! entitled binary must be the process that owns the Hypervisor.framework
//! VM.

mod host;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "vmuc", version, about = "ARM Linux VMs on Apple silicon")]
struct Cli {
    /// Log filter, e.g. `info` or `vmuc_vmm=debug`.
    #[arg(long, env = "VMUC_LOG", default_value = "info", global = true)]
    log: String,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Show versions, host capabilities and paths.
    Info {
        /// Emit JSON instead of text.
        #[arg(long)]
        json: bool,
    },
    /// Run the VMM for one instance (spawned by `vmuc start`).
    #[command(name = "_vmm", hide = true)]
    Vmm { name: String },
    /// Bridge stdio to a guest TCP port over vsock (ssh ProxyCommand).
    #[command(name = "_proxy", hide = true)]
    Proxy { name: String, port: u16 },
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new(&cli.log))
        .with_writer(std::io::stderr)
        .init();

    match cli.command {
        Command::Info { json } => {
            let info = host::HostInfo::collect();
            if json {
                println!("{}", serde_json::to_string_pretty(&info.to_json())?);
            } else {
                print!("{info}");
            }
            Ok(())
        }
        Command::Vmm { name } => anyhow::bail!("`_vmm {name}`: the VMM arrives in milestone M2"),
        Command::Proxy { name, port } => {
            anyhow::bail!("`_proxy {name} {port}`: the vsock bridge arrives in milestone M4")
        }
    }
}
