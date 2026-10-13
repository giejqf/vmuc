// SPDX-License-Identifier: Apache-2.0
//! The configuration reference: every key the schema accepts, with its
//! built-in default and what it does. `guest/templates/reference.yaml`
//! and `docs/config.md` are rendered from this table, and tests hold the
//! table to the schema (serde's own field lists) and the files to the
//! table, so none of them can drift.

use std::fmt::Write as _;

/// One key: a dotted path (`network.mode`), `[]` for the fields of a list
/// entry (`mounts[].writable`).
#[derive(Debug, Clone, Copy)]
pub struct Field {
    pub key: &'static str,
    pub default: &'static str,
    pub doc: &'static str,
    /// Commented YAML shown under the key in reference.yaml.
    pub example: &'static str,
    /// Another accepted name for this key (not repeated in the YAML).
    pub alias_of: &'static str,
}

const fn f(key: &'static str, default: &'static str, doc: &'static str) -> Field {
    Field {
        key,
        default,
        doc,
        example: "",
        alias_of: "",
    }
}

const fn ex(
    key: &'static str,
    default: &'static str,
    doc: &'static str,
    example: &'static str,
) -> Field {
    Field {
        key,
        default,
        doc,
        example,
        alias_of: "",
    }
}

const fn alias(key: &'static str, of: &'static str) -> Field {
    Field {
        key,
        default: "",
        doc: "",
        example: "",
        alias_of: of,
    }
}

/// `(section title, fields)`, in reference order.
pub const SECTIONS: &[(&str, &[Field])] = &[
    ("Basic configuration", BASIC),
    ("Advanced configuration", ADVANCED),
];

const BASIC: &[Field] = &[
    f(
        "base",
        "[] (and `default.yaml` uses the Ubuntu image and the ~ mount)",
        "Templates to flatten into this one at `vmuc create` time. Each entry is `template:NAME` (searched in $VMUC_TEMPLATES_PATH, ~/.vmuc/_templates, the installed templates, then the built-in ones) or a file path relative to this file. The child wins for scalars and maps; `provision` and `probes` run base-first; other lists are child-first; mounts merge by location; a non-empty child `images` replaces the base's. Earlier entries win over later ones. Nesting is limited to 8 levels.",
    ),
    ex(
        "images",
        "[] (an instance cannot start without an image)",
        "Root filesystem candidates, tried in order: the first one that downloads and verifies is used.",
        "- location: \"https://cloud-images.ubuntu.com/.../ubuntu-24.04-server-cloudimg-arm64.img\"\n  arch: \"aarch64\"\n  digest: \"sha256:...\"\n  kernel:\n    location: \"https://example.com/Image\"\n    digest: \"sha256:...\"\n    cmdline: \"console=hvc0 root=/dev/vda1 rw\"",
    ),
    alias("image", "images"),
    f(
        "images[].location",
        "required",
        "An https:// URL or a local path; qcow2 and raw images, optionally compressed.",
    ),
    f(
        "images[].arch",
        "\"aarch64\"",
        "Always \"aarch64\" (\"arm64\" is accepted).",
    ),
    f(
        "images[].digest",
        "none",
        "\"sha256:...\" or \"sha512:...\"; strongly recommended.",
    ),
    f(
        "images[].kernel",
        "the kernel vmuc ships",
        "A kernel to boot instead of vmuc's.",
    ),
    f(
        "images[].kernel.location",
        "required",
        "An https:// URL or a local path to an arm64 `Image`.",
    ),
    f(
        "images[].kernel.digest",
        "none",
        "\"sha256:...\" or \"sha512:...\".",
    ),
    f(
        "images[].kernel.cmdline",
        "vmuc's command line",
        "The kernel command line.",
    ),
    f("cpus", "4", "Number of vCPUs."),
    f(
        "memory",
        "min(\"8GiB\", half of the host's RAM)",
        "Guest RAM ceiling. The host pays only for pages the guest touches and gets back what the guest frees (free page reporting is always on). Binary units: \"8GiB\", \"8G\", \"8192MiB\", \"8192M\". Minimum 64MiB.",
    ),
    f(
        "disk",
        "\"100GiB\"",
        "Virtual disk size: a sparse raw file that grows on demand. Minimum 1GiB.",
    ),
    ex(
        "mounts",
        "the host home (\"~\") read-only at the same path",
        "Host directories shared into the guest over virtiofs. macOS asks once before the guest may read Desktop, Documents or Downloads.",
        "- location: \"~/src\"\n  mountPoint: \"/src\"\n  writable: true",
    ),
    f(
        "mounts[].location",
        "required",
        "Host path; \"~\" expands to the host home.",
    ),
    f(
        "mounts[].mountPoint",
        "the expanded host path (mirrored)",
        "Guest path.",
    ),
    f("mounts[].writable", "false", "Whether the guest may write."),
    f(
        "mounts[].dax",
        "false",
        "Map files straight from the host's file cache (virtiofs DAX) instead of copying them into the guest's: the guest's page cache holds none of the share's data. Experiment E2; see docs/experiments/2026-10-13-e2-virtiofs-dax.md.",
    ),
];

const ADVANCED: &[Field] = &[
    f(
        "ssh",
        "",
        "How `vmuc shell` and plain ssh reach the guest (always through vsock).",
    ),
    f(
        "ssh.loadDotSSHPubKeys",
        "false",
        "Also authorize ~/.ssh/*.pub besides ~/.vmuc/_config/user.pub.",
    ),
    f(
        "ssh.forwardAgent",
        "false",
        "Forward the host ssh agent into `vmuc shell`.",
    ),
    f(
        "network",
        "",
        "Guest networking through macOS vmnet, via the root helper `vmuc sudoers` installs; without it the instance boots without a NIC (ssh, shares and port forwards still work over vsock).",
    ),
    f(
        "network.mode",
        "\"shared\"",
        "\"shared\": NAT behind the host, the guest gets a 192.168.x address the host can reach. \"bridged\": the guest joins the network of `interface`.",
    ),
    f(
        "network.interface",
        "none",
        "Host interface for bridged mode (e.g. \"en0\"); required there.",
    ),
    f(
        "network.macAddress",
        "a stable address derived from the instance name",
        "The NIC's MAC address.",
    ),
    ex(
        "portForwards",
        "[] (plus the catch-all rule)",
        "Guest TCP listeners appear on the host, matched against these rules first to last; the override, instance and default lists are concatenated in that order and a catch-all rule (every guest listener on 0.0.0.0 or 127.0.0.1, ports 1024 and up except 22, to the same port on 127.0.0.1) ends the list. Forwarding rides vsock and needs no NIC.",
        "- guestPort: 8080\n  hostPort: 18080",
    ),
    f(
        "portForwards[].guestIp",
        "127.0.0.1 (also matches 0.0.0.0)",
        "Guest address the rule matches.",
    ),
    f(
        "portForwards[].guestPort",
        "none",
        "Guest port; exclusive with guestPortRange.",
    ),
    f(
        "portForwards[].guestPortRange",
        "[1, 65535]",
        "Guest ports the rule matches.",
    ),
    f(
        "portForwards[].hostIp",
        "127.0.0.1",
        "Host address to listen on.",
    ),
    f(
        "portForwards[].hostPort",
        "none",
        "Host port; exclusive with hostPortRange.",
    ),
    f(
        "portForwards[].hostPortRange",
        "the guest range",
        "Host ports, mapped one to one.",
    ),
    f(
        "portForwards[].proto",
        "any",
        "tcp | udp | any. Only TCP is forwarded today.",
    ),
    f(
        "portForwards[].ignore",
        "false",
        "Match and do not forward.",
    ),
    f(
        "portForwards[].static",
        "false",
        "Forward even while nothing listens in the guest.",
    ),
    ex(
        "provision",
        "[]",
        "Scripts run on every boot from the seed after the agent is up, in list order: `boot` first (cloud-init bootcmd, early), then `data` files are written, then the network is waited for (when there is a NIC), then `system` scripts run as root and `user` scripts as the instance user. Scripts see VMUC_* variables (VMUC_USER, VMUC_UID, VMUC_HOME, VMUC_BOOT_ID, VMUC_PARAM_<KEY>, ...). Make them idempotent.",
        "- mode: system\n  script: |\n    #!/bin/sh\n    set -eu\n    apt-get install -y htop\n- mode: data\n  path: /etc/motd\n  content: \"hello\"",
    ),
    f("provision[].mode", "system", "system | user | boot | data."),
    f(
        "provision[].script",
        "none",
        "The script (system, user and boot modes).",
    ),
    f(
        "provision[].path",
        "none",
        "Where a data entry writes `content`.",
    ),
    f("provision[].content", "none", "What a data entry writes."),
    f(
        "provision[].owner",
        "\"root:root\"",
        "Owner of a data file.",
    ),
    f(
        "provision[].permissions",
        "\"0644\"",
        "Mode of a data file.",
    ),
    f(
        "provision[].overwrite",
        "true",
        "Whether a data entry replaces an existing file.",
    ),
    ex(
        "probes",
        "[]",
        "Readiness probes the host runs through the agent until they succeed (timeout: `vmuc start --timeout`).",
        "- description: \"docker is running\"\n  script: |\n    #!/bin/sh\n    docker info >/dev/null\n  hint: \"See /var/log/cloud-init-output.log in the guest.\"",
    ),
    f("probes[].mode", "readiness", "The only mode."),
    f("probes[].description", "none", "Shown while waiting."),
    f(
        "probes[].script",
        "required",
        "Succeeds (exit 0) when ready.",
    ),
    f("probes[].hint", "none", "Printed when the probe fails."),
    ex(
        "env",
        "{}",
        "Environment variables for provisioning scripts and the login shell.",
        "HTTP_PROXY: http://proxy.example.com:3128",
    ),
    f(
        "param",
        "{}",
        "Template parameters, exported to scripts as VMUC_PARAM_<KEY>. Keys match [A-Za-z_][A-Za-z0-9_]*.",
    ),
    f("user", "", "The guest account `vmuc shell` logs in as."),
    f(
        "user.name",
        "the host user name when it is a valid POSIX name that is not a system account, else \"vmuc\"",
        "Login name.",
    ),
    f(
        "user.uid",
        "the host uid when 500 <= uid < 60000, else 1000",
        "Numeric user ID (matching the host's keeps shared files' owners right).",
    ),
    f(
        "user.home",
        "/home/<name>.linux (clear of the mirrored host home)",
        "Home directory.",
    ),
    f("user.shell", "/bin/bash", "Login shell."),
    f("user.comment", "\"\"", "GECOS field."),
    f(
        "user.passwordlessSudo",
        "true",
        "Whether sudo asks for a password.",
    ),
    f(
        "timezone",
        "the host's zone, else \"UTC\"",
        "IANA zone name applied inside the guest.",
    ),
    f("message", "\"\"", "Printed after `vmuc start`."),
    f(
        "dns",
        "[] (the DNS server vmnet's DHCP hands out: the host's resolver)",
        "Nameservers written to the guest's resolv.conf instead.",
    ),
    f(
        "hostResolver",
        "true",
        "Reserved for vmuc's own user-space network (v0.2); ignored with vmnet, whose DHCP already points the guest at the host's resolver.",
    ),
];

/// Every key, in reference order.
pub fn fields() -> impl Iterator<Item = &'static Field> {
    SECTIONS.iter().flat_map(|(_, f)| f.iter())
}

/// `text` as `# ` comment lines of at most 72 columns, at `indent`.
fn comment(text: &str, indent: &str, out: &mut String) {
    let mut line = String::new();
    for word in text.split_whitespace() {
        if !line.is_empty() && indent.len() + 2 + line.len() + 1 + word.len() > 72 {
            let _ = writeln!(out, "{indent}# {line}");
            line.clear();
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.is_empty() {
        let _ = writeln!(out, "{indent}# {line}");
    }
}

const YAML_HEADER: &str = "\
# vmuc instance configuration reference (generated from the schema by
# crates/vmuc-config/src/reference.rs; edit that, not this file).
#
# Every key is listed with its built-in default. `null` (or an omitted
# key) means \"unset\", which lets the layering fill it in:
#
#   built-in default  <  ~/.vmuc/_config/default.yaml  <  the instance's
#   vmuc.yaml  <  ~/.vmuc/_config/override.yaml
#
# Scalars take the highest-priority set value. `env` and `param` are
# merged (override wins per key). `provision`, `probes` and
# `portForwards` are concatenated override + instance + default, so
# first-match rules respect priority. `mounts` are merged by location
# with the highest-priority `writable`/`mountPoint` winning. `dns` and
# `images` take the highest-priority non-empty list whole.
#
# `vmuc create template:reference` creates an instance with every
# built-in default (and no image, so it cannot start). Copy the keys
# you want into your own template instead.
#
# Keys are camelCase; unknown keys are an error.
";

/// `guest/templates/reference.yaml`.
pub fn render_yaml() -> String {
    let mut out = String::from(YAML_HEADER);
    for (title, fields) in SECTIONS {
        let _ = write!(
            out,
            "\n# {rule}\n# {}\n# {rule}\n",
            title.to_uppercase(),
            rule = "-".repeat(70)
        );
        for field in *fields {
            if field.key.contains("[]") || !field.alias_of.is_empty() {
                continue; // list entries are shown by their list's example
            }
            let depth = field.key.matches('.').count();
            let indent = "  ".repeat(depth);
            let name = field.key.rsplit('.').next().unwrap_or(field.key);
            let is_map = fields
                .iter()
                .any(|g| g.key.starts_with(&format!("{}.", field.key)));
            if depth == 0 {
                out.push('\n');
            }
            comment(field.doc, &indent, &mut out);
            if is_map {
                let _ = writeln!(out, "{indent}{name}:");
                continue;
            }
            comment(
                &format!("Built-in default: {}", field.default),
                &indent,
                &mut out,
            );
            let _ = writeln!(out, "{indent}{name}: null");
            for line in field.example.lines() {
                let _ = writeln!(out, "{indent}# {line}");
            }
        }
    }
    out
}

/// `docs/config.md`.
pub fn render_markdown() -> String {
    let mut out = String::from(
        "# Configuration reference\n\n\
         Generated from the schema (`crates/vmuc-config/src/reference.rs`); the\n\
         commented YAML version is `guest/templates/reference.yaml`. A setting\n\
         left out falls back to the next layer: built-in default <\n\
         `~/.vmuc/_config/default.yaml` < the instance's `vmuc.yaml` <\n\
         `~/.vmuc/_config/override.yaml`. `vmuc create --set path=value` and\n\
         `vmuc edit` change an instance's `vmuc.yaml`.\n",
    );
    for (title, fields) in SECTIONS {
        let _ = write!(
            out,
            "\n## {title}\n\n| Key | Default | Meaning |\n|---|---|---|\n"
        );
        for field in *fields {
            let cell = |s: &str| s.replace('|', "\\|");
            let doc = if field.alias_of.is_empty() {
                cell(field.doc)
            } else {
                format!("Another name for `{}`.", field.alias_of)
            };
            let _ = writeln!(out, "| `{}` | {} | {doc} |", field.key, cell(field.default));
        }
    }
    out
}

#[cfg(test)]
mod tests;
