// SPDX-License-Identifier: Apache-2.0
//! Built-in defaults and the four-layer merge.
//!
//! Layer order, lowest to highest priority:
//! built-in < `~/.vmuc/_config/default.yaml` < instance `vmuc.yaml` <
//! `~/.vmuc/_config/override.yaml`. The rules (Lima's `FillDefault`):
//!
//! * scalars: override > instance > user default > built-in;
//! * maps (`env`, `param`): merged default, then instance, then override;
//! * slices (`provision`, `probes`, `port_forwards`): override + instance +
//!   default, so a first-match search respects priority;
//! * `mounts`: appended default, instance, override but merged by
//!   location; the highest-priority `writable`/`mountPoint` wins;
//! * `dns` and `images`: the highest-priority non-empty list is taken
//!   whole.

use std::net::{IpAddr, Ipv4Addr};
use std::path::{Path, PathBuf};

use indexmap::IndexMap;
use serde::Serialize;

use crate::schema::{
    Arch, ByteSize, Digest, InstanceConfig, KernelRef, Mount, NetworkMode, PortForward, Probe,
    ProbeMode, Proto, Provision, ProvisionMode,
};
use crate::{Error, Result};

pub const DEFAULT_CPUS: u32 = 4;
/// Memory defaults to `min(DEFAULT_MEMORY_CAP, host RAM / 2)`.
pub const DEFAULT_MEMORY_CAP: ByteSize = ByteSize::from_gib(8);
pub const DEFAULT_DISK: ByteSize = ByteSize::from_gib(100);
pub const DEFAULT_USER: &str = "vmuc";
pub const DEFAULT_UID: u32 = 1000;
pub const DEFAULT_SHELL: &str = "/bin/bash";
pub const DEFAULT_TIMEZONE: &str = "UTC";
pub const DEFAULT_REPORT_DELAY_MS: u32 = 500;
pub const DEFAULT_MIN_AVAILABLE: ByteSize = ByteSize::from_mib(256);
pub const DEFAULT_DATA_OWNER: &str = "root:root";
pub const DEFAULT_DATA_PERMISSIONS: &str = "0644";

const LOOPBACK: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);
const ANY_V4: IpAddr = IpAddr::V4(Ipv4Addr::UNSPECIFIED);
const FULL_RANGE: [u16; 2] = [1, 65535];

/// The host account the default guest user is modelled on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostUser {
    pub name: String,
    pub uid: u32,
}

/// Host facts the built-in defaults derive from. Construct with
/// [`Builtin::detect`] in the CLI or literally in tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Builtin {
    /// Host RAM in bytes; `0` means unknown (the cap is used).
    pub host_ram: u64,
    /// The host account, already validated with [`HostUser`] rules.
    pub host_user: Option<HostUser>,
    /// IANA zone name of the host, if known.
    pub host_timezone: Option<String>,
    /// Host home directory, for `~` expansion in mount locations.
    pub host_home: PathBuf,
}

impl Builtin {
    /// Reads the host account, timezone and home directory. `host_ram` is
    /// passed in because its source is platform code outside this crate.
    pub fn detect(host_ram: u64) -> Result<Self> {
        let host_home = dirs::home_dir().ok_or(Error::NoHomeDir)?;
        let name = std::env::var("USER")
            .or_else(|_| std::env::var("LOGNAME"))
            .unwrap_or_default();
        // SAFETY: getuid has no preconditions and cannot fail.
        let uid = unsafe { libc::getuid() };
        let host_user = HostUser { name, uid };
        Ok(Self {
            host_ram,
            host_user: is_valid_guest_user(&host_user.name, host_user.uid).then_some(host_user),
            host_timezone: host_timezone(),
            host_home,
        })
    }

    /// Deterministic facts for tests and documentation.
    pub fn fixed(host_ram: u64, host_home: impl Into<PathBuf>) -> Self {
        Self {
            host_ram,
            host_user: None,
            host_timezone: None,
            host_home: host_home.into(),
        }
    }

    pub fn memory(&self) -> ByteSize {
        if self.host_ram == 0 {
            return DEFAULT_MEMORY_CAP;
        }
        ByteSize(DEFAULT_MEMORY_CAP.0.min(self.host_ram / 2))
    }

    pub fn user(&self) -> ResolvedUser {
        let (name, uid) = match &self.host_user {
            Some(u) if is_valid_guest_user(&u.name, u.uid) => (u.name.clone(), u.uid),
            _ => (DEFAULT_USER.to_owned(), DEFAULT_UID),
        };
        ResolvedUser {
            // `.linux` keeps the guest home apart from the mirrored host home.
            home: format!("/home/{name}.linux"),
            name,
            uid,
            shell: DEFAULT_SHELL.to_owned(),
            comment: String::new(),
            passwordless_sudo: true,
        }
    }

    pub fn timezone(&self) -> String {
        self.host_timezone
            .clone()
            .unwrap_or_else(|| DEFAULT_TIMEZONE.to_owned())
    }

    /// The host home, read-only, mirrored at the same path.
    pub fn mounts(&self) -> Vec<Mount> {
        vec![Mount {
            location: "~".to_owned(),
            mount_point: None,
            writable: Some(false),
        }]
    }

    fn expand_tilde(&self, location: &str) -> PathBuf {
        if location == "~" {
            self.host_home.clone()
        } else if let Some(rest) = location.strip_prefix("~/") {
            self.host_home.join(rest)
        } else {
            PathBuf::from(location)
        }
    }
}

/// Names that must not become the guest login user.
const RESERVED_USERS: &[&str] = &[
    "root",
    "daemon",
    "bin",
    "sys",
    "sync",
    "games",
    "man",
    "lp",
    "mail",
    "news",
    "uucp",
    "proxy",
    "backup",
    "list",
    "irc",
    "nobody",
    "admin",
    "adm",
    "ubuntu",
    "alpine",
    "operator",
    "shutdown",
    "halt",
    "ftp",
    "sshd",
    "ntp",
    "messagebus",
    "systemd-network",
    "systemd-resolve",
    "www-data",
];

/// Lima's rule: a POSIX user name that is not a system account, and a uid
/// outside the system range so it cannot collide with distro accounts.
pub fn is_valid_guest_user(name: &str, uid: u32) -> bool {
    let mut chars = name.chars();
    let first_ok = chars
        .next()
        .is_some_and(|c| c.is_ascii_lowercase() || c == '_');
    let rest_ok =
        chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-');
    first_ok
        && rest_ok
        && name.len() <= 32
        && !RESERVED_USERS.contains(&name)
        && (500..60000).contains(&uid)
}

fn host_timezone() -> Option<String> {
    if let Ok(target) = std::fs::read_link("/etc/localtime") {
        let target = target.to_string_lossy();
        if let Some((_, zone)) = target.split_once("zoneinfo/") {
            return Some(zone.to_owned());
        }
    }
    std::env::var("TZ").ok().filter(|tz| !tz.is_empty())
}

/// A deterministic locally administered unicast MAC derived from the
/// instance name, so an instance keeps its DHCP lease across restarts.
pub fn mac_address_for(name: &str) -> String {
    // FNV-1a 64; stability across releases matters more than quality here.
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in name.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    let b = h.to_le_bytes();
    format!("0a:56:4d:{:02x}:{:02x}:{:02x}", b[0], b[1], b[2])
}

fn is_valid_mac(mac: &str) -> bool {
    let parts: Vec<&str> = mac.split(':').collect();
    parts.len() == 6
        && parts
            .iter()
            .all(|p| p.len() == 2 && p.chars().all(|c| c.is_ascii_hexdigit()))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResolvedMount {
    pub location: PathBuf,
    pub mount_point: String,
    pub writable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResolvedSsh {
    pub load_dot_ssh_pub_keys: bool,
    pub forward_agent: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResolvedNetwork {
    pub mode: NetworkMode,
    /// Host interface; empty for `shared`.
    pub interface: String,
    /// Lower-case `xx:xx:xx:xx:xx:xx`.
    pub mac_address: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResolvedPortForward {
    pub guest_ip: IpAddr,
    pub guest_port_range: [u16; 2],
    pub host_ip: IpAddr,
    pub host_port_range: [u16; 2],
    pub proto: Proto,
    pub ignore: bool,
    pub is_static: bool,
}

impl ResolvedPortForward {
    /// The catch-all rule Lima appends: every guest listener on any address
    /// is forwarded to the same port on the host loopback.
    pub const fn catch_all() -> Self {
        Self {
            guest_ip: ANY_V4,
            guest_port_range: FULL_RANGE,
            host_ip: LOOPBACK,
            host_port_range: FULL_RANGE,
            proto: Proto::Any,
            ignore: false,
            is_static: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "mode", rename_all = "lowercase")]
pub enum ResolvedProvision {
    System { script: String },
    User { script: String },
    Boot { script: String },
    Data(DataFile),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DataFile {
    pub path: String,
    pub owner: String,
    pub permissions: String,
    pub content: String,
    pub overwrite: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResolvedProbe {
    pub mode: ProbeMode,
    pub script: String,
    pub description: String,
    pub hint: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResolvedUser {
    pub name: String,
    pub uid: u32,
    pub home: String,
    pub shell: String,
    pub comment: String,
    pub passwordless_sudo: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResolvedMemoryPolicy {
    pub pressure_balloon: bool,
    pub report_delay_ms: u32,
    pub min_available: ByteSize,
}

/// A concrete image choice; `digest` and `kernel` stay optional because
/// "unpinned" and "no kernel override" are meaningful final states.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResolvedImage {
    pub location: String,
    pub arch: Arch,
    pub digest: Option<Digest>,
    pub kernel: Option<KernelRef>,
}

/// The fully layered, validated configuration of one instance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResolvedConfig {
    pub name: String,
    /// Candidates in order; the first one that downloads and verifies wins.
    pub images: Vec<ResolvedImage>,
    pub cpus: u32,
    pub memory: ByteSize,
    pub disk: ByteSize,
    pub mounts: Vec<ResolvedMount>,
    pub ssh: ResolvedSsh,
    pub network: ResolvedNetwork,
    /// Highest priority first; ends with [`ResolvedPortForward::catch_all`].
    pub port_forwards: Vec<ResolvedPortForward>,
    pub provision: Vec<ResolvedProvision>,
    pub probes: Vec<ResolvedProbe>,
    pub env: IndexMap<String, String>,
    pub param: IndexMap<String, String>,
    pub user: ResolvedUser,
    pub timezone: String,
    pub memory_policy: ResolvedMemoryPolicy,
    pub message: String,
    pub dns: Vec<IpAddr>,
    pub host_resolver: bool,
}

impl ResolvedConfig {
    /// Guest hostname: `vmuc-<name>`.
    pub fn hostname(&self) -> String {
        format!("vmuc-{}", self.name)
    }
}

/// The three YAML layers; `pick` applies the scalar rule across them.
struct Layers<'a> {
    y: &'a InstanceConfig,
    d: &'a InstanceConfig,
    o: &'a InstanceConfig,
}

impl Layers<'_> {
    /// override > instance > user default.
    fn pick<T: Clone>(&self, get: impl Fn(&InstanceConfig) -> Option<&T>) -> Option<T> {
        get(self.o).or(get(self.y)).or(get(self.d)).cloned()
    }

    /// Default, then instance, then override entries; later keys win.
    fn merge_map(
        &self,
        get: impl Fn(&InstanceConfig) -> &IndexMap<String, String>,
    ) -> IndexMap<String, String> {
        let mut out = get(self.d).clone();
        for layer in [self.y, self.o] {
            out.extend(get(layer).iter().map(|(k, v)| (k.clone(), v.clone())));
        }
        out
    }

    /// The highest-priority non-empty list, whole.
    fn first_non_empty<T: Clone>(&self, get: impl Fn(&InstanceConfig) -> &Vec<T>) -> Vec<T> {
        [self.o, self.y, self.d]
            .into_iter()
            .map(get)
            .find(|l| !l.is_empty())
            .cloned()
            .unwrap_or_default()
    }

    /// override + instance + default, each entry resolved with its index.
    fn concat<T, R>(
        &self,
        get: impl Fn(&InstanceConfig) -> &Vec<T>,
        resolve: impl Fn(&T, usize) -> Result<R>,
    ) -> Result<Vec<R>> {
        [self.o, self.y, self.d]
            .into_iter()
            .flat_map(get)
            .enumerate()
            .map(|(i, item)| resolve(item, i))
            .collect()
    }
}

/// Layers `instance` over `user_default`, under `override_`, on top of the
/// built-in defaults, and validates the result. See the module docs for
/// the per-field rules.
pub fn fill_defaults(
    name: &str,
    instance: &InstanceConfig,
    user_default: Option<&InstanceConfig>,
    override_: Option<&InstanceConfig>,
    builtin: &Builtin,
) -> Result<ResolvedConfig> {
    let empty = InstanceConfig::default();
    let l = Layers {
        y: instance,
        d: user_default.unwrap_or(&empty),
        o: override_.unwrap_or(&empty),
    };

    let cpus = l.pick(|c| c.cpus.as_ref()).unwrap_or(DEFAULT_CPUS);
    if cpus == 0 {
        return Err(Error::invalid("cpus", "must be at least 1"));
    }
    let memory = l
        .pick(|c| c.memory.as_ref())
        .unwrap_or_else(|| builtin.memory());
    if memory.0 < 64 * ByteSize::MIB {
        return Err(Error::invalid("memory", "must be at least 64MiB"));
    }
    let disk = l.pick(|c| c.disk.as_ref()).unwrap_or(DEFAULT_DISK);
    if disk.0 < ByteSize::GIB {
        return Err(Error::invalid("disk", "must be at least 1GiB"));
    }
    let env = l.merge_map(|c| &c.env);
    let param = l.merge_map(|c| &c.param);
    for key in env.keys().chain(param.keys()) {
        if !is_identifier(key) {
            return Err(Error::invalid(
                format!("env/param key {key:?}"),
                "must match [A-Za-z_][A-Za-z0-9_]* (keys become shell variables)",
            ));
        }
    }
    let mut port_forwards = l.concat(|c| &c.port_forwards, resolve_port_forward)?;
    port_forwards.push(ResolvedPortForward::catch_all());

    Ok(ResolvedConfig {
        name: name.to_owned(),
        images: l
            .first_non_empty(|c| &c.images)
            .into_iter()
            .map(|i| ResolvedImage {
                location: i.location,
                arch: i.arch,
                digest: i.digest,
                kernel: i.kernel,
            })
            .collect(),
        cpus,
        memory,
        disk,
        mounts: resolve_mounts(
            builtin,
            [&builtin.mounts(), &l.d.mounts, &l.y.mounts, &l.o.mounts],
        )?,
        ssh: ResolvedSsh {
            load_dot_ssh_pub_keys: l
                .pick(|c| c.ssh.load_dot_ssh_pub_keys.as_ref())
                .unwrap_or(false),
            forward_agent: l.pick(|c| c.ssh.forward_agent.as_ref()).unwrap_or(false),
        },
        network: resolve_network(name, &l)?,
        port_forwards,
        provision: l.concat(|c| &c.provision, resolve_provision)?,
        probes: l.concat(|c| &c.probes, resolve_probe)?,
        env,
        param,
        user: resolve_user(builtin, &l)?,
        timezone: l
            .pick(|c| c.timezone.as_ref())
            .unwrap_or_else(|| builtin.timezone()),
        memory_policy: ResolvedMemoryPolicy {
            pressure_balloon: l
                .pick(|c| c.memory_policy.pressure_balloon.as_ref())
                .unwrap_or(false),
            report_delay_ms: l
                .pick(|c| c.memory_policy.report_delay_ms.as_ref())
                .unwrap_or(DEFAULT_REPORT_DELAY_MS),
            min_available: l
                .pick(|c| c.memory_policy.min_available.as_ref())
                .unwrap_or(DEFAULT_MIN_AVAILABLE),
        },
        message: l.pick(|c| c.message.as_ref()).unwrap_or_default(),
        dns: l.first_non_empty(|c| &c.dns),
        host_resolver: l.pick(|c| c.host_resolver.as_ref()).unwrap_or(true),
    })
}

fn is_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Appends `layers` lowest priority first, merging entries whose expanded
/// location matches; a later layer's set fields win.
fn resolve_mounts(builtin: &Builtin, layers: [&Vec<Mount>; 4]) -> Result<Vec<ResolvedMount>> {
    struct Partial {
        location: PathBuf,
        mount_point: Option<String>,
        writable: Option<bool>,
    }
    let mut merged: Vec<Partial> = Vec::new();
    for mount in layers.into_iter().flatten() {
        let location = builtin.expand_tilde(&mount.location);
        if !location.is_absolute() {
            return Err(Error::invalid(
                format!("mounts[{}].location", mount.location),
                "must be an absolute path or start with ~",
            ));
        }
        if let Some(mp) = &mount.mount_point
            && !Path::new(mp).is_absolute()
        {
            return Err(Error::invalid(
                format!("mounts[{}].mountPoint", mount.location),
                "must be an absolute guest path",
            ));
        }
        if let Some(existing) = merged.iter_mut().find(|m| m.location == location) {
            if mount.mount_point.is_some() {
                existing.mount_point.clone_from(&mount.mount_point);
            }
            if mount.writable.is_some() {
                existing.writable = mount.writable;
            }
        } else {
            merged.push(Partial {
                location,
                mount_point: mount.mount_point.clone(),
                writable: mount.writable,
            });
        }
    }
    let mut out = Vec::with_capacity(merged.len());
    for m in merged {
        let mount_point = m
            .mount_point
            .unwrap_or_else(|| m.location.to_string_lossy().into_owned());
        if out
            .iter()
            .any(|r: &ResolvedMount| r.mount_point == mount_point)
        {
            return Err(Error::invalid(
                "mounts",
                format!("mount point {mount_point} is used by more than one mount"),
            ));
        }
        out.push(ResolvedMount {
            location: m.location,
            mount_point,
            writable: m.writable.unwrap_or(false),
        });
    }
    Ok(out)
}

fn resolve_network(name: &str, l: &Layers<'_>) -> Result<ResolvedNetwork> {
    let mode = l.pick(|c| c.network.mode.as_ref()).unwrap_or_default();
    let interface = l.pick(|c| c.network.interface.as_ref()).unwrap_or_default();
    if mode == NetworkMode::Bridged && interface.is_empty() {
        return Err(Error::invalid(
            "network.interface",
            "required when network.mode is bridged",
        ));
    }
    let mac_address = l
        .pick(|c| c.network.mac_address.as_ref())
        .map_or_else(|| mac_address_for(name), |m| m.to_ascii_lowercase());
    if !is_valid_mac(&mac_address) {
        return Err(Error::invalid(
            "network.macAddress",
            format!("{mac_address:?} is not of the form xx:xx:xx:xx:xx:xx"),
        ));
    }
    Ok(ResolvedNetwork {
        mode,
        interface,
        mac_address,
    })
}

fn resolve_user(builtin: &Builtin, l: &Layers<'_>) -> Result<ResolvedUser> {
    let base = builtin.user();
    let name = l.pick(|c| c.user.name.as_ref()).unwrap_or(base.name);
    let uid = l.pick(|c| c.user.uid.as_ref()).unwrap_or(base.uid);
    if !is_valid_guest_user(&name, uid) {
        return Err(Error::invalid(
            "user",
            format!(
                "{name}:{uid} is not a valid guest login (lower-case POSIX name, not a system account, uid 500..60000)"
            ),
        ));
    }
    let home = match l.pick(|c| c.user.home.as_ref()) {
        Some(h) if !Path::new(&h).is_absolute() => {
            return Err(Error::invalid(
                "user.home",
                "must be an absolute guest path",
            ));
        }
        Some(h) => h,
        None => format!("/home/{name}.linux"),
    };
    Ok(ResolvedUser {
        name,
        uid,
        home,
        shell: l.pick(|c| c.user.shell.as_ref()).unwrap_or(base.shell),
        comment: l.pick(|c| c.user.comment.as_ref()).unwrap_or_default(),
        passwordless_sudo: l
            .pick(|c| c.user.passwordless_sudo.as_ref())
            .unwrap_or(base.passwordless_sudo),
    })
}

fn resolve_port_forward(pf: &PortForward, index: usize) -> Result<ResolvedPortForward> {
    let field = |f: &str| format!("portForwards[{index}].{f}");
    let range =
        |port: Option<u16>, range: Option<[u16; 2]>, what: &str| -> Result<Option<[u16; 2]>> {
            match (port, range) {
                (Some(_), Some(_)) => Err(Error::invalid(
                    field(&format!("{what}Port")),
                    format!("{what}Port and {what}PortRange are mutually exclusive"),
                )),
                (Some(p), None) => Ok(Some([p, p])),
                (None, Some([lo, hi])) if lo == 0 || lo > hi => Err(Error::invalid(
                    field(&format!("{what}PortRange")),
                    "must be [low, high] with 1 <= low <= high",
                )),
                (None, Some(r)) => Ok(Some(r)),
                (None, None) => Ok(None),
            }
        };
    let guest_port_range =
        range(pf.guest_port, pf.guest_port_range, "guest")?.unwrap_or(FULL_RANGE);
    let host_port_range =
        range(pf.host_port, pf.host_port_range, "host")?.unwrap_or(guest_port_range);
    if u32::from(host_port_range[1]) - u32::from(host_port_range[0])
        != u32::from(guest_port_range[1]) - u32::from(guest_port_range[0])
    {
        return Err(Error::invalid(
            field("hostPortRange"),
            "must span as many ports as the guest range",
        ));
    }
    Ok(ResolvedPortForward {
        guest_ip: pf.guest_ip.unwrap_or(LOOPBACK),
        guest_port_range,
        host_ip: pf.host_ip.unwrap_or(LOOPBACK),
        host_port_range,
        proto: pf.proto.unwrap_or_default(),
        ignore: pf.ignore,
        is_static: pf.is_static,
    })
}

fn resolve_provision(p: &Provision, index: usize) -> Result<ResolvedProvision> {
    let field = |f: &str| format!("provision[{index}].{f}");
    let script = || {
        p.script
            .clone()
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| Error::invalid(field("script"), "required for this mode"))
    };
    if p.mode != ProvisionMode::Data
        && (p.path.is_some() || p.owner.is_some() || p.permissions.is_some() || p.content.is_some())
    {
        return Err(Error::invalid(
            field("mode"),
            "path/owner/permissions/content are only valid with mode: data",
        ));
    }
    Ok(match p.mode {
        ProvisionMode::System => ResolvedProvision::System { script: script()? },
        ProvisionMode::User => ResolvedProvision::User { script: script()? },
        ProvisionMode::Boot => ResolvedProvision::Boot { script: script()? },
        ProvisionMode::Data => {
            if p.script.is_some() {
                return Err(Error::invalid(field("script"), "not valid with mode: data"));
            }
            let path = p
                .path
                .clone()
                .ok_or_else(|| Error::invalid(field("path"), "required for mode: data"))?;
            if !Path::new(&path).is_absolute() {
                return Err(Error::invalid(
                    field("path"),
                    "must be an absolute guest path",
                ));
            }
            let permissions = p
                .permissions
                .clone()
                .unwrap_or_else(|| DEFAULT_DATA_PERMISSIONS.to_owned());
            if permissions.len() > 4 || !permissions.chars().all(|c| ('0'..='7').contains(&c)) {
                return Err(Error::invalid(
                    field("permissions"),
                    "must be octal, e.g. \"0644\"",
                ));
            }
            ResolvedProvision::Data(DataFile {
                path,
                owner: p
                    .owner
                    .clone()
                    .unwrap_or_else(|| DEFAULT_DATA_OWNER.to_owned()),
                permissions,
                content: p
                    .content
                    .clone()
                    .ok_or_else(|| Error::invalid(field("content"), "required for mode: data"))?,
                overwrite: p.overwrite.unwrap_or(true),
            })
        }
    })
}

fn resolve_probe(p: &Probe, index: usize) -> Result<ResolvedProbe> {
    let script = p
        .script
        .clone()
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| Error::invalid(format!("probes[{index}].script"), "required"))?;
    Ok(ResolvedProbe {
        mode: p.mode,
        script,
        description: p.description.clone().unwrap_or_default(),
        hint: p.hint.clone().unwrap_or_default(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{MemoryPolicy, Network, Ssh, User};

    const HOST_RAM_32G: u64 = 32 * ByteSize::GIB;

    fn builtin() -> Builtin {
        Builtin::fixed(HOST_RAM_32G, "/Users/alice")
    }

    fn yaml(text: &str) -> InstanceConfig {
        InstanceConfig::from_yaml(text, "test").unwrap()
    }

    fn resolve(y: &str, d: Option<&str>, o: Option<&str>) -> Result<ResolvedConfig> {
        let y = yaml(y);
        let d = d.map(yaml);
        let o = o.map(yaml);
        fill_defaults("demo", &y, d.as_ref(), o.as_ref(), &builtin())
    }

    #[test]
    fn builtin_defaults_apply_to_an_empty_config() {
        let r = resolve("", None, None).unwrap();
        assert_eq!(r.name, "demo");
        assert_eq!(r.hostname(), "vmuc-demo");
        assert_eq!(r.cpus, DEFAULT_CPUS);
        assert_eq!(r.memory, ByteSize::from_gib(8));
        assert_eq!(r.disk, DEFAULT_DISK);
        assert_eq!(r.mounts.len(), 1);
        assert_eq!(r.mounts[0].location, PathBuf::from("/Users/alice"));
        assert_eq!(r.mounts[0].mount_point, "/Users/alice");
        assert!(!r.mounts[0].writable);
        assert_eq!(r.user.name, DEFAULT_USER);
        assert_eq!(r.user.uid, DEFAULT_UID);
        assert_eq!(r.user.home, "/home/vmuc.linux");
        assert_eq!(r.user.shell, DEFAULT_SHELL);
        assert!(r.user.passwordless_sudo);
        assert_eq!(r.timezone, DEFAULT_TIMEZONE);
        assert_eq!(r.network.mode, NetworkMode::Shared);
        assert_eq!(r.network.mac_address, mac_address_for("demo"));
        assert!(!r.memory_policy.pressure_balloon);
        assert_eq!(r.memory_policy.report_delay_ms, DEFAULT_REPORT_DELAY_MS);
        assert_eq!(r.memory_policy.min_available, DEFAULT_MIN_AVAILABLE);
        assert!(!r.ssh.load_dot_ssh_pub_keys);
        assert!(!r.ssh.forward_agent);
        assert!(r.host_resolver);
        assert_eq!(r.dns, [] as [IpAddr; 0]);
        assert_eq!(r.images, [] as [ResolvedImage; 0]);
        assert_eq!(r.port_forwards, vec![ResolvedPortForward::catch_all()]);
        assert_eq!(r.message, "");
    }

    #[test]
    fn memory_default_is_half_host_ram_capped_at_8gib() {
        assert_eq!(
            Builtin::fixed(HOST_RAM_32G, "/h").memory(),
            ByteSize::from_gib(8)
        );
        assert_eq!(
            Builtin::fixed(8 * ByteSize::GIB, "/h").memory(),
            ByteSize::from_gib(4)
        );
        assert_eq!(Builtin::fixed(0, "/h").memory(), ByteSize::from_gib(8));
    }

    #[test]
    fn host_user_is_used_when_valid() {
        let mut b = builtin();
        b.host_user = Some(HostUser {
            name: "alice".into(),
            uid: 501,
        });
        let u = b.user();
        assert_eq!(
            (u.name.as_str(), u.uid, u.home.as_str()),
            ("alice", 501, "/home/alice.linux")
        );
        for (name, uid) in [
            ("root", 0),
            ("Alice", 501),
            ("alice", 499),
            ("ubuntu", 1000),
            ("", 501),
        ] {
            b.host_user = Some(HostUser {
                name: name.into(),
                uid,
            });
            assert_eq!(b.user().name, DEFAULT_USER, "{name}:{uid}");
        }
    }

    #[test]
    fn scalars_follow_override_instance_default_builtin() {
        let r = resolve("cpus: 2", Some("cpus: 1"), Some("cpus: 3")).unwrap();
        assert_eq!(r.cpus, 3);
        let r = resolve("cpus: 2", Some("cpus: 1"), None).unwrap();
        assert_eq!(r.cpus, 2);
        let r = resolve("", Some("cpus: 1"), None).unwrap();
        assert_eq!(r.cpus, 1);
        let r = resolve("", Some("memory: 1GiB"), Some("timezone: Europe/Berlin")).unwrap();
        assert_eq!(r.memory, ByteSize::from_gib(1));
        assert_eq!(r.timezone, "Europe/Berlin");
        // Nested scalars follow the same rule.
        let r = resolve(
            "user: {shell: /bin/zsh}\nmemoryPolicy: {reportDelayMs: 100}",
            Some("user: {name: bob, uid: 1001}\nmemoryPolicy: {reportDelayMs: 1, minAvailable: 1GiB}"),
            Some("user: {name: carol}"),
        )
        .unwrap();
        assert_eq!(r.user.name, "carol");
        assert_eq!(r.user.uid, 1001);
        assert_eq!(r.user.shell, "/bin/zsh");
        assert_eq!(r.memory_policy.report_delay_ms, 100);
        assert_eq!(r.memory_policy.min_available, ByteSize::from_gib(1));
    }

    #[test]
    fn maps_merge_default_then_instance_then_override() {
        let r = resolve(
            "env: {A: y, B: y}\nparam: {P: y}",
            Some("env: {A: d, C: d}\nparam: {P: d, Q: d}"),
            Some("env: {B: o}"),
        )
        .unwrap();
        let env: Vec<(&str, &str)> = r
            .env
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        assert_eq!(env, [("A", "y"), ("C", "d"), ("B", "o")]);
        assert_eq!(r.param["P"], "y");
        assert_eq!(r.param["Q"], "d");
        let err = resolve("param: {bad-key: x}", None, None).unwrap_err();
        assert!(err.to_string().contains("bad-key"), "{err}");
        assert!(resolve("env: {1X: x}", None, None).is_err());
    }

    #[test]
    fn slices_concatenate_override_instance_default() {
        let r = resolve(
            "provision: [{script: y}]\nprobes: [{script: y}]\nportForwards: [{guestPort: 2}]",
            Some("provision: [{script: d}]\nprobes: [{script: d}]\nportForwards: [{guestPort: 1}]"),
            Some("provision: [{script: o}]\nprobes: [{script: o}]\nportForwards: [{guestPort: 3}]"),
        )
        .unwrap();
        let scripts: Vec<&str> = r
            .provision
            .iter()
            .map(|p| match p {
                ResolvedProvision::System { script } => script.as_str(),
                _ => unreachable!(),
            })
            .collect();
        assert_eq!(scripts, ["o", "y", "d"]);
        let probes: Vec<&str> = r.probes.iter().map(|p| p.script.as_str()).collect();
        assert_eq!(probes, ["o", "y", "d"]);
        let ports: Vec<u16> = r
            .port_forwards
            .iter()
            .map(|p| p.guest_port_range[0])
            .collect();
        assert_eq!(ports, [3, 2, 1, 1]);
        assert_eq!(r.port_forwards[3], ResolvedPortForward::catch_all());
    }

    #[test]
    fn dns_and_images_take_the_highest_priority_non_empty_list() {
        let r = resolve("dns: [1.1.1.1]", Some("dns: [9.9.9.9, 8.8.8.8]"), None).unwrap();
        assert_eq!(r.dns, ["1.1.1.1".parse::<IpAddr>().unwrap()]);
        let r = resolve("", Some("dns: [9.9.9.9, 8.8.8.8]"), None).unwrap();
        assert_eq!(r.dns.len(), 2);
        let r = resolve("dns: [1.1.1.1]", None, Some("dns: [2.2.2.2]")).unwrap();
        assert_eq!(r.dns, ["2.2.2.2".parse::<IpAddr>().unwrap()]);
        let r = resolve(
            "images: [{location: y}]",
            Some("images: [{location: d1}, {location: d2}]"),
            None,
        )
        .unwrap();
        assert_eq!(r.images.len(), 1);
        assert_eq!(r.images[0].location, "y");
        let r = resolve("", Some("images: [{location: d1}, {location: d2}]"), None).unwrap();
        assert_eq!(r.images.len(), 2);
    }

    #[test]
    fn mounts_merge_by_location_with_highest_priority_writable() {
        let r = resolve(
            "mounts: [{location: '~', writable: true}, {location: /tmp/y}]",
            Some("mounts: [{location: /tmp/d, writable: true}]"),
            Some("mounts: [{location: /tmp/d, writable: false, mountPoint: /mnt/d}]"),
        )
        .unwrap();
        let got: Vec<(&str, &str, bool)> = r
            .mounts
            .iter()
            .map(|m| {
                (
                    m.location.to_str().unwrap(),
                    m.mount_point.as_str(),
                    m.writable,
                )
            })
            .collect();
        assert_eq!(
            got,
            [
                ("/Users/alice", "/Users/alice", true),
                ("/tmp/d", "/mnt/d", false),
                ("/tmp/y", "/tmp/y", false),
            ]
        );
        // ~ is written back read-only by the override even though the instance said writable.
        let r = resolve(
            "mounts: [{location: '~', writable: true}]",
            None,
            Some("mounts: [{location: '~/'}]"),
        )
        .unwrap();
        assert!(
            r.mounts[0].writable,
            "unset writable in override keeps the instance value"
        );
        let r = resolve(
            "mounts: [{location: '~', writable: true}]",
            None,
            Some("mounts: [{location: '~', writable: false}]"),
        )
        .unwrap();
        assert!(!r.mounts[0].writable);
        assert!(resolve("mounts: [{location: relative}]", None, None).is_err());
        assert!(resolve("mounts: [{location: /a, mountPoint: rel}]", None, None).is_err());
        assert!(
            resolve(
                "mounts: [{location: /a, mountPoint: /x}, {location: /b, mountPoint: /x}]",
                None,
                None
            )
            .is_err()
        );
    }

    #[test]
    fn port_forward_defaults_and_validation() {
        let r = resolve(
            "portForwards:\n- guestPortRange: [8000, 8010]\n  hostIp: 0.0.0.0\n  proto: udp\n- guestPort: 80\n  hostPort: 8080\n  ignore: true",
            None,
            None,
        )
        .unwrap();
        let a = &r.port_forwards[0];
        assert_eq!(a.guest_ip, LOOPBACK);
        assert_eq!(a.guest_port_range, [8000, 8010]);
        assert_eq!(a.host_port_range, [8000, 8010]);
        assert_eq!(a.host_ip, ANY_V4);
        assert_eq!(a.proto, Proto::Udp);
        let b = &r.port_forwards[1];
        assert_eq!(b.host_port_range, [8080, 8080]);
        assert!(b.ignore);
        for bad in [
            "portForwards: [{guestPort: 1, guestPortRange: [1, 2]}]",
            "portForwards: [{guestPortRange: [5, 2]}]",
            "portForwards: [{guestPortRange: [0, 2]}]",
            "portForwards: [{guestPortRange: [1, 2], hostPortRange: [1, 3]}]",
        ] {
            assert!(resolve(bad, None, None).is_err(), "{bad}");
        }
    }

    #[test]
    fn provision_validation() {
        let r = resolve(
            "provision:\n- mode: data\n  path: /etc/x\n  content: hi\n- mode: user\n  script: whoami\n- mode: boot\n  script: 'true'",
            None,
            None,
        )
        .unwrap();
        assert_eq!(
            r.provision[0],
            ResolvedProvision::Data(DataFile {
                path: "/etc/x".into(),
                owner: DEFAULT_DATA_OWNER.into(),
                permissions: DEFAULT_DATA_PERMISSIONS.into(),
                content: "hi".into(),
                overwrite: true,
            })
        );
        assert!(matches!(r.provision[1], ResolvedProvision::User { .. }));
        assert!(matches!(r.provision[2], ResolvedProvision::Boot { .. }));
        for bad in [
            "provision: [{mode: system}]",
            "provision: [{mode: system, script: '  '}]",
            "provision: [{mode: system, script: x, path: /y}]",
            "provision: [{mode: data, content: x}]",
            "provision: [{mode: data, path: rel, content: x}]",
            "provision: [{mode: data, path: /x}]",
            "provision: [{mode: data, path: /x, content: y, script: z}]",
            "provision: [{mode: data, path: /x, content: y, permissions: rw}]",
            "probes: [{hint: no script}]",
        ] {
            assert!(resolve(bad, None, None).is_err(), "{bad}");
        }
    }

    #[test]
    fn network_validation() {
        let r = resolve("network: {macAddress: 'AA:BB:CC:DD:EE:FF'}", None, None).unwrap();
        assert_eq!(r.network.mac_address, "aa:bb:cc:dd:ee:ff");
        assert!(resolve("network: {mode: bridged}", None, None).is_err());
        let r = resolve("network: {mode: bridged, interface: en0}", None, None).unwrap();
        assert_eq!(r.network.interface, "en0");
        assert!(resolve("network: {macAddress: nope}", None, None).is_err());
        assert_ne!(mac_address_for("a"), mac_address_for("b"));
        assert!(is_valid_mac(&mac_address_for("x")));
        assert!(resolve("user: {name: root}", None, None).is_err());
        assert!(resolve("user: {home: rel}", None, None).is_err());
        assert!(resolve("cpus: 0", None, None).is_err());
        assert!(resolve("memory: 1MiB", None, None).is_err());
        assert!(resolve("disk: 1MiB", None, None).is_err());
    }

    #[test]
    fn fully_set_instance_ignores_lower_layers() {
        let y = InstanceConfig {
            cpus: Some(7),
            memory: Some(ByteSize::from_gib(3)),
            disk: Some(ByteSize::from_gib(20)),
            ssh: Ssh {
                load_dot_ssh_pub_keys: Some(true),
                forward_agent: Some(true),
            },
            network: Network {
                mode: Some(NetworkMode::Shared),
                interface: None,
                mac_address: Some("02:00:00:00:00:01".into()),
            },
            user: User {
                name: Some("dev".into()),
                uid: Some(2000),
                home: Some("/home/dev".into()),
                shell: Some("/bin/sh".into()),
                comment: Some("Dev".into()),
                passwordless_sudo: Some(false),
            },
            timezone: Some("Asia/Tokyo".into()),
            memory_policy: MemoryPolicy {
                pressure_balloon: Some(true),
                report_delay_ms: Some(50),
                min_available: Some(ByteSize::from_mib(100)),
            },
            message: Some("hi".into()),
            host_resolver: Some(false),
            ..Default::default()
        };
        let d = yaml("cpus: 1\nuser: {name: other}\nhostResolver: true");
        let r = fill_defaults("x", &y, Some(&d), None, &builtin()).unwrap();
        assert_eq!(r.cpus, 7);
        assert!(r.ssh.load_dot_ssh_pub_keys && r.ssh.forward_agent);
        assert_eq!(r.user.name, "dev");
        assert_eq!(r.user.home, "/home/dev");
        assert!(!r.user.passwordless_sudo);
        assert_eq!(r.user.comment, "Dev");
        assert_eq!(r.timezone, "Asia/Tokyo");
        assert!(r.memory_policy.pressure_balloon);
        assert_eq!(r.message, "hi");
        assert!(!r.host_resolver);
    }
}
