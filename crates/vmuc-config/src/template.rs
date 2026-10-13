// SPDX-License-Identifier: Apache-2.0
//! Template locators, the template search path and `base:` flattening.
//!
//! A locator is `template:NAME`, a file path, `-` (stdin) or an `https://`
//! URL. Names are searched in `$VMUC_TEMPLATES_PATH`, `~/.vmuc/_templates`,
//! the installed templates directory, then the templates compiled into the
//! binary from `guest/templates/`.
//!
//! `embed` flattens `base:` recursively the way Lima's `limatmpl` does:
//! the child wins for scalars and maps, `provision`/`probes` run base first,
//! other lists are child first, mounts merge by location and a non-empty
//! child `images` list replaces the base's.

use std::fmt;
use std::path::{Path, PathBuf};

use crate::paths::{TEMPLATES_PATH_ENV, VmucHome};
use crate::schema::{InstanceConfig, Mount};
use crate::{Error, Result};

/// Maximum `base:` nesting depth.
pub const MAX_EMBED_DEPTH: usize = 8;

/// Templates compiled into the binary, by name.
pub const EMBEDDED: &[(&str, &str)] = &[
    (
        "default",
        include_str!("../../../guest/templates/default.yaml"),
    ),
    (
        "ubuntu",
        include_str!("../../../guest/templates/ubuntu.yaml"),
    ),
    (
        "alpine",
        include_str!("../../../guest/templates/alpine.yaml"),
    ),
    (
        "docker",
        include_str!("../../../guest/templates/docker.yaml"),
    ),
    (
        "reference",
        include_str!("../../../guest/templates/reference.yaml"),
    ),
    (
        "_images/ubuntu-24.04",
        include_str!("../../../guest/templates/_images/ubuntu-24.04.yaml"),
    ),
    (
        "_images/alpine",
        include_str!("../../../guest/templates/_images/alpine.yaml"),
    ),
    (
        "_default/mounts",
        include_str!("../../../guest/templates/_default/mounts.yaml"),
    ),
];

/// Where a template comes from.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Locator {
    /// `template:NAME`, resolved through the search path.
    Template(String),
    /// A YAML file on the host.
    File(PathBuf),
    /// `-`: bytes supplied by the caller.
    Stdin,
    /// `https://…`: the caller downloads it (via vmuc-image), then calls
    /// [`TemplateStore::load_yaml`].
    Url(String),
}

impl Locator {
    pub fn parse(input: &str) -> Result<Self> {
        if input == "-" {
            return Ok(Self::Stdin);
        }
        if let Some(name) = input.strip_prefix("template:") {
            validate_template_name(name)?;
            return Ok(Self::Template(name.to_owned()));
        }
        if input.starts_with("https://") {
            return Ok(Self::Url(input.to_owned()));
        }
        if input.starts_with("http://") {
            return Err(Error::invalid(
                "locator",
                format!("{input}: only https:// URLs are accepted"),
            ));
        }
        let path = input.strip_prefix("file://").unwrap_or(input);
        if path.is_empty() {
            return Err(Error::invalid("locator", "empty"));
        }
        Ok(Self::File(PathBuf::from(path)))
    }

    /// Parses a `base:` entry relative to the template that lists it.
    fn parse_base(input: &str, parent: &Self) -> Result<Self> {
        let loc = Self::parse(input)?;
        match (&loc, parent) {
            (Self::File(p), Self::File(parent_path)) if p.is_relative() => Ok(Self::File(
                parent_path
                    .parent()
                    .map_or_else(|| p.clone(), |dir| dir.join(p)),
            )),
            (Self::Stdin, _) => Err(Error::invalid("base", "\"-\" cannot be used as a base")),
            _ => Ok(loc),
        }
    }
}

impl fmt::Display for Locator {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Template(n) => write!(f, "template:{n}"),
            Self::File(p) => write!(f, "{}", p.display()),
            Self::Stdin => write!(f, "-"),
            Self::Url(u) => write!(f, "{u}"),
        }
    }
}

/// Template names are relative paths without `..`, like `_images/alpine`.
pub fn validate_template_name(name: &str) -> Result<()> {
    let bad = name.is_empty()
        || name.starts_with('/')
        || name.ends_with('/')
        || name.contains("//")
        || name.contains('\\')
        || name.contains('\0')
        || name.split('/').any(|c| c == "." || c == "..");
    if bad {
        return Err(Error::InvalidTemplateName(name.to_owned()));
    }
    Ok(())
}

/// A parsed template with its origin.
#[derive(Debug, Clone, PartialEq)]
pub struct Template {
    pub locator: Locator,
    pub config: InstanceConfig,
}

/// What [`TemplateStore::resolve`] produced.
#[derive(Debug, Clone, PartialEq)]
pub enum Resolution {
    Loaded(Box<Template>),
    /// Download this URL, then call [`TemplateStore::load_yaml`].
    Fetch(String),
}

/// The template search path.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TemplateStore {
    dirs: Vec<PathBuf>,
}

impl TemplateStore {
    /// `$VMUC_TEMPLATES_PATH` entries, `~/.vmuc/_templates`, then
    /// `installed` (the distribution's templates directory, if any).
    pub fn new(home: &VmucHome, installed: Option<&Path>) -> Self {
        let mut dirs = Vec::new();
        if let Some(path) = std::env::var_os(TEMPLATES_PATH_ENV) {
            dirs.extend(std::env::split_paths(&path).filter(|p| !p.as_os_str().is_empty()));
        }
        dirs.push(home.templates_dir());
        dirs.extend(installed.map(Path::to_path_buf));
        Self { dirs }
    }

    /// A store with an explicit search path (tests, or embedded-only when
    /// empty).
    pub fn with_dirs(dirs: impl IntoIterator<Item = PathBuf>) -> Self {
        Self {
            dirs: dirs.into_iter().collect(),
        }
    }

    pub fn search_dirs(&self) -> &[PathBuf] {
        &self.dirs
    }

    /// Reads a template's YAML text by name.
    pub fn read_template(&self, name: &str) -> Result<String> {
        validate_template_name(name)?;
        for dir in &self.dirs {
            for candidate in [dir.join(format!("{name}.yaml")), dir.join(name)] {
                if candidate.is_file() {
                    return std::fs::read_to_string(&candidate)
                        .map_err(|e| Error::io(candidate, e));
                }
            }
        }
        EMBEDDED
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, text)| (*text).to_owned())
            .ok_or_else(|| Error::TemplateNotFound {
                name: name.to_owned(),
                searched: self.dirs.clone(),
            })
    }

    /// All template names visible through this store, sorted and unique.
    /// Names under `_images/` and `_default/` are building blocks and are
    /// listed too.
    pub fn names(&self) -> Vec<String> {
        let mut names: Vec<String> = EMBEDDED.iter().map(|(n, _)| (*n).to_owned()).collect();
        for dir in &self.dirs {
            collect_yaml_names(dir, "", &mut names, 0);
        }
        names.sort();
        names.dedup();
        names
    }

    /// Resolves user input to a parsed template, or asks the caller to fetch.
    pub fn resolve(&self, input: &str, stdin: Option<&[u8]>) -> Result<Resolution> {
        let locator = Locator::parse(input)?;
        let text = match &locator {
            Locator::Template(name) => self.read_template(name)?,
            Locator::File(path) => std::fs::read_to_string(path).map_err(|e| Error::io(path, e))?,
            Locator::Stdin => {
                String::from_utf8_lossy(stdin.ok_or(Error::StdinRequired)?).into_owned()
            }
            Locator::Url(url) => return Ok(Resolution::Fetch(url.clone())),
        };
        Ok(Resolution::Loaded(Box::new(
            self.load_yaml(locator, &text)?,
        )))
    }

    /// Parses YAML the caller already has (downloaded, or from stdin).
    pub fn load_yaml(&self, locator: Locator, text: &str) -> Result<Template> {
        let config = InstanceConfig::from_yaml(text, &locator.to_string())?;
        Ok(Template { locator, config })
    }

    /// Flattens `base:` recursively; the result has an empty `base`.
    pub fn embed(&self, template: &Template) -> Result<InstanceConfig> {
        let mut seen = vec![template.locator.to_string()];
        self.embed_into(template.config.clone(), &template.locator, 0, &mut seen)
    }

    fn embed_into(
        &self,
        mut config: InstanceConfig,
        origin: &Locator,
        depth: usize,
        seen: &mut Vec<String>,
    ) -> Result<InstanceConfig> {
        let bases = std::mem::take(&mut config.base);
        for base in bases {
            let locator = Locator::parse_base(&base, origin)?;
            if depth + 1 > MAX_EMBED_DEPTH {
                return Err(Error::TemplateTooDeep {
                    max: MAX_EMBED_DEPTH,
                    locator: locator.to_string(),
                });
            }
            let key = locator.to_string();
            if seen.contains(&key) {
                return Err(Error::TemplateCycle(key));
            }
            let text = match &locator {
                Locator::Template(name) => self.read_template(name)?,
                Locator::File(path) => {
                    std::fs::read_to_string(path).map_err(|e| Error::io(path, e))?
                }
                Locator::Url(url) => return Err(Error::RemoteBase(url.clone())),
                Locator::Stdin => unreachable!("rejected by parse_base"),
            };
            let base_config = InstanceConfig::from_yaml(&text, &key)?;
            seen.push(key);
            let base_config = self.embed_into(base_config, &locator, depth + 1, seen)?;
            seen.pop();
            config = merge(config, base_config);
        }
        Ok(config)
    }
}

fn collect_yaml_names(dir: &Path, prefix: &str, out: &mut Vec<String>, depth: usize) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(file_name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if path.is_dir() && depth < 2 {
            collect_yaml_names(&path, &format!("{prefix}{file_name}/"), out, depth + 1);
        } else if let Some(stem) = file_name.strip_suffix(".yaml") {
            out.push(format!("{prefix}{stem}"));
        }
    }
}

/// Merges `base` under `child`.
fn merge(child: InstanceConfig, base: InstanceConfig) -> InstanceConfig {
    let mut env = base.env;
    env.extend(child.env);
    let mut param = base.param;
    param.extend(child.param);
    InstanceConfig {
        base: Vec::new(),
        images: if child.images.is_empty() {
            base.images
        } else {
            child.images
        },
        cpus: child.cpus.or(base.cpus),
        memory: child.memory.or(base.memory),
        disk: child.disk.or(base.disk),
        mounts: merge_mounts(child.mounts, base.mounts),
        ssh: crate::schema::Ssh {
            load_dot_ssh_pub_keys: child
                .ssh
                .load_dot_ssh_pub_keys
                .or(base.ssh.load_dot_ssh_pub_keys),
            forward_agent: child.ssh.forward_agent.or(base.ssh.forward_agent),
        },
        network: crate::schema::Network {
            mode: child.network.mode.or(base.network.mode),
            interface: child.network.interface.or(base.network.interface),
            mac_address: child.network.mac_address.or(base.network.mac_address),
        },
        port_forwards: concat(child.port_forwards, base.port_forwards),
        provision: concat(base.provision, child.provision),
        probes: concat(base.probes, child.probes),
        env,
        param,
        user: crate::schema::User {
            name: child.user.name.or(base.user.name),
            uid: child.user.uid.or(base.user.uid),
            home: child.user.home.or(base.user.home),
            shell: child.user.shell.or(base.user.shell),
            comment: child.user.comment.or(base.user.comment),
            passwordless_sudo: child.user.passwordless_sudo.or(base.user.passwordless_sudo),
        },
        timezone: child.timezone.or(base.timezone),
        message: child.message.or(base.message),
        dns: if child.dns.is_empty() {
            base.dns
        } else {
            child.dns
        },
        host_resolver: child.host_resolver.or(base.host_resolver),
    }
}

fn concat<T>(mut first: Vec<T>, second: Vec<T>) -> Vec<T> {
    first.extend(second);
    first
}

/// Child mounts first; a base mount with a known location only fills in
/// fields the child left unset.
fn merge_mounts(child: Vec<Mount>, base: Vec<Mount>) -> Vec<Mount> {
    let mut out = child;
    for m in base {
        if let Some(existing) = out.iter_mut().find(|c| c.location == m.location) {
            if existing.mount_point.is_none() {
                existing.mount_point = m.mount_point;
            }
            if existing.writable.is_none() {
                existing.writable = m.writable;
            }
            if existing.dax.is_none() {
                existing.dax = m.dax;
            }
        } else {
            out.push(m);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::defaults::{Builtin, fill_defaults};
    use crate::schema::ByteSize;

    fn embedded_only() -> TemplateStore {
        TemplateStore::with_dirs([])
    }

    fn write(dir: &Path, name: &str, text: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, text).unwrap();
        path
    }

    #[test]
    fn locator_forms() {
        assert_eq!(Locator::parse("-").unwrap(), Locator::Stdin);
        assert_eq!(
            Locator::parse("template:docker").unwrap(),
            Locator::Template("docker".into())
        );
        assert_eq!(
            Locator::parse("template:_images/alpine").unwrap(),
            Locator::Template("_images/alpine".into())
        );
        assert_eq!(
            Locator::parse("https://example.com/x.yaml").unwrap(),
            Locator::Url("https://example.com/x.yaml".into())
        );
        assert_eq!(
            Locator::parse("./x.yaml").unwrap(),
            Locator::File("./x.yaml".into())
        );
        assert_eq!(
            Locator::parse("file:///x.yaml").unwrap(),
            Locator::File("/x.yaml".into())
        );
        assert!(Locator::parse("http://example.com/x.yaml").is_err());
        assert!(Locator::parse("").is_err());
        for bad in [
            "template:",
            "template:../x",
            "template:/etc/passwd",
            "template:a/../b",
            "template:a//b",
            "template:a/",
        ] {
            assert!(Locator::parse(bad).is_err(), "{bad}");
        }
        assert_eq!(Locator::Template("x".into()).to_string(), "template:x");
    }

    #[test]
    fn embedded_templates_are_found_and_listed() {
        let store = embedded_only();
        assert!(store.read_template("default").unwrap().contains("base:"));
        assert!(
            store
                .read_template("_images/ubuntu-24.04")
                .unwrap()
                .contains("images:")
        );
        let names = store.names();
        for n in [
            "default",
            "ubuntu",
            "alpine",
            "docker",
            "reference",
            "_images/alpine",
            "_default/mounts",
        ] {
            assert!(names.contains(&n.to_owned()), "{n} missing from {names:?}");
        }
        let err = store.read_template("nope").unwrap_err().to_string();
        assert!(err.contains("not found"), "{err}");
    }

    #[test]
    fn search_dirs_shadow_embedded_in_order() {
        let tmp = tempfile::tempdir().unwrap();
        let first = tmp.path().join("first");
        let second = tmp.path().join("second");
        write(&second, "default.yaml", "cpus: 2\n");
        write(&second, "only-second.yaml", "cpus: 9\n");
        write(
            &second,
            "_images/custom.yaml",
            "images: [{location: /img}]\n",
        );
        let store = TemplateStore::with_dirs([first.clone(), second.clone()]);
        assert_eq!(store.read_template("default").unwrap(), "cpus: 2\n");
        write(&first, "default.yaml", "cpus: 1\n");
        assert_eq!(store.read_template("default").unwrap(), "cpus: 1\n");
        assert!(store.read_template("docker").unwrap().contains("docker"));
        let names = store.names();
        assert!(names.contains(&"only-second".to_owned()));
        assert!(names.contains(&"_images/custom".to_owned()));
        assert_eq!(names.iter().filter(|n| *n == "default").count(), 1);
        assert_eq!(store.search_dirs(), [first, second]);
    }

    #[test]
    fn resolve_handles_every_locator() {
        let store = embedded_only();
        match store.resolve("template:ubuntu", None).unwrap() {
            Resolution::Loaded(t) => assert_eq!(t.locator, Locator::Template("ubuntu".into())),
            Resolution::Fetch(_) => panic!(),
        }
        assert_eq!(
            store.resolve("https://x/y.yaml", None).unwrap(),
            Resolution::Fetch("https://x/y.yaml".into())
        );
        match store.resolve("-", Some(b"cpus: 3\n")).unwrap() {
            Resolution::Loaded(t) => assert_eq!(t.config.cpus, Some(3)),
            Resolution::Fetch(_) => panic!(),
        }
        assert!(matches!(
            store.resolve("-", None),
            Err(Error::StdinRequired)
        ));
        let tmp = tempfile::tempdir().unwrap();
        let file = write(tmp.path(), "f.yaml", "cpus: 4\n");
        match store.resolve(file.to_str().unwrap(), None).unwrap() {
            Resolution::Loaded(t) => assert_eq!(t.config.cpus, Some(4)),
            Resolution::Fetch(_) => panic!(),
        }
        let err = store
            .resolve(tmp.path().join("missing.yaml").to_str().unwrap(), None)
            .unwrap_err();
        assert!(matches!(err, Error::Io { .. }), "{err}");
        let err = store.resolve("-", Some(b"cpus: [\n")).unwrap_err();
        assert!(matches!(err, Error::Yaml { .. }), "{err}");
        let fetched = store
            .load_yaml(Locator::Url("https://x".into()), "cpus: 5\n")
            .unwrap();
        assert_eq!(fetched.config.cpus, Some(5));
    }

    #[test]
    fn embed_merges_child_over_base() {
        let tmp = tempfile::tempdir().unwrap();
        write(
            tmp.path(),
            "base.yaml",
            "cpus: 1\nmemory: 1GiB\nimages: [{location: base.img}]\nmounts: [{location: /a, writable: true}, {location: /b}]\nprovision: [{script: base}]\nprobes: [{script: base}]\nportForwards: [{guestPort: 1}]\nenv: {X: base, Y: base}\ndns: [9.9.9.9]\nuser: {name: baseuser, shell: /bin/sh}\n",
        );
        write(
            tmp.path(),
            "child.yaml",
            "base: base.yaml\ncpus: 2\nmounts: [{location: /a, mountPoint: /mnt/a}, {location: /c}]\nprovision: [{script: child}]\nprobes: [{script: child}]\nportForwards: [{guestPort: 2}]\nenv: {Y: child, Z: child}\nuser: {name: childuser}\n",
        );
        let store = TemplateStore::with_dirs([tmp.path().to_path_buf()]);
        let child = store.read_template("child").unwrap();
        let t = store
            .load_yaml(Locator::File(tmp.path().join("child.yaml")), &child)
            .unwrap();
        let c = store.embed(&t).unwrap();
        assert_eq!(c.base, [] as [String; 0]);
        assert_eq!(c.cpus, Some(2));
        assert_eq!(c.memory, Some(ByteSize::from_gib(1)));
        assert_eq!(c.images[0].location, "base.img");
        let mounts: Vec<(&str, Option<&str>, Option<bool>)> = c
            .mounts
            .iter()
            .map(|m| (m.location.as_str(), m.mount_point.as_deref(), m.writable))
            .collect();
        assert_eq!(
            mounts,
            [
                ("/a", Some("/mnt/a"), Some(true)),
                ("/c", None, None),
                ("/b", None, None)
            ]
        );
        let scripts: Vec<&str> = c
            .provision
            .iter()
            .map(|p| p.script.as_deref().unwrap())
            .collect();
        assert_eq!(scripts, ["base", "child"], "provision runs base first");
        let probes: Vec<&str> = c
            .probes
            .iter()
            .map(|p| p.script.as_deref().unwrap())
            .collect();
        assert_eq!(probes, ["base", "child"]);
        let ports: Vec<u16> = c
            .port_forwards
            .iter()
            .map(|p| p.guest_port.unwrap())
            .collect();
        assert_eq!(ports, [2, 1], "port forwards are child first");
        let env: Vec<(&str, &str)> = c
            .env
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        assert_eq!(env, [("X", "base"), ("Y", "child"), ("Z", "child")]);
        assert_eq!(c.dns.len(), 1);
        assert_eq!(c.user.name.as_deref(), Some("childuser"));
        assert_eq!(c.user.shell.as_deref(), Some("/bin/sh"));
    }

    #[test]
    fn embed_child_images_replace_base_images() {
        let tmp = tempfile::tempdir().unwrap();
        write(
            tmp.path(),
            "base.yaml",
            "images: [{location: b1}, {location: b2}]\n",
        );
        write(
            tmp.path(),
            "child.yaml",
            "base: base.yaml\nimages: [{location: c}]\n",
        );
        let store = TemplateStore::with_dirs([tmp.path().to_path_buf()]);
        let t = store
            .load_yaml(
                Locator::File(tmp.path().join("child.yaml")),
                "base: base.yaml\nimages: [{location: c}]\n",
            )
            .unwrap();
        let c = store.embed(&t).unwrap();
        assert_eq!(c.images.len(), 1);
        assert_eq!(c.images[0].location, "c");
    }

    #[test]
    fn embed_earlier_bases_win_over_later_ones() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "a.yaml", "cpus: 1\nprovision: [{script: a}]\n");
        write(
            tmp.path(),
            "b.yaml",
            "cpus: 2\nmemory: 2GiB\nprovision: [{script: b}]\n",
        );
        let store = TemplateStore::with_dirs([tmp.path().to_path_buf()]);
        let t = store
            .load_yaml(
                Locator::Template("x".into()),
                "base: [template:a, template:b]\n",
            )
            .unwrap();
        let c = store.embed(&t).unwrap();
        assert_eq!(c.cpus, Some(1));
        assert_eq!(c.memory, Some(ByteSize::from_gib(2)));
        let scripts: Vec<&str> = c
            .provision
            .iter()
            .map(|p| p.script.as_deref().unwrap())
            .collect();
        assert_eq!(scripts, ["b", "a"]);
    }

    #[test]
    fn embed_detects_cycles_and_depth() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "a.yaml", "base: template:b\n");
        write(tmp.path(), "b.yaml", "base: template:a\n");
        let store = TemplateStore::with_dirs([tmp.path().to_path_buf()]);
        let t = store
            .load_yaml(Locator::Template("a".into()), "base: template:b\n")
            .unwrap();
        let err = store.embed(&t).unwrap_err();
        assert!(matches!(err, Error::TemplateCycle(_)), "{err}");
        // self reference
        write(tmp.path(), "s.yaml", "base: template:s\n");
        let t = store
            .load_yaml(Locator::Template("s".into()), "base: template:s\n")
            .unwrap();
        assert!(matches!(
            store.embed(&t).unwrap_err(),
            Error::TemplateCycle(_)
        ));
        // a chain deeper than the limit
        for i in 0..=MAX_EMBED_DEPTH + 1 {
            write(
                tmp.path(),
                &format!("d{i}.yaml"),
                &format!("base: template:d{}\n", i + 1),
            );
        }
        write(
            tmp.path(),
            &format!("d{}.yaml", MAX_EMBED_DEPTH + 2),
            "cpus: 1\n",
        );
        let t = store
            .load_yaml(Locator::Template("d0".into()), "base: template:d1\n")
            .unwrap();
        assert!(matches!(
            store.embed(&t).unwrap_err(),
            Error::TemplateTooDeep { .. }
        ));
        // exactly at the limit is fine: d(N-8) .. d(N) is 8 levels
        let start = MAX_EMBED_DEPTH + 2 - MAX_EMBED_DEPTH;
        let t = store
            .load_yaml(
                Locator::Template(format!("d{start}")),
                &format!("base: template:d{}\n", start + 1),
            )
            .unwrap();
        assert_eq!(store.embed(&t).unwrap().cpus, Some(1));
        // diamonds are allowed
        write(tmp.path(), "leaf.yaml", "cpus: 3\n");
        write(tmp.path(), "l.yaml", "base: template:leaf\n");
        write(tmp.path(), "r.yaml", "base: template:leaf\n");
        let t = store
            .load_yaml(
                Locator::Template("top".into()),
                "base: [template:l, template:r]\n",
            )
            .unwrap();
        assert_eq!(store.embed(&t).unwrap().cpus, Some(3));
    }

    #[test]
    fn embed_rejects_remote_and_stdin_bases() {
        let store = embedded_only();
        let t = store
            .load_yaml(Locator::Template("x".into()), "base: https://x/y.yaml\n")
            .unwrap();
        assert!(matches!(store.embed(&t).unwrap_err(), Error::RemoteBase(_)));
        let t = store
            .load_yaml(Locator::Template("x".into()), "base: '-'\n")
            .unwrap();
        assert!(matches!(
            store.embed(&t).unwrap_err(),
            Error::Invalid { .. }
        ));
    }

    #[test]
    fn relative_file_bases_resolve_against_the_parent_template() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "sub/base.yaml", "cpus: 6\n");
        let child = write(tmp.path(), "sub/child.yaml", "base: base.yaml\n");
        let store = embedded_only();
        match store.resolve(child.to_str().unwrap(), None).unwrap() {
            Resolution::Loaded(t) => assert_eq!(store.embed(&t).unwrap().cpus, Some(6)),
            Resolution::Fetch(_) => panic!(),
        }
    }

    #[test]
    fn every_shipped_template_parses_embeds_and_resolves() {
        let store = embedded_only();
        let builtin = Builtin::fixed(16 * ByteSize::GIB, "/Users/tester");
        for (name, _) in EMBEDDED {
            let text = store.read_template(name).unwrap();
            let t = store
                .load_yaml(Locator::Template((*name).to_owned()), &text)
                .unwrap_or_else(|e| panic!("{name}: {e}"));
            let flat = store.embed(&t).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(flat.base, [] as [String; 0]);
            let resolved = fill_defaults("t", &flat, None, None, &builtin)
                .unwrap_or_else(|e| panic!("{name}: {e}"));
            if !name.starts_with('_') && *name != "reference" {
                assert!(!resolved.images.is_empty(), "{name} has no images");
                assert!(
                    resolved.images.iter().any(|i| i.digest.is_some()),
                    "{name} has no pinned image"
                );
                assert_eq!(resolved.mounts.len(), 1, "{name} should mount ~ only");
            }
        }
        // reference.yaml sets nothing, so it resolves to the built-in defaults.
        let reference = InstanceConfig::from_yaml(
            store.read_template("reference").unwrap().as_str(),
            "reference",
        )
        .unwrap();
        assert_eq!(reference, InstanceConfig::default());
    }
}
