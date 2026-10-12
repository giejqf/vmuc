// SPDX-License-Identifier: Apache-2.0
//! Where the guest kernel and the guest agent come from.
//!
//! In order: an environment override (`VMUC_KERNEL`, `VMUC_AGENT`), the
//! install prefix (`<prefix>/share/vmuc/`, Homebrew), then a development
//! tree the binary was built in (`guest/kernel/out/Image` and the musl
//! agent next to `target/`). The release download of the kernel joins
//! the list once a release publishes one (`vmuc_image::assets`).

use std::path::{Path, PathBuf};

use anyhow::bail;

pub const KERNEL_ENV: &str = "VMUC_KERNEL";
pub const AGENT_ENV: &str = "VMUC_AGENT";
/// The vmnet helper, when not at its default root-owned home.
pub const VMNET_HELPER_ENV: &str = "VMUC_VMNET_HELPER";
/// Where `vmuc sudoers` tells the user to put the rule.
pub const SUDOERS_FILE: &str = "/etc/sudoers.d/vmuc";
/// Extra kernel parameters appended at boot (experiments and benchmarks).
pub const CMDLINE_ENV: &str = "VMUC_KERNEL_CMDLINE";

const AGENT_TARGET: &str = "aarch64-unknown-linux-musl";

/// `<prefix>` for a binary at `<prefix>/bin/vmuc`.
fn prefix(exe: &Path) -> Option<&Path> {
    exe.parent()?.parent()
}

/// The repository root for a binary at `<root>/target/<profile>/vmuc`.
fn dev_root(exe: &Path) -> Option<&Path> {
    let target = exe.parent()?.parent()?;
    (target.file_name()? == "target")
        .then(|| target.parent())
        .flatten()
}

fn candidates_kernel(exe: &Path) -> Vec<PathBuf> {
    let mut c = Vec::new();
    if let Some(p) = prefix(exe) {
        c.push(p.join("share/vmuc/kernel/Image"));
    }
    if let Some(r) = dev_root(exe) {
        c.push(r.join("guest/kernel/out/Image"));
    }
    c
}

fn candidates_agent(exe: &Path) -> Vec<PathBuf> {
    let mut c = Vec::new();
    if let Some(p) = prefix(exe) {
        c.push(p.join("share/vmuc/vmuc-agent"));
    }
    if let Some(r) = dev_root(exe) {
        for profile in ["release", "debug"] {
            c.push(
                r.join("target")
                    .join(AGENT_TARGET)
                    .join(profile)
                    .join("vmuc-agent"),
            );
        }
    }
    c
}

fn resolve(env: &str, candidates: &[PathBuf], what: &str, hint: &str) -> anyhow::Result<PathBuf> {
    if let Some(p) = std::env::var_os(env).filter(|v| !v.is_empty()) {
        let p = PathBuf::from(p);
        if !p.is_file() {
            bail!("{env}={} does not exist", p.display());
        }
        return Ok(p);
    }
    if let Some(p) = candidates.iter().find(|p| p.is_file()) {
        return Ok(p.clone());
    }
    let looked: Vec<String> = candidates.iter().map(|p| p.display().to_string()).collect();
    bail!(
        "no {what} found (looked in {}); set {env} or {hint}",
        looked.join(", ")
    )
}

pub fn kernel() -> anyhow::Result<PathBuf> {
    let exe = std::env::current_exe()?;
    resolve(
        KERNEL_ENV,
        &candidates_kernel(&exe),
        "guest kernel",
        "build it (make kernel) or download the kernel workflow artifact into guest/kernel/out",
    )
}

/// The vmnet helper (`vmuc sudoers` explains how to install it). Not
/// searched for in the build tree: sudo must only run a root-owned copy.
pub fn vmnet_helper() -> PathBuf {
    std::env::var_os(VMNET_HELPER_ENV)
        .filter(|v| !v.is_empty())
        .map_or_else(
            || PathBuf::from(vmuc_net::launch::DEFAULT_HELPER),
            PathBuf::from,
        )
}

pub fn agent() -> anyhow::Result<PathBuf> {
    let exe = std::env::current_exe()?;
    resolve(
        AGENT_ENV,
        &candidates_agent(&exe),
        "guest agent",
        "run make agent",
    )
}

/// What this install has of the pieces an instance needs, for `vmuc info`.
#[derive(Debug)]
pub struct Inventory {
    pub kernel: Result<PathBuf, String>,
    pub agent: Result<PathBuf, String>,
    pub helper: PathBuf,
    /// Why networking will be off, or `None` when the helper is installed
    /// root-owned where sudo may run it.
    pub helper_problem: Option<String>,
    /// The sudoers rule file exists (it is readable only by root).
    pub sudoers_rule: bool,
}

impl Inventory {
    pub fn collect() -> Self {
        let helper = vmnet_helper();
        Self {
            kernel: kernel().map_err(|e| e.to_string()),
            agent: agent().map_err(|e| e.to_string()),
            helper_problem: vmuc_net::launch::check_root_owned(&helper)
                .err()
                .map(|e| e.to_string()),
            helper,
            sudoers_rule: Path::new(SUDOERS_FILE).exists(),
        }
    }

    /// Instances can start (networking is optional).
    pub fn ready(&self) -> bool {
        self.kernel.is_ok() && self.agent.is_ok()
    }

    pub fn to_json(&self) -> serde_json::Value {
        let path = |r: &Result<PathBuf, String>| match r {
            Ok(p) => serde_json::json!({ "path": p }),
            Err(e) => serde_json::json!({ "error": e }),
        };
        serde_json::json!({
            "kernel": path(&self.kernel),
            "agent": path(&self.agent),
            "vmnetHelper": { "path": self.helper, "problem": self.helper_problem },
            "sudoersRule": self.sudoers_rule,
            "ready": self.ready(),
        })
    }
}

impl std::fmt::Display for Inventory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let show = |r: &Result<PathBuf, String>| match r {
            Ok(p) => p.display().to_string(),
            Err(e) => format!("missing: {e}"),
        };
        writeln!(f, "kernel:      {}", show(&self.kernel))?;
        writeln!(f, "agent:       {}", show(&self.agent))?;
        match (&self.helper_problem, self.sudoers_rule) {
            (None, true) => writeln!(f, "networking:  vmnet helper {}", self.helper.display())?,
            (None, false) => writeln!(
                f,
                "networking:  off (helper installed, no {SUDOERS_FILE}; see `vmuc sudoers`)"
            )?,
            (Some(why), _) => writeln!(f, "networking:  off ({why})")?,
        }
        writeln!(
            f,
            "ready:       {}",
            if self.ready() { "yes" } else { "no" }
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inventory_reports_what_is_missing() {
        let inv = Inventory {
            kernel: Ok("/k/Image".into()),
            agent: Err("no guest agent found".into()),
            helper: "/usr/local/libexec/vmuc/vmuc-vmnet-helper".into(),
            helper_problem: Some("it does not exist".into()),
            sudoers_rule: false,
        };
        assert!(!inv.ready());
        let text = inv.to_string();
        assert!(text.contains("kernel:      /k/Image"), "{text}");
        assert!(
            text.contains("agent:       missing: no guest agent found"),
            "{text}"
        );
        assert!(
            text.contains("networking:  off (it does not exist)"),
            "{text}"
        );
        assert_eq!(inv.to_json()["agent"]["error"], "no guest agent found");
        assert_eq!(inv.to_json()["ready"], false);
    }

    #[test]
    fn dev_tree_and_prefix_candidates() {
        let exe = Path::new("/src/vmuc/target/debug/vmuc");
        assert_eq!(dev_root(exe), Some(Path::new("/src/vmuc")));
        assert!(
            candidates_kernel(exe).contains(&PathBuf::from("/src/vmuc/guest/kernel/out/Image"))
        );
        assert!(candidates_agent(exe).contains(&PathBuf::from(
            "/src/vmuc/target/aarch64-unknown-linux-musl/release/vmuc-agent"
        )));
        let brew = Path::new("/opt/homebrew/Cellar/vmuc/0.1.0/bin/vmuc");
        assert_eq!(dev_root(brew), None);
        assert_eq!(
            candidates_agent(brew),
            vec![PathBuf::from(
                "/opt/homebrew/Cellar/vmuc/0.1.0/share/vmuc/vmuc-agent"
            )]
        );
    }

    #[test]
    fn missing_everything_explains_itself() {
        let err = resolve(
            "VMUC_TEST_UNSET_VAR",
            &[PathBuf::from("/nope")],
            "thing",
            "make it",
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("/nope") && err.contains("make it"), "{err}");
    }
}
