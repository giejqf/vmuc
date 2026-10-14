// SPDX-License-Identifier: Apache-2.0
//! The `vmuc` binary. One executable, several roles: the user-facing CLI,
//! the per-instance VMM (`vmuc _vmm NAME`) and the ssh ProxyCommand
//! bridge (`vmuc _proxy`). The hidden roles exist because the entitled
//! binary must be the process that owns the Hypervisor.framework VM.

mod agent_client;
mod assets;
mod boot;
mod control;
mod copy;
mod duplicate;
mod exit;
mod host;
mod instance;
mod keys;
mod lifecycle;
mod proxy;
mod stat;
mod state;
mod suspend;
mod terminal;
mod tui;
mod vmm;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand, ValueEnum};
use vmuc_config::{TemplateStore, VmucHome};

#[derive(Parser)]
#[command(name = "vmuc", version, about = "ARM Linux VMs on Apple silicon")]
struct Cli {
    /// Log filter, e.g. `info` or `vmuc_vmm=debug`.
    #[arg(long, env = "VMUC_LOG", default_value = "warn", global = true)]
    log: String,
    /// Print results as JSON on stdout, failures as JSON on stderr.
    #[arg(
        long,
        env = "VMUC_JSON",
        global = true,
        action = clap::ArgAction::SetTrue,
        value_parser = clap::builder::BoolishValueParser::new()
    )]
    json: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Clone, Copy, ValueEnum)]
enum Format {
    Table,
    Json,
}

#[derive(Clone, Copy, ValueEnum)]
enum Level {
    Normal,
    Warn,
    Critical,
}

impl From<Level> for vmuc_proto::PressureLevel {
    fn from(l: Level) -> Self {
        match l {
            Level::Normal => Self::Normal,
            Level::Warn => Self::Warn,
            Level::Critical => Self::Critical,
        }
    }
}

#[derive(Subcommand)]
enum Command {
    /// Show versions, host capabilities and paths.
    Info,
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
        /// `path=value`, applied last, may repeat: `network.mode=bridged`,
        /// `mounts=[{location: ~/src, writable: true}]`.
        #[arg(long = "set", value_name = "PATH=VALUE")]
        set: Vec<String>,
    },
    /// Start an instance and wait until ssh works.
    Start {
        name: String,
        /// Seconds to wait for the guest to become ready.
        #[arg(long, default_value_t = 600)]
        timeout: u64,
        /// Stay attached: the VMM logs to this terminal and Ctrl-C powers
        /// the guest off.
        #[arg(long)]
        foreground: bool,
    },
    /// Power an instance off (asks the guest; `--force` pulls the plug,
    /// or discards a suspended state).
    Stop {
        name: String,
        #[arg(long, short)]
        force: bool,
    },
    /// Save a running instance and end its VM; `resume` (or `start`)
    /// brings it back with its programs as they were.
    Suspend { name: String },
    /// Resume a suspended instance; refuses one that is not suspended.
    Resume {
        name: String,
        /// Seconds to wait for the guest to become ready.
        #[arg(long, default_value_t = 600)]
        timeout: u64,
    },
    /// Copy a stopped instance under a new name: its configuration and its
    /// disk, cloned copy-on-write. The copy gets its own MAC, address,
    /// hostname and machine ID.
    #[command(alias = "clone")]
    Duplicate {
        source: String,
        name: String,
        #[arg(long)]
        cpus: Option<u32>,
        /// e.g. `8GiB`.
        #[arg(long)]
        memory: Option<String>,
        /// e.g. `200GiB`; a copy's disk can grow, not shrink.
        #[arg(long)]
        disk: Option<String>,
        /// `LOCATION[:w]`, may repeat; added to the source's mounts.
        #[arg(long)]
        mount: Vec<String>,
        /// `path=value`, applied last, may repeat.
        #[arg(long = "set", value_name = "PATH=VALUE")]
        set: Vec<String>,
        /// Start the copy once it is made.
        #[arg(long)]
        start: bool,
    },
    /// Stop, then start.
    Restart {
        name: String,
        #[arg(long, default_value_t = 600)]
        timeout: u64,
    },
    /// Print how to install the vmnet helper and its sudoers rule.
    Sudoers {
        /// Where the helper is (or will be) installed.
        #[arg(long)]
        helper: Option<PathBuf>,
    },
    /// Memory: host footprint, guest totals, reclaimed bytes.
    Stat {
        name: String,
        /// Refresh every second (with --json, one document per line).
        #[arg(long, short)]
        watch: bool,
    },
    /// Ask the guest to drop caches and compact so its free memory can be
    /// handed back now.
    Reclaim { name: String },
    /// Watch and drive every instance in a terminal UI.
    Tui,
    /// Copy files to or from an instance over ssh: `NAME:PATH` is a path
    /// in the instance, anything else is local.
    Copy {
        /// Copy directories recursively.
        #[arg(short, long)]
        recursive: bool,
        /// Sources, then the destination.
        #[arg(required = true, num_args = 2..)]
        paths: Vec<String>,
    },
    /// Edit an instance's configuration (checked before it is saved).
    Edit { name: String },
    /// Remove downloaded images from the cache (instances keep their own
    /// disks; a later create downloads again).
    Prune,
    /// Print a shell completion script: `vmuc completion zsh > _vmuc`.
    Completion { shell: clap_complete::Shell },
    /// Open a shell in an instance, or run a command there.
    Shell {
        name: String,
        /// Guest directory to start in. Default: the current directory when
        /// a share makes it visible in the guest, else the home directory.
        #[arg(long)]
        workdir: Option<String>,
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
    Delete {
        name: String,
        /// Stop it first (`stop --force`) if it is running.
        #[arg(long, short)]
        force: bool,
    },
    /// List shipped and user templates.
    Template {
        #[command(subcommand)]
        cmd: TemplateCmd,
    },
    /// Direct-boot a kernel in the foreground, PL011 on the terminal
    /// (`Ctrl-A x` quits).
    #[command(name = "_boot", hide = true)]
    Boot {
        /// Needed unless restoring.
        #[arg(long, required_unless_present = "restore")]
        kernel: Option<PathBuf>,
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
        /// Add the balloon (free page reporting).
        #[arg(long)]
        balloon: bool,
        /// Write a snapshot here after `--snapshot-after` seconds, then stop.
        #[arg(long, requires = "snapshot_after")]
        snapshot: Option<PathBuf>,
        #[arg(long, requires = "snapshot")]
        snapshot_after: Option<f64>,
        /// Resume from a snapshot instead of booting; the other options
        /// must describe the same machine.
        #[arg(long)]
        restore: Option<PathBuf>,
        /// `copy` guest RAM in, or `map` it copy-on-write.
        #[arg(long, default_value = "copy", value_parser = ["copy", "map"])]
        restore_ram: String,
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
    /// Make a running instance's memory manager act as if macOS reported
    /// this pressure level (experiments): `vmuc _pressure dev critical`.
    #[command(name = "_pressure", hide = true)]
    Pressure {
        name: String,
        level: Level,
        /// Seconds before the host's own level applies again.
        #[arg(long = "for", default_value_t = 600)]
        seconds: u32,
    },
    /// Call one agent method: `vmuc _agent tcp:127.0.0.1:17700 mem_info`.
    #[command(name = "_agent", hide = true)]
    Agent {
        via: String,
        method: String,
        params: Option<String>,
        /// Call it N times and print round-trip quantiles instead.
        #[arg(long)]
        repeat: Option<u32>,
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

fn main() -> ExitCode {
    let cli = Cli::parse();
    let json = cli.json;
    let program = matches!(cli.command, Command::Shell { .. });
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => exit::report(&e, json, program),
    }
}

#[allow(clippy::too_many_lines)]
fn run(cli: Cli) -> anyhow::Result<()> {
    let json = cli.json;
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new(&cli.log))
        .with_writer(std::io::stderr)
        .init();

    let home = VmucHome::detect()?;
    let store = TemplateStore::new(&home, installed_templates().as_deref());

    match cli.command {
        Command::Info => {
            let info = host::HostInfo::collect();
            let inventory = assets::Inventory::collect();
            if json {
                let mut v = info.to_json();
                v["install"] = inventory.to_json();
                v["home"] = serde_json::json!(home.root());
                println!("{}", serde_json::to_string_pretty(&v)?);
            } else {
                print!("{info}{inventory}");
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
            set,
        } => {
            let overrides = instance::CreateOverrides {
                cpus,
                memory,
                disk,
                mounts: mount,
                set,
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
                        "{:<16} {:<9} {:<15} {:>4} {:>8} {:>8}  DIR",
                        "NAME", "STATUS", "ADDRESS", "CPUS", "MEMORY", "DISK"
                    );
                    for r in &rows {
                        let addr = r
                            .addrs
                            .iter()
                            .find(|a| a.is_ipv4())
                            .or(r.addrs.first())
                            .map_or_else(|| "-".to_owned(), ToString::to_string);
                        println!(
                            "{:<16} {:<9} {:<15} {:>4} {:>8} {:>8}  {}",
                            r.name,
                            r.status,
                            addr,
                            r.cpus,
                            r.memory,
                            r.disk,
                            r.dir.display()
                        );
                    }
                }
            }
        }
        Command::Delete { name, force } => {
            if state::running_pid(&home.instance_dir(&name)?).is_some() {
                if !force {
                    exit::fail!(
                        exit::Kind::WrongState,
                        "{name} is running; `vmuc stop {name}` or `vmuc delete --force {name}`"
                    );
                }
                lifecycle::stop(&home, &name, true)?;
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
                    exit::fail!(exit::Kind::Invalid, "{name} is a URL; download it first");
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
            balloon,
            snapshot,
            snapshot_after,
            restore,
            restore_ram,
        } => {
            let memory: vmuc_config::ByteSize = memory.parse()?;
            let ram = if restore_ram == "map" {
                vmuc_vmm::snapshot::RamRestore::Map
            } else {
                vmuc_vmm::snapshot::RamRestore::Copy
            };
            let exit = boot::run(&boot::BootArgs {
                kernel,
                initrd,
                cmdline,
                cpus,
                memory: memory.0,
                disks: disk.iter().map(|d| boot::parse_disk(d)).collect(),
                hvc,
                balloon,
                snapshot: snapshot.zip(snapshot_after.map(std::time::Duration::from_secs_f64)),
                restore: restore.map(|dir| (dir, ram)),
            })?;
            eprintln!("vmuc: guest exited: {exit:?}");
            if let vmuc_vmm::VmExit::Crashed(_) = exit {
                std::process::exit(1);
            }
        }
        Command::Start {
            name,
            timeout,
            foreground,
        } => {
            let timeout = std::time::Duration::from_secs(timeout);
            if foreground {
                lifecycle::start_foreground(&home, &name, timeout)?;
            } else {
                lifecycle::start(&home, &name, timeout)?;
            }
        }
        Command::Stop { name, force } => lifecycle::stop(&home, &name, force)?,
        Command::Suspend { name } => lifecycle::suspend(&home, &name)?,
        Command::Resume { name, timeout } => {
            lifecycle::resume(&home, &name, std::time::Duration::from_secs(timeout))?;
        }
        Command::Duplicate {
            source,
            name,
            cpus,
            memory,
            disk,
            mount,
            set,
            start,
        } => {
            let overrides = instance::CreateOverrides {
                cpus,
                memory,
                disk,
                mounts: mount,
                set,
            };
            let (inst, kind) = duplicate::duplicate(&home, &source, &name, &overrides)?;
            let how = match kind {
                vmuc_image::CloneKind::Shared => "its disk shared copy-on-write",
                vmuc_image::CloneKind::Copied => "its disk copied",
            };
            println!(
                "duplicated {source} as {name} ({how}) in {}",
                inst.dir.display()
            );
            if start {
                lifecycle::start(&home, &name, std::time::Duration::from_secs(600))?;
            }
        }
        Command::Restart { name, timeout } => {
            lifecycle::stop(&home, &name, false)?;
            lifecycle::start(&home, &name, std::time::Duration::from_secs(timeout))?;
        }
        Command::Shell {
            name,
            workdir,
            command,
        } => lifecycle::shell(&home, &name, workdir.as_deref(), &command)?,
        Command::Stat { name, watch } => stat::run(&home, &name, watch, json)?,
        Command::Tui => tui::run(&home)?,
        Command::Copy { recursive, paths } => copy::run(&home, recursive, &paths)?,
        Command::Edit { name } => instance::edit(&home, &name)?,
        Command::Prune => {
            let pruned = vmuc_image::Cache::open_default()?.prune()?;
            for p in &pruned {
                println!("removed {} ({})", p.url, stat::human(p.bytes));
            }
            let total: u64 = pruned.iter().map(|p| p.bytes).sum();
            println!("freed {}", stat::human(total));
        }
        Command::Completion { shell } => {
            clap_complete::generate(shell, &mut public_command(), "vmuc", &mut std::io::stdout());
        }
        Command::Sudoers { helper } => {
            let helper = helper.unwrap_or_else(assets::vmnet_helper);
            print!("{}", sudoers_text(&helper));
        }
        Command::Reclaim { name } => {
            let dir = home.instance_dir(&name)?;
            match control::request(&dir, &vmuc_proto::ControlRequest::ReclaimNow)? {
                vmuc_proto::ControlResponse::Ok => {
                    eprintln!("asked {name} to drop caches and compact");
                }
                other => anyhow::bail!("{other:?}"),
            }
        }
        Command::Pressure {
            name,
            level,
            seconds,
        } => {
            let dir = home.instance_dir(&name)?;
            let req = vmuc_proto::ControlRequest::SimulatePressure {
                level: level.into(),
                seconds,
            };
            match control::request(&dir, &req)? {
                vmuc_proto::ControlResponse::Ok => {}
                other => anyhow::bail!("{other:?}"),
            }
        }
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
            let mounts = if no_mounts {
                Vec::new()
            } else {
                instance::seed_mounts(&resolved)
            };
            let boot_id = instance::write_seed(&home, &inst, &resolved, agent.as_deref(), &mounts)?;
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
            repeat,
        } => {
            let req = agent_client::parse_request(&method, params.as_deref())?;
            if let Some(n) = repeat {
                let q = agent_client::round_trips(&via, &req, n)?;
                println!("{}", serde_json::to_string(&q)?);
                return Ok(());
            }
            let resp = agent_client::call(&via, &req)?;
            println!("{}", serde_json::to_string_pretty(&resp)?);
            if matches!(resp, vmuc_proto::AgentResponse::Error { .. }) {
                std::process::exit(1);
            }
        }
    }
    Ok(())
}

/// The command tree without the hidden internal subcommands, for
/// completion scripts (clap_complete's bash and fish generators list
/// hidden subcommands too).
fn public_command() -> clap::Command {
    use clap::CommandFactory;
    let full = Cli::command();
    clap::Command::new("vmuc")
        .about(full.get_about().cloned().unwrap_or_default())
        .version(env!("CARGO_PKG_VERSION"))
        .args(full.get_arguments().filter(|a| !a.is_hide_set()).cloned())
        .subcommands(full.get_subcommands().filter(|c| !c.is_hide_set()).cloned())
}

/// Install instructions (as comments) followed by the sudoers rule, so
/// the output is itself a valid sudoers file. The rule is checked with
/// visudo before it goes in: a broken file in /etc/sudoers.d breaks sudo.
fn sudoers_text(helper: &std::path::Path) -> String {
    let exe = std::env::current_exe().ok();
    let built = exe
        .as_ref()
        .and_then(|e| e.parent().map(|d| d.join("vmuc-vmnet-helper")))
        .filter(|p| p.is_file())
        .map_or_else(
            || "<path to vmuc-vmnet-helper>".to_owned(),
            |p| p.display().to_string(),
        );
    let exe = exe.map_or_else(|| "vmuc".to_owned(), |e| e.display().to_string());
    let dir = helper
        .parent()
        .map_or_else(String::new, |d| d.display().to_string());
    format!(
        "# vmuc networking runs vmnet through a small root helper. Install it\n\
         # where only root can change it, then this rule, checked first:\n\
         #   sudo install -d -o root -g wheel -m 0755 {dir}\n\
         #   sudo install -o root -g wheel -m 0755 {built} {helper}\n\
         #   {exe} sudoers > /tmp/vmuc.sudoers\n\
         #   sudo visudo -cf /tmp/vmuc.sudoers && sudo install -o root -g wheel -m 0440 /tmp/vmuc.sudoers {sudoers}\n{}",
        vmuc_net::launch::sudoers_rule(helper),
        helper = helper.display(),
        sudoers = assets::SUDOERS_FILE,
    )
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
    fn completions_leave_out_internal_commands() {
        let names: Vec<String> = super::public_command()
            .get_subcommands()
            .map(|c| c.get_name().to_owned())
            .collect();
        assert!(names.iter().any(|n| n == "shell"));
        assert!(names.iter().all(|n| !n.starts_with('_')), "{names:?}");
    }

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
