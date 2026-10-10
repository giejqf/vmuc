// SPDX-License-Identifier: Apache-2.0
//! The instance YAML schema.
//!
//! Every field is an `Option` or a collection so that the layering in
//! [`crate::defaults`] can tell "unset" from "set to the default". Keys are
//! camelCase in YAML (Lima's convention) and unknown keys are rejected.

use std::fmt;
use std::net::IpAddr;
use std::str::FromStr;

use indexmap::IndexMap;
use serde::de::{self, Deserializer};
use serde::{Deserialize, Serialize};

use crate::Error;

/// Byte count with binary-unit parsing and printing (`8GiB`, `512M`, `1024`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct ByteSize(pub u64);

impl ByteSize {
    pub const KIB: u64 = 1 << 10;
    pub const MIB: u64 = 1 << 20;
    pub const GIB: u64 = 1 << 30;

    pub const fn from_mib(mib: u64) -> Self {
        Self(mib * Self::MIB)
    }

    pub const fn from_gib(gib: u64) -> Self {
        Self(gib * Self::GIB)
    }

    pub const fn bytes(self) -> u64 {
        self.0
    }
}

impl FromStr for ByteSize {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Error> {
        let err = || Error::InvalidByteSize(s.to_owned());
        let trimmed = s.trim();
        let num_end = trimmed
            .find(|c: char| !(c.is_ascii_digit() || c == '.'))
            .unwrap_or(trimmed.len());
        let (num, unit) = trimmed.split_at(num_end);
        if num.is_empty() || num == "." {
            return Err(err());
        }
        let unit = unit.trim().to_ascii_lowercase();
        let unit = unit.strip_suffix('b').unwrap_or(&unit);
        let unit = unit.strip_suffix('i').unwrap_or(unit);
        let shift: u32 = match unit {
            "" => 0,
            "k" => 10,
            "m" => 20,
            "g" => 30,
            "t" => 40,
            "p" => 50,
            _ => return Err(err()),
        };
        let (int_part, frac_part) = num.split_once('.').unwrap_or((num, ""));
        let int_part: u128 = if int_part.is_empty() {
            0
        } else {
            int_part.parse().map_err(|_| err())?
        };
        let mut total = int_part << shift;
        if !frac_part.is_empty() {
            let frac: u128 = frac_part.parse().map_err(|_| err())?;
            let scale = 10u128.pow(u32::try_from(frac_part.len()).map_err(|_| err())?);
            total += (frac << shift) / scale;
        }
        u64::try_from(total).map(ByteSize).map_err(|_| err())
    }
}

impl fmt::Display for ByteSize {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.0 == 0 {
            return write!(f, "0");
        }
        for (shift, unit) in [
            (50, "PiB"),
            (40, "TiB"),
            (30, "GiB"),
            (20, "MiB"),
            (10, "KiB"),
        ] {
            if self.0.is_multiple_of(1u64 << shift) {
                return write!(f, "{}{unit}", self.0 >> shift);
            }
        }
        write!(f, "{}B", self.0)
    }
}

impl Serialize for ByteSize {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for ByteSize {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Bytes(u64),
            Text(String),
        }
        match Raw::deserialize(d)? {
            Raw::Bytes(b) => Ok(ByteSize(b)),
            Raw::Text(t) => t.parse().map_err(de::Error::custom),
        }
    }
}

/// Digest algorithm of a [`Digest`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DigestAlgorithm {
    Sha256,
    Sha512,
}

impl DigestAlgorithm {
    const fn hex_len(self) -> usize {
        match self {
            Self::Sha256 => 64,
            Self::Sha512 => 128,
        }
    }

    const fn name(self) -> &'static str {
        match self {
            Self::Sha256 => "sha256",
            Self::Sha512 => "sha512",
        }
    }
}

/// A content digest in `algorithm:hex` form.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Digest {
    pub algorithm: DigestAlgorithm,
    /// Lower-case hex.
    pub hex: String,
}

impl FromStr for Digest {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Error> {
        let err = || Error::InvalidDigest(s.to_owned());
        let (algo, hex) = s.split_once(':').ok_or_else(err)?;
        let algorithm = match algo {
            "sha256" => DigestAlgorithm::Sha256,
            "sha512" => DigestAlgorithm::Sha512,
            _ => return Err(err()),
        };
        if hex.len() != algorithm.hex_len() || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(err());
        }
        Ok(Self {
            algorithm,
            hex: hex.to_ascii_lowercase(),
        })
    }
}

impl fmt::Display for Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.algorithm.name(), self.hex)
    }
}

impl Serialize for Digest {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Digest {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        String::deserialize(d)?.parse().map_err(de::Error::custom)
    }
}

/// Guest CPU architecture. Only arm64 is supported (requirement 2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum Arch {
    #[default]
    #[serde(rename = "aarch64", alias = "arm64")]
    Aarch64,
}

/// A kernel override for an image entry.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct KernelRef {
    pub location: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub digest: Option<Digest>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cmdline: Option<String>,
}

/// A rootfs image: URL or local path, with an optional pinned digest.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ImageRef {
    pub location: String,
    #[serde(default)]
    pub arch: Arch,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub digest: Option<Digest>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kernel: Option<KernelRef>,
}

/// A host directory shared into the guest over virtiofs.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Mount {
    /// Host path; a leading `~` is expanded to the host home directory.
    pub location: String,
    /// Guest path; defaults to the expanded host path (mirrored).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mount_point: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub writable: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Ssh {
    /// Load `~/.ssh/*.pub` in addition to `~/.vmuc/_config/user.pub`.
    #[serde(
        default,
        rename = "loadDotSSHPubKeys",
        skip_serializing_if = "Option::is_none"
    )]
    pub load_dot_ssh_pub_keys: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub forward_agent: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NetworkMode {
    /// User-space NAT (the only mode implemented in v0.1).
    #[default]
    Shared,
    /// Bridged onto a host interface (out of scope for v0.1; reserved).
    Bridged,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Network {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<NetworkMode>,
    /// Host interface for `bridged`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interface: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mac_address: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Proto {
    Tcp,
    Udp,
    #[default]
    Any,
}

/// A port-forwarding rule, Lima's shape: the first matching rule wins.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PortForward {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guest_ip: Option<IpAddr>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guest_port: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guest_port_range: Option<[u16; 2]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_ip: Option<IpAddr>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_port: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_port_range: Option<[u16; 2]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proto: Option<Proto>,
    /// Matching guest ports are not forwarded.
    #[serde(default, skip_serializing_if = "is_false")]
    pub ignore: bool,
    /// Forward even when nothing listens in the guest.
    #[serde(default, rename = "static", skip_serializing_if = "is_false")]
    pub is_static: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProvisionMode {
    /// Runs as root on every boot after the agent is up.
    #[default]
    System,
    /// Runs as the instance user on every boot.
    User,
    /// Runs early on every boot via cloud-init `bootcmd`.
    Boot,
    /// Copies `content` to `path` instead of running a script.
    Data,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Provision {
    #[serde(default)]
    pub mode: ProvisionMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub script: Option<String>,
    // The following apply to `mode: data` only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permissions: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overwrite: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProbeMode {
    #[default]
    Readiness,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Probe {
    #[serde(default)]
    pub mode: ProbeMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub script: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct User {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uid: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub home: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shell: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub passwordless_sudo: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MemoryPolicy {
    /// Answer host memory pressure by inflating the balloon.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pressure_balloon: Option<bool>,
    /// Never balloon the guest below this much available memory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_available: Option<ByteSize>,
}

/// One instance (or template) YAML document.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields, default)]
pub struct InstanceConfig {
    /// Template references flattened at create time; see [`crate::template`].
    #[serde(
        deserialize_with = "de_string_or_seq",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub base: Vec<String>,
    #[serde(
        deserialize_with = "de_null_default",
        alias = "image",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub images: Vec<ImageRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cpus: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory: Option<ByteSize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disk: Option<ByteSize>,
    #[serde(
        deserialize_with = "de_null_default",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub mounts: Vec<Mount>,
    #[serde(
        deserialize_with = "de_null_default",
        skip_serializing_if = "is_default"
    )]
    pub ssh: Ssh,
    #[serde(
        deserialize_with = "de_null_default",
        skip_serializing_if = "is_default"
    )]
    pub network: Network,
    #[serde(
        deserialize_with = "de_null_default",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub port_forwards: Vec<PortForward>,
    #[serde(
        deserialize_with = "de_null_default",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub provision: Vec<Provision>,
    #[serde(
        deserialize_with = "de_null_default",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub probes: Vec<Probe>,
    /// Environment for provisioning scripts and the login shell.
    #[serde(
        deserialize_with = "de_null_default",
        skip_serializing_if = "IndexMap::is_empty"
    )]
    pub env: IndexMap<String, String>,
    /// Template parameters, exported as `VMUC_PARAM_<KEY>` to scripts.
    #[serde(
        deserialize_with = "de_null_default",
        skip_serializing_if = "IndexMap::is_empty"
    )]
    pub param: IndexMap<String, String>,
    #[serde(
        deserialize_with = "de_null_default",
        skip_serializing_if = "is_default"
    )]
    pub user: User,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timezone: Option<String>,
    #[serde(
        deserialize_with = "de_null_default",
        skip_serializing_if = "is_default"
    )]
    pub memory_policy: MemoryPolicy,
    /// Printed after `vmuc start`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(
        deserialize_with = "de_null_default",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub dns: Vec<IpAddr>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host_resolver: Option<bool>,
}

impl InstanceConfig {
    /// Parses one YAML document. `origin` names the source in errors.
    pub fn from_yaml(text: &str, origin: &str) -> crate::Result<Self> {
        serde_yaml::from_str(text).map_err(|source| Error::Yaml {
            origin: origin.to_owned(),
            source,
        })
    }

    /// Serializes only the fields that are set.
    pub fn to_yaml(&self) -> crate::Result<String> {
        serde_yaml::to_string(self).map_err(Error::YamlSerialize)
    }
}

#[allow(clippy::trivially_copy_pass_by_ref)] // serde passes a reference
fn is_false(b: &bool) -> bool {
    !*b
}

fn is_default<T: Default + PartialEq>(t: &T) -> bool {
    *t == T::default()
}

/// `null` means "unset" for lists, maps and nested sections too, so a
/// reference-style document can spell out every key.
fn de_null_default<'de, D, T>(d: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de> + Default,
{
    Ok(Option::<T>::deserialize(d)?.unwrap_or_default())
}

/// Accepts `base: template:x`, `base: [a, b]` and `base: null`.
fn de_string_or_seq<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<String>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Raw {
        One(String),
        Many(Vec<String>),
    }
    Ok(match Option::<Raw>::deserialize(d)? {
        None => Vec::new(),
        Some(Raw::One(s)) => vec![s],
        Some(Raw::Many(v)) => v,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn byte_size_parses_binary_units() {
        for (text, bytes) in [
            ("8GiB", 8 * ByteSize::GIB),
            ("8G", 8 * ByteSize::GIB),
            ("8gb", 8 * ByteSize::GIB),
            ("8192MiB", 8 * ByteSize::GIB),
            ("8192M", 8 * ByteSize::GIB),
            ("512KiB", 512 * ByteSize::KIB),
            ("1.5GiB", 3 * ByteSize::GIB / 2),
            (" 100 GiB ", 100 * ByteSize::GIB),
            ("1024", 1024),
            ("1024B", 1024),
            ("0", 0),
            ("1TiB", 1 << 40),
        ] {
            assert_eq!(text.parse::<ByteSize>().unwrap().0, bytes, "{text}");
        }
        for bad in [
            "",
            "GiB",
            "8XiB",
            "-1",
            "1..5G",
            "8 Gi B",
            "99999999999999999999G",
        ] {
            assert!(bad.parse::<ByteSize>().is_err(), "{bad}");
        }
    }

    #[test]
    fn byte_size_display_round_trips() {
        for (bytes, text) in [
            (8 * ByteSize::GIB, "8GiB"),
            (1536 * ByteSize::MIB, "1536MiB"),
            (3 * ByteSize::KIB, "3KiB"),
            (1000, "1000B"),
            (0, "0"),
        ] {
            let b = ByteSize(bytes);
            assert_eq!(b.to_string(), text);
            assert_eq!(text.parse::<ByteSize>().unwrap(), b);
        }
    }

    #[test]
    fn byte_size_serde_accepts_string_and_integer() {
        #[derive(Deserialize)]
        struct T {
            a: ByteSize,
            b: ByteSize,
        }
        let t: T = serde_yaml::from_str("a: 4GiB\nb: 2048\n").unwrap();
        assert_eq!(t.a, ByteSize::from_gib(4));
        assert_eq!(t.b, ByteSize(2048));
        assert_eq!(
            serde_yaml::to_string(&ByteSize::from_gib(4))
                .unwrap()
                .trim(),
            "4GiB"
        );
    }

    #[test]
    fn digest_parses_sha256_and_sha512() {
        let d: Digest = format!("sha256:{}", "ab".repeat(32)).parse().unwrap();
        assert_eq!(d.algorithm, DigestAlgorithm::Sha256);
        let err = format!("SHA512:{}", "AB".repeat(64))
            .parse::<Digest>()
            .unwrap_err();
        assert!(err.to_string().contains("invalid digest"));
        let d: Digest = format!("sha512:{}", "AB".repeat(64)).parse().unwrap();
        assert_eq!(d.hex, "ab".repeat(64));
        assert_eq!(d.to_string(), format!("sha512:{}", "ab".repeat(64)));
        assert!("sha256:abc".parse::<Digest>().is_err());
        assert!("md5:ab".parse::<Digest>().is_err());
        assert!(
            format!("sha256:{}", "zz".repeat(32))
                .parse::<Digest>()
                .is_err()
        );
    }

    #[test]
    fn parses_lima_style_document() {
        let y = r#"
base:
- template:_images/ubuntu-24.04
images:
- location: https://example.com/x.img
  digest: "sha256:0000000000000000000000000000000000000000000000000000000000000000"
cpus: 2
memory: 4GiB
mounts:
- location: "~"
  writable: true
portForwards:
- guestPort: 8080
  hostPort: 8080
  static: true
provision:
- mode: system
  script: |
    #!/bin/sh
    echo hi
- mode: data
  path: /etc/motd
  content: hello
probes:
- script: "true"
  hint: nope
env:
  FOO: bar
param:
  KEY: value
user:
  name: alice
  uid: 501
memoryPolicy:
  pressureBalloon: true
  minAvailable: 512MiB
dns: [1.1.1.1, "2606:4700:4700::1111"]
"#;
        let c = InstanceConfig::from_yaml(y, "test").unwrap();
        assert_eq!(c.base, ["template:_images/ubuntu-24.04"]);
        assert_eq!(c.images[0].arch, Arch::Aarch64);
        assert_eq!(c.cpus, Some(2));
        assert_eq!(c.memory, Some(ByteSize::from_gib(4)));
        assert_eq!(c.mounts[0].writable, Some(true));
        assert!(c.port_forwards[0].is_static);
        assert_eq!(c.provision[1].mode, ProvisionMode::Data);
        assert_eq!(c.probes[0].mode, ProbeMode::Readiness);
        assert_eq!(c.user.uid, Some(501));
        assert_eq!(c.memory_policy.min_available, Some(ByteSize::from_mib(512)));
        assert_eq!(c.dns.len(), 2);
        // Round trip through the compact serializer.
        let again = InstanceConfig::from_yaml(&c.to_yaml().unwrap(), "again").unwrap();
        assert_eq!(again, c);
    }

    #[test]
    fn base_accepts_scalar_and_null() {
        let c = InstanceConfig::from_yaml("base: template:default\n", "t").unwrap();
        assert_eq!(c.base, ["template:default"]);
        let c = InstanceConfig::from_yaml("base: null\n", "t").unwrap();
        assert_eq!(c.base, [] as [String; 0]);
        let c = InstanceConfig::from_yaml("image:\n- location: /x.img\n", "t").unwrap();
        assert_eq!(c.images.len(), 1);
        let c = InstanceConfig::from_yaml(
            "images: null\nmounts: ~\nssh: null\nenv: null\ndns: null\n",
            "t",
        )
        .unwrap();
        assert_eq!(c, InstanceConfig::default());
    }

    #[test]
    fn rejects_unknown_keys() {
        let err = InstanceConfig::from_yaml("vmType: qemu\n", "t").unwrap_err();
        assert!(err.to_string().contains("vmType"), "{err}");
        let err =
            InstanceConfig::from_yaml("mounts:\n- location: /x\n  sshfs: {}\n", "t").unwrap_err();
        assert!(err.to_string().contains("sshfs"), "{err}");
    }

    #[test]
    fn empty_document_is_default() {
        let c = InstanceConfig::from_yaml("", "t").unwrap();
        assert_eq!(c, InstanceConfig::default());
        let c = InstanceConfig::from_yaml("# only a comment\n", "t").unwrap();
        assert_eq!(c, InstanceConfig::default());
    }
}
