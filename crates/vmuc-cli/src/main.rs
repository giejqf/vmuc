// SPDX-License-Identifier: Apache-2.0
//! The `vmuc` binary. One executable, several roles: the user-facing CLI,
//! the per-instance VMM (`vmuc _vmm NAME`) and the ssh ProxyCommand
//! bridge (`vmuc _proxy`). The hidden roles exist because the entitled
//! binary must be the process that owns the Hypervisor.framework VM.

mod agent_client;
mod assets;
mod boot;
mod control;
mod host;
mod instance;
mod keys;
mod lifecycle;
mod proxy;
mod state;
mod terminal;
mod vmm;

use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};
use vmuc_config::{TemplateStore, VmucHome};

#[derive(Parser)]
#[command(name = "vmuc", version, about = "ARM Linux VMs on Apple silicon")]
struct Cli {
    /// Log filter, e.g. `info` or `vmuc_vmm=debug`.
    #[arg(long, env = "VMUC_LOG", default_value = "warn", global = true)]
    log: String,
    #[command(subcommand)]
    command: Command,
}

#[derive(Clone, Copy, ValueEnum)]
enum Format {
    Table,
    Json,
}

#[derive(Subcommand)]
enum Command {
    /// Show versions, host capabilities and paths.
    Info {
        #[arg(long)]
        json: bool,
    },
    /// Create an instance from a template (`template:NAME`, a file, or a URL).
    Create {
        /// `ubuntu`, `template:docker`, `./my.yaml`, `https://…/x.yaml`.
        #[arg(default_value = "template:default")]
        template: String,
        /// Instance name; defaults to the template's name.
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        cpus: Option<u32>,
        /// e.g. `8GiB`.
        #[arg(long)]
        memory: Option<String>,
        /// e.g. `100GiB`.
        #[arg(long)]
        disk: Option<String>,
        /// `LOCATION[:w]`, may repeat.
        #[arg(long)]
        mount: Vec<String>,
    },
    /// Start an instance and wait until ssh works.
    Start {
        name: String,
        /// Seconds to wait for the guest to become ready.
        #[arg(long, default_value_t = 600)]
        timeout: u64,
    },
    /// Power an instance off (asks the guest; `--force` pulls the plug).
    Stop {
        name: String,
        #[arg(long, short)]
        force: bool,
    },
    /// Stop, then start.
    Restart {
        name: String,
        #[arg(long, default_value_t = 600)]
        timeout: u64,
    },
    /// Open a shell in an instance, or run a command there.
    Shell {
        name: String,
        /// Command to run instead of a login shell.
        #[arg(last = true)]
        command: Vec<String>,
    },
    /// List instances.
    #[command(alias = "ls")]
    List {
        #[arg(long, short, value_enum, default_value = "table")]
        format: Format,
        /// Names only.
        #[arg(short)]
        quiet: bool,
    },
    /// Delete an instance and its disk.
    #[command(alias = "rm")]
    Delete { name: String },
    /// List shipped and user templates.
    Template {
        #[command(subcommand)]
        cmd: TemplateCmd,
    },
    /// Direct-boot a kernel in the foreground, PL011 on the terminal
    /// (`Ctrl-A x` quits).
    #[command(name = "_boot", hide = true)]
    Boot {
        #[arg(long)]
        kernel: PathBuf,
        #[arg(long)]
        initrd: Option<PathBuf>,
        #[arg(
            long,
            default_value = "console=ttyAMA0 earlycon=pl011,0x09000000 panic=-1"
        )]
        cmdline: String,
        #[arg(long, default_value_t = 1)]
        cpus: u32,
        /// e.g. `1GiB`.
        #[arg(long, default_value = "1GiB")]
        memory: String,
        /// Block device `PATH` or `PATH:ro`, in slot order; may repeat.
        #[arg(long)]
        disk: Vec<String>,
        /// Add a virtio-console (hvc0) and send the keyboard there.
        #[arg(long)]
        hvc: bool,
    },
    /// Run the VMM for one instance (spawned by `vmuc start`).
    #[command(name = "_vmm", hide = true)]
    Vmm { name: String },
    /// ssh ProxyCommand: bridge stdio to a guest TCP port via the agent.
    #[command(name = "_proxy", hide = true)]
    Proxy {
        name: String,
        #[arg(default_value_t = 22)]
        port: u16,
        /// Reach the agent directly (harness): `tcp:HOST:PORT` or `unix:PATH`.
        #[arg(long)]
        via: Option<String>,
    },
    /// Render a fresh cloud-init seed (`cidata.iso`) for one boot.
    #[command(name = "_seed", hide = true)]
    Seed {
        name: String,
        /// Agent binary to ship in the seed.
        #[arg(long)]
        agent: Option<PathBuf>,
        /// Leave virtiofs mounts out (no virtiofs device present).
        #[arg(long)]
        no_mounts: bool,
    },
    /// Print the boot parameters for an instance as JSON.
    #[command(name = "_bootinfo", hide = true)]
    BootInfo { name: String },
    /// Call one agent method: `vmuc _agent tcp:127.0.0.1:17700 mem_info`.
    #[command(name = "_agent", hide = true)]
    Agent {
        via: String,
        method: String,
        params: Option<String>,
    },
}

#[derive(Subcommand)]
enum TemplateCmd {
    List,
    /// Print a template after flattening its `base:` chain.
    Show {
        name: String,
    },
}

#[allow(clippy::too_many_lines)]
fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new(&cli.log))
        .with_writer(std::io::stderr)
        .init();

    let home = VmucHome::detect()?;
    let store = TemplateStore::new(&home, installed_templates().as_deref());

    match cli.command {
        Command::Info { json } => {
            let info = host::HostInfo::collect();
            if json {
                println!("{}", serde_json::to_string_pretty(&info.to_json())?);
            } else {
                print!("{info}");
                println!("home:        {}", home.root().display());
            }
        }
        Command::Create {
            template,
            name,
            cpus,
            memory,
            disk,
            mount,
        } => {
            let overrides = instance::CreateOverrides {
                cpus,
                memory,
                disk,
                mounts: mount,
            };
            let template = normalize_locator(&template);
            let inst = instance::create(&home, &store, &template, name.as_deref(), &overrides)?;
            println!("created {} in {}", inst.name, inst.dir.display());
        }
        Command::List { format, quiet } => {
            let rows = instance::list(&home)?;
            match format {
                _ if quiet => rows.iter().for_each(|r| println!("{}", r.name)),
                Format::Json => println!("{}", serde_json::to_string_pretty(&rows)?),
                Format::Table => {
                    println!(
                        "{:<16} {:<8} {:>4} {:>8} {:>8}  DIR",
                        "NAME", "STATUS", "CPUS", "MEMORY", "DISK"
                    );
                    for r in &rows {
                        println!(
                            "{:<16} {:<8} {:>4} {:>8} {:>8}  {}",
                            r.name,
                            r.status,
                            r.cpus,
                            r.memory,
                            r.disk,
                            r.dir.display()
                        );
                    }
                }
            }
        }
        Command::Delete { name } => {
            if state::running_pid(&home.instance_dir(&name)?).is_some() {
                anyhow::bail!("{name} is running; `vmuc stop {name}` first");
            }
            instance::delete(&home, &name)?;
            println!("deleted {name}");
        }
        Command::Template { cmd } => match cmd {
            TemplateCmd::List => store.names().iter().for_each(|n| println!("{n}")),
            TemplateCmd::Show { name } => {
                let vmuc_config::Resolution::Loaded(t) =
                    store.resolve(&normalize_locator(&name), None)?
                else {
                    anyhow::bail!("{name} is a URL; download it first");
                };
                print!("{}", store.embed(&t)?.to_yaml()?);
            }
        },
        Command::Boot {
            kernel,
            initrd,
            cmdline,
            cpus,
            memory,
            disk,
            hvc,
        } => {
            let memory: vmuc_config::ByteSize = memory.parse()?;
            let exit = boot::run(&boot::BootArgs {
                kernel,
                initrd,
                cmdline,
                cpus,
                memory: memory.0,
                disks: disk.iter().map(|d| boot::parse_disk(d)).collect(),
                hvc,
            })?;
            eprintln!("vmuc: guest exited: {exit:?}");
            if let vmuc_vmm::VmExit::Crashed(_) = exit {
                std::process::exit(1);
            }
        }
        Command::Start { name, timeout } => {
            lifecycle::start(&home, &name, std::time::Duration::from_secs(timeout))?;
        }
        Command::Stop { name, force } => lifecycle::stop(&home, &name, force)?,
        Command::Restart { name, timeout } => {
            lifecycle::stop(&home, &name, false)?;
            lifecycle::start(&home, &name, std::time::Duration::from_secs(timeout))?;
        }
        Command::Shell { name, command } => lifecycle::shell(&home, &name, &command)?,
        Command::Vmm { name } => vmm::run(&home, &name)?,
        Command::Proxy { name, port, via } => {
            let conn: Box<dyn agent_client::AgentConn> = match via {
                Some(via) => agent_client::connect(&via)?,
                None => Box::new(control::vsock_connect(
                    &home.instance_dir(&name)?,
                    vmuc_proto::AGENT_VSOCK_PORT,
                )?),
            };
            proxy::run(conn, port)?;
        }
        Command::Seed {
            name,
            agent,
            no_mounts,
        } => {
            let inst = instance::Instance::open(&home, &name)?;
            let resolved = inst.resolved(&home)?;
            let boot_id =
                instance::write_seed(&home, &inst, &resolved, agent.as_deref(), !no_mounts)?;
            println!(
                "{}",
                serde_json::json!({ "bootId": boot_id, "seed": inst.seed() })
            );
        }
        Command::BootInfo { name } => {
            let inst = instance::Instance::open(&home, &name)?;
            let resolved = inst.resolved(&home)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&instance::boot_info(&inst, &resolved)?)?
            );
        }
        Command::Agent {
            via,
            method,
            params,
        } => {
            let req = agent_client::parse_request(&method, params.as_deref())?;
            let resp = agent_client::call(&via, &req)?;
            println!("{}", serde_json::to_string_pretty(&resp)?);
            if matches!(resp, vmuc_proto::AgentResponse::Error { .. }) {
                std::process::exit(1);
            }
        }
    }
    Ok(())
}

/// `ubuntu` means `template:ubuntu` unless it names an existing file or
/// carries a scheme; `vmuc create ubuntu` should just work.
fn normalize_locator(input: &str) -> String {
    let has_scheme = input.contains(':');
    let yaml_ext = std::path::Path::new(input)
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("yaml") || e.eq_ignore_ascii_case("yml"));
    let looks_like_path = input.contains('/') || yaml_ext;
    if input == "-" || has_scheme || looks_like_path || std::path::Path::new(input).exists() {
        input.to_owned()
    } else {
        format!("template:{input}")
    }
}

/// `share/vmuc/templates` next to the binary's prefix (Homebrew), else the
/// embedded copies alone.
fn installed_templates() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let prefix = exe.parent()?.parent()?;
    let dir = prefix.join("share/vmuc/templates");
    dir.is_dir().then_some(dir)
}

#[cfg(test)]
mod tests {
    #[test]
    fn bare_names_become_templates() {
        assert_eq!(super::normalize_locator("ubuntu"), "template:ubuntu");
        assert_eq!(
            super::normalize_locator("template:docker"),
            "template:docker"
        );
        assert_eq!(super::normalize_locator("./x.yaml"), "./x.yaml");
        assert_eq!(
            super::normalize_locator("https://a/b.yaml"),
            "https://a/b.yaml"
        );
        assert_eq!(super::normalize_locator("-"), "-");
    }
}
