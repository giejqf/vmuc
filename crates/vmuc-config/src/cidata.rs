// SPDX-License-Identifier: Apache-2.0
//! The cloud-init NoCloud seed (`cidata.iso` contents).
//!
//! `render_seed` turns a [`ResolvedConfig`] into the files vmuc-image
//! writes into the ISO: cloud-init's `meta-data`, `user-data` and
//! `network-config`, plus vmuc's own `vmuc.env`, `boot.sh`, built-in boot
//! scripts, provisioning scripts and the agent binary. The scripts are
//! POSIX sh and live next to this file so shellcheck can lint them.

use std::fmt::Write as _;

use serde::Serialize;

use crate::defaults::{ResolvedConfig, ResolvedProvision};

/// Volume label of the seed ISO; the guest mounts it by this label.
pub const SEED_LABEL: &str = "cidata";
/// Where the guest mounts the seed.
pub const SEED_MOUNT_POINT: &str = "/mnt/vmuc-cidata";
/// The guest writes the boot ID here when provisioning is done.
pub const BOOT_DONE_PATH: &str = "/run/vmuc-boot-done";
/// Where the agent is installed in the guest.
pub const AGENT_PATH: &str = "/usr/local/bin/vmuc-agent";
/// cloud-init per-boot hook that mounts the seed and runs `boot.sh`.
pub const PER_BOOT_HOOK_PATH: &str = "/var/lib/cloud/scripts/per-boot/00-vmuc.boot.sh";
/// systemd unit written by cloud-init.
pub const AGENT_UNIT_PATH: &str = "/etc/systemd/system/vmuc-agent.service";

const BOOT_SH: &str = include_str!("cidata/boot.sh");
const MOUNTS_SH: &str = include_str!("cidata/05-mounts.sh");
const PER_BOOT_HOOK: &str = include_str!("cidata/00-vmuc.boot.sh");
const AGENT_UNIT: &str = include_str!("cidata/vmuc-agent.service");
const AGENT_OPENRC: &str = include_str!("cidata/vmuc-agent.openrc");

/// One file in the seed ISO.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeedFile {
    /// Path inside the ISO, relative, `/`-separated.
    pub path: String,
    pub data: Vec<u8>,
    /// POSIX mode bits (Rock Ridge), e.g. `0o644` or `0o755`.
    pub mode: u32,
}

impl SeedFile {
    fn text(path: &str, text: impl Into<String>) -> Self {
        Self {
            path: path.to_owned(),
            data: text.into().into_bytes(),
            mode: 0o644,
        }
    }

    fn script(path: &str, text: impl Into<String>) -> Self {
        Self {
            mode: 0o755,
            ..Self::text(path, text)
        }
    }
}

/// A virtiofs share as the VMM exposes it: the tag the guest mounts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeedMount {
    pub tag: String,
    pub mount_point: String,
    pub writable: bool,
}

/// A fresh per-boot instance ID (cloud-init's `instance-id`).
pub fn new_boot_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// Renders every file of the seed for one boot.
pub fn render_seed(
    resolved: &ResolvedConfig,
    boot_id: &str,
    ssh_pub_keys: &[String],
    agent_binary: Option<&[u8]>,
    mounts: &[SeedMount],
) -> Vec<SeedFile> {
    let mut files = vec![
        SeedFile::text("meta-data", render_meta_data(resolved, boot_id)),
        SeedFile::text(
            "user-data",
            render_user_data(resolved, ssh_pub_keys, mounts),
        ),
        SeedFile::text("network-config", render_network_config(resolved)),
        SeedFile::text("vmuc.env", render_env(resolved, boot_id, mounts)),
        SeedFile::script("boot.sh", BOOT_SH),
        SeedFile::script("boot/05-mounts.sh", MOUNTS_SH),
        SeedFile::script(
            "boot/10-etc-environment.sh",
            render_etc_environment_script(resolved),
        ),
        SeedFile::script("vmuc-agent.openrc", AGENT_OPENRC),
    ];
    for (i, p) in resolved.provision.iter().enumerate() {
        match p {
            ResolvedProvision::System { script } => {
                files.push(SeedFile::script(
                    &format!("provision.system/{i:02}.sh"),
                    script.clone(),
                ));
            }
            ResolvedProvision::User { script } => {
                files.push(SeedFile::script(
                    &format!("provision.user/{i:02}.sh"),
                    script.clone(),
                ));
            }
            ResolvedProvision::Boot { .. } => {} // rendered into user-data bootcmd
            ResolvedProvision::Data(d) => {
                files.push(SeedFile::text(
                    &format!("provision.data/{i:02}"),
                    d.content.clone(),
                ));
                files.push(SeedFile::script(
                    &format!("provision.data/{i:02}.sh"),
                    render_data_script(i, &d.path, &d.owner, &d.permissions, d.overwrite),
                ));
            }
        }
    }
    if let Some(agent) = agent_binary {
        files.push(SeedFile {
            path: "vmuc-agent".to_owned(),
            data: agent.to_vec(),
            mode: 0o755,
        });
    }
    files
}

fn render_meta_data(resolved: &ResolvedConfig, boot_id: &str) -> String {
    format!(
        "instance-id: {boot_id}\nlocal-hostname: {}\n",
        resolved.hostname()
    )
}

#[derive(Serialize)]
struct CloudConfig<'a> {
    growpart: Growpart,
    users: Vec<CloudUser<'a>>,
    write_files: Vec<WriteFile<'a>>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    mounts: Vec<[String; 6]>,
    timezone: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    manage_resolv_conf: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    resolv_conf: Option<ResolvConf>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    bootcmd: Vec<String>,
}

#[derive(Serialize)]
struct Growpart {
    mode: &'static str,
    devices: [&'static str; 1],
}

#[derive(Serialize)]
struct CloudUser<'a> {
    name: &'a str,
    uid: u32,
    #[serde(skip_serializing_if = "str::is_empty")]
    gecos: &'a str,
    homedir: &'a str,
    shell: &'a str,
    sudo: &'static str,
    lock_passwd: bool,
    ssh_authorized_keys: &'a [String],
}

#[derive(Serialize)]
struct WriteFile<'a> {
    path: &'a str,
    owner: &'static str,
    permissions: &'static str,
    content: &'a str,
}

#[derive(Serialize)]
struct ResolvConf {
    nameservers: Vec<String>,
}

fn render_user_data(
    resolved: &ResolvedConfig,
    ssh_pub_keys: &[String],
    mounts: &[SeedMount],
) -> String {
    let user = &resolved.user;
    let param_exports = param_exports(resolved);
    let bootcmd = resolved
        .provision
        .iter()
        .filter_map(|p| match p {
            ResolvedProvision::Boot { script } => Some(format!("{param_exports}{script}")),
            _ => None,
        })
        .collect();
    let config = CloudConfig {
        growpart: Growpart {
            mode: "auto",
            devices: ["/"],
        },
        users: vec![CloudUser {
            name: &user.name,
            uid: user.uid,
            gecos: &user.comment,
            homedir: &user.home,
            shell: &user.shell,
            sudo: if user.passwordless_sudo {
                "ALL=(ALL) NOPASSWD:ALL"
            } else {
                "ALL=(ALL) ALL"
            },
            lock_passwd: true,
            ssh_authorized_keys: ssh_pub_keys,
        }],
        write_files: vec![
            WriteFile {
                path: AGENT_UNIT_PATH,
                owner: "root:root",
                permissions: "0644",
                content: AGENT_UNIT,
            },
            WriteFile {
                path: PER_BOOT_HOOK_PATH,
                owner: "root:root",
                permissions: "0755",
                content: PER_BOOT_HOOK,
            },
        ],
        mounts: mounts
            .iter()
            .map(|m| {
                [
                    m.tag.clone(),
                    m.mount_point.clone(),
                    "virtiofs".to_owned(),
                    if m.writable {
                        "defaults"
                    } else {
                        "defaults,ro"
                    }
                    .to_owned(),
                    "0".to_owned(),
                    "0".to_owned(),
                ]
            })
            .collect(),
        timezone: &resolved.timezone,
        manage_resolv_conf: (!resolved.dns.is_empty()).then_some(true),
        resolv_conf: (!resolved.dns.is_empty()).then(|| ResolvConf {
            nameservers: resolved.dns.iter().map(ToString::to_string).collect(),
        }),
        bootcmd,
    };
    let body = serde_yaml::to_string(&config).expect("cloud-config is plain data");
    format!("#cloud-config\n{body}")
}

fn render_network_config(resolved: &ResolvedConfig) -> String {
    // Matching by MAC works with every cloud-init renderer (netplan on
    // Ubuntu, ifupdown on Alpine); matching by driver only works with netplan.
    format!(
        "version: 2\nethernets:\n  eth0:\n    match:\n      macaddress: \"{}\"\n    dhcp4: true\n    dhcp-identifier: mac\n",
        resolved.network.mac_address
    )
}

/// Single-quotes `s` for `sh`.
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

fn param_exports(resolved: &ResolvedConfig) -> String {
    let mut out = String::new();
    for (k, v) in &resolved.param {
        let _ = writeln!(out, "export VMUC_PARAM_{k}={}", shell_quote(v));
    }
    out
}

fn render_env(resolved: &ResolvedConfig, boot_id: &str, mounts: &[SeedMount]) -> String {
    let mut out = String::new();
    let mut put = |k: &str, v: &str| {
        let _ = writeln!(out, "export {k}={}", shell_quote(v));
    };
    put("VMUC_BOOT_ID", boot_id);
    put("VMUC_NAME", &resolved.name);
    put("VMUC_HOSTNAME", &resolved.hostname());
    put("VMUC_USER", &resolved.user.name);
    put("VMUC_UID", &resolved.user.uid.to_string());
    put("VMUC_HOME", &resolved.user.home);
    put("VMUC_SHELL", &resolved.user.shell);
    put(
        "VMUC_PASSWORDLESS_SUDO",
        if resolved.user.passwordless_sudo {
            "1"
        } else {
            "0"
        },
    );
    put("VMUC_MOUNTS_COUNT", &mounts.len().to_string());
    for (i, m) in mounts.iter().enumerate() {
        put(&format!("VMUC_MOUNT_{i}_TAG"), &m.tag);
        put(&format!("VMUC_MOUNT_{i}_MOUNTPOINT"), &m.mount_point);
        put(
            &format!("VMUC_MOUNT_{i}_WRITABLE"),
            if m.writable { "1" } else { "0" },
        );
    }
    for (k, v) in &resolved.param {
        put(&format!("VMUC_PARAM_{k}"), v);
    }
    for (k, v) in &resolved.env {
        put(k, v);
    }
    out
}

/// Writes `env:` entries into `/etc/environment` between markers so
/// login shells (pam_env) see them; idempotent across boots.
fn render_etc_environment_script(resolved: &ResolvedConfig) -> String {
    let mut lines = String::new();
    for (k, v) in &resolved.env {
        let _ = writeln!(lines, "{k}={}", shell_quote(v));
    }
    format!(
        "#!/bin/sh\n\
# SPDX-License-Identifier: Apache-2.0\n\
# Generated by vmuc: merges the instance env into /etc/environment.\n\
set -eu\n\
f=/etc/environment\n\
tmp=\"$(mktemp)\"\n\
if [ -f \"$f\" ]; then\n\
\tsed '/^# vmuc-env-begin$/,/^# vmuc-env-end$/d' \"$f\" >\"$tmp\"\n\
fi\n\
cat >>\"$tmp\" <<'VMUC_EOF'\n\
# vmuc-env-begin\n\
{lines}\
# vmuc-env-end\n\
VMUC_EOF\n\
chmod 0644 \"$tmp\"\n\
mv -f \"$tmp\" \"$f\"\n"
    )
}

/// The script that installs one `mode: data` file.
fn render_data_script(
    index: usize,
    path: &str,
    owner: &str,
    permissions: &str,
    overwrite: bool,
) -> String {
    format!(
        "#!/bin/sh\n\
# SPDX-License-Identifier: Apache-2.0\n\
# Generated by vmuc: installs provision.data/{index:02}.\n\
set -eu\n\
src=\"${{VMUC_CIDATA_MNT:-{SEED_MOUNT_POINT}}}/provision.data/{index:02}\"\n\
dst={dst}\n\
overwrite={overwrite}\n\
if [ -e \"$dst\" ] && [ \"$overwrite\" = 0 ]; then\n\
\techo \"vmuc: not overwriting $dst\"\n\
\texit 0\n\
fi\n\
mkdir -p \"$(dirname \"$dst\")\"\n\
cp \"$src\" \"$dst\"\n\
chown {owner} \"$dst\"\n\
chmod {permissions} \"$dst\"\n",
        dst = shell_quote(path),
        overwrite = u8::from(overwrite),
        owner = shell_quote(owner),
        permissions = shell_quote(permissions),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::defaults::{Builtin, fill_defaults};
    use crate::schema::{ByteSize, InstanceConfig};
    use std::process::Command;

    fn sample() -> ResolvedConfig {
        let y = InstanceConfig::from_yaml(
            r#"
provision:
- mode: boot
  script: |
    #!/bin/sh
    echo early
- mode: system
  script: |
    #!/bin/sh
    echo "it's root"
- mode: user
  script: "whoami"
- mode: data
  path: /etc/motd
  content: "welcome\n"
  permissions: "0600"
  overwrite: false
param:
  SNAPSHOTTER: "o'verlay"
env:
  HTTP_PROXY: http://proxy:3128
dns: [1.1.1.1]
user:
  name: alice
  uid: 501
  comment: "Alice Example"
timezone: Europe/Berlin
"#,
            "sample",
        )
        .unwrap();
        fill_defaults(
            "demo",
            &y,
            None,
            None,
            &Builtin::fixed(16 * ByteSize::GIB, "/Users/alice"),
        )
        .unwrap()
    }

    fn mounts() -> Vec<SeedMount> {
        vec![
            SeedMount {
                tag: "mount0".into(),
                mount_point: "/Users/alice".into(),
                writable: false,
            },
            SeedMount {
                tag: "mount1".into(),
                mount_point: "/tmp/share".into(),
                writable: true,
            },
        ]
    }

    fn file<'a>(files: &'a [SeedFile], path: &str) -> &'a SeedFile {
        files.iter().find(|f| f.path == path).unwrap_or_else(|| {
            panic!(
                "{path} missing from {:?}",
                files.iter().map(|f| &f.path).collect::<Vec<_>>()
            )
        })
    }

    fn text<'a>(files: &'a [SeedFile], path: &str) -> &'a str {
        std::str::from_utf8(&file(files, path).data).unwrap()
    }

    #[test]
    fn renders_every_file() {
        let keys = ["ssh-ed25519 AAAA alice@mac".to_owned()];
        let files = render_seed(&sample(), "boot-1", &keys, Some(b"ELF"), &mounts());
        let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(
            paths,
            [
                "meta-data",
                "user-data",
                "network-config",
                "vmuc.env",
                "boot.sh",
                "boot/05-mounts.sh",
                "boot/10-etc-environment.sh",
                "vmuc-agent.openrc",
                "provision.system/01.sh",
                "provision.user/02.sh",
                "provision.data/03",
                "provision.data/03.sh",
                "vmuc-agent",
            ]
        );
        assert_eq!(file(&files, "boot.sh").mode, 0o755);
        assert_eq!(file(&files, "vmuc-agent").mode, 0o755);
        assert_eq!(file(&files, "vmuc-agent").data, b"ELF");
        assert_eq!(file(&files, "meta-data").mode, 0o644);
        assert_eq!(
            text(&files, "provision.system/01.sh"),
            "#!/bin/sh\necho \"it's root\"\n"
        );
        assert_eq!(text(&files, "provision.data/03"), "welcome\n");
        let without_agent = render_seed(&sample(), "boot-1", &keys, None, &[]);
        assert!(without_agent.iter().all(|f| f.path != "vmuc-agent"));
    }

    #[test]
    fn meta_data_and_network_config() {
        let r = sample();
        let files = render_seed(&r, "boot-42", &[], None, &[]);
        assert_eq!(
            text(&files, "meta-data"),
            "instance-id: boot-42\nlocal-hostname: vmuc-demo\n"
        );
        let net = text(&files, "network-config");
        let parsed: serde_yaml::Value = serde_yaml::from_str(net).unwrap();
        assert_eq!(parsed["version"], 2);
        assert_eq!(
            parsed["ethernets"]["eth0"]["match"]["macaddress"],
            r.network.mac_address.as_str()
        );
        assert_eq!(parsed["ethernets"]["eth0"]["dhcp4"], true);
    }

    #[test]
    fn user_data_is_valid_cloud_config() {
        let keys = ["ssh-ed25519 AAAA alice@mac".to_owned()];
        let files = render_seed(&sample(), "boot-1", &keys, None, &mounts());
        let ud = text(&files, "user-data");
        assert!(ud.starts_with("#cloud-config\n"), "{ud}");
        let v: serde_yaml::Value = serde_yaml::from_str(ud).unwrap();
        assert_eq!(v["growpart"]["mode"], "auto");
        let user = &v["users"][0];
        assert_eq!(user["name"], "alice");
        assert_eq!(user["uid"], 501);
        assert_eq!(user["gecos"], "Alice Example");
        assert_eq!(user["homedir"], "/home/alice.linux");
        assert_eq!(user["shell"], "/bin/bash");
        assert_eq!(user["sudo"], "ALL=(ALL) NOPASSWD:ALL");
        assert_eq!(user["lock_passwd"], true);
        assert_eq!(user["ssh_authorized_keys"][0], "ssh-ed25519 AAAA alice@mac");
        let wf = &v["write_files"];
        assert_eq!(wf[0]["path"], AGENT_UNIT_PATH);
        let unit = wf[0]["content"].as_str().unwrap();
        assert!(unit.contains("ExecStart=/usr/local/bin/vmuc-agent"));
        assert!(unit.contains("Restart=always"));
        assert!(unit.contains("After=network.target"));
        assert!(unit.contains("User=root"));
        assert_eq!(wf[1]["path"], PER_BOOT_HOOK_PATH);
        assert_eq!(wf[1]["permissions"], "0755");
        let hook = wf[1]["content"].as_str().unwrap();
        assert_eq!(hook, PER_BOOT_HOOK);
        assert!(hook.contains("/dev/disk/by-label/cidata"));
        assert!(hook.contains("exec \"${VMUC_CIDATA_MNT}/boot.sh\""));
        let m = &v["mounts"];
        assert_eq!(m[0][0], "mount0");
        assert_eq!(m[0][1], "/Users/alice");
        assert_eq!(m[0][2], "virtiofs");
        assert_eq!(m[0][3], "defaults,ro");
        assert_eq!(m[1][3], "defaults");
        assert_eq!(m[1][5], "0");
        assert_eq!(v["timezone"], "Europe/Berlin");
        assert_eq!(v["manage_resolv_conf"], true);
        assert_eq!(v["resolv_conf"]["nameservers"][0], "1.1.1.1");
        let boot = v["bootcmd"][0].as_str().unwrap();
        assert!(
            boot.starts_with("export VMUC_PARAM_SNAPSHOTTER='o'\\''verlay'\n"),
            "{boot}"
        );
        assert!(boot.ends_with("echo early\n"), "{boot}");
    }

    #[test]
    fn user_data_omits_optional_sections() {
        let y = InstanceConfig::from_yaml("user: {passwordlessSudo: false}", "t").unwrap();
        let r = fill_defaults("x", &y, None, None, &Builtin::fixed(0, "/h")).unwrap();
        let files = render_seed(&r, "b", &[], None, &[]);
        let v: serde_yaml::Value = serde_yaml::from_str(text(&files, "user-data")).unwrap();
        assert!(v.get("mounts").is_none());
        assert!(v.get("resolv_conf").is_none());
        assert!(v.get("manage_resolv_conf").is_none());
        assert!(v.get("bootcmd").is_none());
        assert!(v["users"][0].get("gecos").is_none());
        assert_eq!(v["users"][0]["sudo"], "ALL=(ALL) ALL");
        assert_eq!(
            v["users"][0]["ssh_authorized_keys"]
                .as_sequence()
                .unwrap()
                .len(),
            0
        );
    }

    #[test]
    fn env_file_is_shell_safe() {
        let files = render_seed(&sample(), "boot-7", &[], None, &mounts());
        let env = text(&files, "vmuc.env");
        for line in [
            "export VMUC_BOOT_ID='boot-7'",
            "export VMUC_NAME='demo'",
            "export VMUC_HOSTNAME='vmuc-demo'",
            "export VMUC_USER='alice'",
            "export VMUC_UID='501'",
            "export VMUC_HOME='/home/alice.linux'",
            "export VMUC_PASSWORDLESS_SUDO='1'",
            "export VMUC_MOUNTS_COUNT='2'",
            "export VMUC_MOUNT_0_TAG='mount0'",
            "export VMUC_MOUNT_0_MOUNTPOINT='/Users/alice'",
            "export VMUC_MOUNT_0_WRITABLE='0'",
            "export VMUC_MOUNT_1_WRITABLE='1'",
            "export VMUC_PARAM_SNAPSHOTTER='o'\\''verlay'",
            "export HTTP_PROXY='http://proxy:3128'",
        ] {
            assert!(env.lines().any(|l| l == line), "missing {line:?} in\n{env}");
        }
        let etc = text(&files, "boot/10-etc-environment.sh");
        assert!(etc.contains("HTTP_PROXY='http://proxy:3128'"), "{etc}");
        assert_eq!(shell_quote("a'b"), "'a'\\''b'");
    }

    #[test]
    fn boot_script_contract() {
        assert!(BOOT_SH.starts_with("#!/bin/sh\n"));
        assert!(BOOT_SH.contains("set -eu"));
        assert!(BOOT_SH.contains(". \"${VMUC_CIDATA_MNT}/vmuc.env\""));
        assert!(BOOT_SH.contains("sha256sum"));
        assert!(BOOT_SH.contains("systemctl daemon-reload"));
        assert!(BOOT_SH.contains("systemctl enable --now vmuc-agent.service"));
        assert!(BOOT_SH.contains("run_scripts \"${VMUC_CIDATA_MNT}/boot\""));
        assert!(BOOT_SH.contains("run_scripts \"${VMUC_CIDATA_MNT}/provision.system\""));
        assert!(BOOT_SH.contains("echo \"${VMUC_BOOT_ID}\" >/run/vmuc-boot-done"));
        assert!(MOUNTS_SH.contains("mount -t virtiofs"));
        assert!(MOUNTS_SH.contains("mount -a || true"));
        let data = render_data_script(3, "/etc/motd", "root:root", "0600", false);
        assert!(data.contains("dst='/etc/motd'"));
        assert!(data.contains("overwrite=0\n"));
        assert!(data.contains("chmod '0600'"));
    }

    /// Lints the static and the generated scripts when shellcheck is installed.
    #[test]
    fn scripts_are_shellcheck_clean() {
        if Command::new("shellcheck")
            .arg("--version")
            .output()
            .is_err()
        {
            eprintln!("shellcheck not installed; skipping");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let files = render_seed(&sample(), "b", &[], None, &mounts());
        let mut paths = Vec::new();
        // The OpenRC script is not sh (openrc-run reads its variables).
        let is_sh = |f: &&SeedFile| {
            f.mode == 0o755 && f.path != "vmuc-agent" && !f.path.ends_with(".openrc")
        };
        for f in files.iter().filter(is_sh) {
            let p = dir.path().join(f.path.replace('/', "_"));
            std::fs::write(&p, &f.data).unwrap();
            paths.push(p);
        }
        let hook = dir.path().join("00-vmuc.boot.sh");
        std::fs::write(&hook, PER_BOOT_HOOK).unwrap();
        paths.push(hook);
        let out = Command::new("shellcheck")
            .args(["--shell=sh", "--severity=style"])
            .args(&paths)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "shellcheck:\n{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }

    #[test]
    fn boot_ids_are_unique_uuids() {
        let a = new_boot_id();
        let b = new_boot_id();
        assert_ne!(a, b);
        assert_eq!(a.len(), 36);
        uuid::Uuid::parse_str(&a).unwrap();
    }
}
