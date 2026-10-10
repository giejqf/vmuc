// SPDX-License-Identifier: Apache-2.0
//! The ssh keypair vmuc uses for every instance (`~/.vmuc/_config/user`),
//! generated on first use with the system `ssh-keygen`.

use std::fs;
use std::process::Command;

use anyhow::{Context, bail};
use vmuc_config::VmucHome;

pub fn ensure_keypair(home: &VmucHome) -> anyhow::Result<()> {
    let key = home.ssh_key();
    if key.is_file() && home.ssh_pub_key().is_file() {
        return Ok(());
    }
    fs::create_dir_all(home.config_dir())?;
    let status = Command::new("ssh-keygen")
        .args(["-q", "-t", "ed25519", "-N", "", "-C", "vmuc", "-f"])
        .arg(&key)
        .status()
        .context("running ssh-keygen (is OpenSSH installed?)")?;
    if !status.success() {
        bail!("ssh-keygen failed with {status}");
    }
    Ok(())
}

/// The vmuc key plus, optionally, every `~/.ssh/*.pub` (Lima's
/// `loadDotSSHPubKeys`).
pub fn authorized_keys(home: &VmucHome, load_dot_ssh: bool) -> anyhow::Result<Vec<String>> {
    ensure_keypair(home)?;
    let mut keys = vec![fs::read_to_string(home.ssh_pub_key())?.trim().to_owned()];
    if load_dot_ssh && let Some(ssh_dir) = dirs::home_dir().map(|h| h.join(".ssh")) {
        let mut extra: Vec<_> = fs::read_dir(&ssh_dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x == "pub"))
            .filter_map(|e| fs::read_to_string(e.path()).ok())
            .map(|s| s.trim().to_owned())
            .filter(|s| s.starts_with("ssh-") || s.starts_with("ecdsa-") || s.starts_with("sk-"))
            .collect();
        extra.sort();
        keys.extend(extra);
    }
    keys.dedup();
    Ok(keys)
}
