// SPDX-License-Identifier: Apache-2.0
//! `vmuc copy`: scp through an instance's ssh tunnel. `NAME:PATH` is a
//! path in instance NAME (`NAME:` alone is its home directory); anything
//! else is local.

use std::os::unix::process::CommandExt;
use std::process::Command;

use anyhow::bail;
use vmuc_config::VmucHome;
use vmuc_config::paths::instance as files;

use crate::instance::Instance;
use crate::lifecycle::ssh_host;
use crate::state;

/// Replaces this process with scp.
pub fn run(home: &VmucHome, recursive: bool, paths: &[String]) -> anyhow::Result<()> {
    let (name, args) = scp_args(paths)?;
    let inst = Instance::open(home, &name)?;
    if state::running_pid(&inst.dir).is_none() {
        bail!("{name} is not running; `vmuc start {name}` first");
    }
    let mut cmd = Command::new("scp");
    cmd.arg("-F").arg(inst.dir.join(files::SSH_CONFIG));
    if recursive {
        cmd.arg("-r");
    }
    cmd.args(args);
    Err(anyhow::anyhow!("exec scp: {}", cmd.exec()))
}

/// The instance the paths name, and scp's arguments for them.
fn scp_args(paths: &[String]) -> anyhow::Result<(String, Vec<String>)> {
    if paths.len() < 2 {
        bail!("give at least one source and a destination");
    }
    let mut name: Option<&str> = None;
    let mut args = Vec::with_capacity(paths.len());
    for p in paths {
        match remote(p) {
            Some((n, path)) => {
                if *name.get_or_insert(n) != n {
                    bail!("copying between two instances is not supported; copy through the host");
                }
                args.push(format!("{}:{path}", ssh_host(n)));
            }
            None => args.push(p.clone()),
        }
    }
    let Some(name) = name else {
        bail!("name an instance path as NAME:PATH");
    };
    Ok((name.to_owned(), args))
}

/// `NAME:PATH` when NAME could be an instance (so `./a:b` stays local).
fn remote(p: &str) -> Option<(&str, &str)> {
    let (n, path) = p.split_once(':')?;
    vmuc_config::validate_instance_name(n)
        .is_ok()
        .then_some((n, path))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn instance_paths_become_ssh_hosts() {
        let (name, a) = scp_args(&args(&["notes.txt", "dev:/tmp/"])).unwrap();
        assert_eq!(name, "dev");
        assert_eq!(a, args(&["notes.txt", "vmuc-dev:/tmp/"]));
        let (_, a) = scp_args(&args(&["dev:", "dev:log.txt", "./a:b", "."])).unwrap();
        assert_eq!(a, args(&["vmuc-dev:", "vmuc-dev:log.txt", "./a:b", "."]));
    }

    #[test]
    fn refusals() {
        for bad in [&["dev:a"][..], &["a", "b"], &["one:a", "two:b"]] {
            assert!(scp_args(&args(bad)).is_err(), "{bad:?}");
        }
    }
}
