// SPDX-License-Identifier: Apache-2.0
//! Instance lifecycle that does not need a running VMM: create (template →
//! `vmuc.yaml` → disk image), list, delete, seed rendering and the boot
//! parameters a VMM or the QEMU harness needs.

use std::fs;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use vmuc_config::cidata::SeedMount;
use vmuc_config::paths::instance as files;
use vmuc_config::schema::{DigestAlgorithm, Mount};
use vmuc_config::{
    Builtin, InstanceConfig, Resolution, ResolvedConfig, TemplateStore, VmucHome, fill_defaults,
};
use vmuc_image::sparse::SparseWriter;
use vmuc_image::{Cache, Digest, decompress_if_needed};

/// Overrides from `vmuc create` flags, applied on top of the template.
#[derive(Debug, Default)]
pub struct CreateOverrides {
    pub cpus: Option<u32>,
    pub memory: Option<String>,
    pub disk: Option<String>,
    pub mounts: Vec<String>,
}

pub struct Instance {
    pub name: String,
    pub dir: PathBuf,
}

impl Instance {
    pub fn open(home: &VmucHome, name: &str) -> anyhow::Result<Self> {
        let dir = home.instance_dir(name)?;
        if !dir.join(files::CONFIG).is_file() {
            bail!(
                "instance {name:?} does not exist (looked in {})",
                dir.display()
            );
        }
        Ok(Self {
            name: name.to_owned(),
            dir,
        })
    }

    pub fn config(&self) -> anyhow::Result<InstanceConfig> {
        let path = self.dir.join(files::CONFIG);
        let text = fs::read_to_string(&path).with_context(|| path.display().to_string())?;
        Ok(InstanceConfig::from_yaml(
            &text,
            &path.display().to_string(),
        )?)
    }

    pub fn resolved(&self, home: &VmucHome) -> anyhow::Result<ResolvedConfig> {
        resolve(home, &self.name, &self.config()?)
    }

    pub fn disk(&self) -> PathBuf {
        self.dir.join(files::DISK)
    }

    pub fn seed(&self) -> PathBuf {
        self.dir.join(files::CIDATA)
    }
}

/// Layers the user's `default.yaml` / `override.yaml` and host facts onto
/// an instance config.
pub fn resolve(
    home: &VmucHome,
    name: &str,
    cfg: &InstanceConfig,
) -> anyhow::Result<ResolvedConfig> {
    let load = |p: PathBuf| -> anyhow::Result<Option<InstanceConfig>> {
        if p.is_file() {
            let text = fs::read_to_string(&p).with_context(|| p.display().to_string())?;
            Ok(Some(InstanceConfig::from_yaml(
                &text,
                &p.display().to_string(),
            )?))
        } else {
            Ok(None)
        }
    };
    let user_default = load(home.user_default_config())?;
    let override_ = load(home.override_config())?;
    let builtin = Builtin::detect(crate::host::host_ram_bytes())?;
    Ok(fill_defaults(
        name,
        cfg,
        user_default.as_ref(),
        override_.as_ref(),
        &builtin,
    )?)
}

/// `vmuc create`: resolve and flatten the template, apply flag overrides,
/// validate, write the instance directory, then fetch and convert the
/// disk image.
pub fn create(
    home: &VmucHome,
    store: &TemplateStore,
    template: &str,
    name: Option<&str>,
    overrides: &CreateOverrides,
) -> anyhow::Result<Instance> {
    let template = match store.resolve(template, None)? {
        Resolution::Loaded(t) => *t,
        Resolution::Fetch(url) => {
            let cache = Cache::open_default()?;
            let path = cache.fetch(&url, None, |_, _| {})?;
            let text = fs::read_to_string(&path)?;
            store.load_yaml(vmuc_config::Locator::parse(&url)?, &text)?
        }
    };
    let name = name.map_or_else(
        || default_name(&template.locator.to_string()),
        str::to_owned,
    );
    let mut cfg = store.embed(&template)?;
    apply_overrides(&mut cfg, overrides)?;

    let dir = home.instance_dir(&name)?;
    if dir.exists() {
        bail!("instance {name:?} already exists at {}", dir.display());
    }
    // Validate before touching the filesystem.
    let resolved = resolve(home, &name, &cfg)?;

    fs::create_dir_all(&dir).with_context(|| dir.display().to_string())?;
    fs::write(dir.join(files::CONFIG), cfg.to_yaml()?)?;
    fs::write(
        dir.join(files::VERSION),
        concat!(env!("CARGO_PKG_VERSION"), "\n"),
    )?;
    let inst = Instance { name, dir };
    prepare_disk(&inst, &resolved)?;
    Ok(inst)
}

fn default_name(locator: &str) -> String {
    let base = locator
        .rsplit(['/', ':'])
        .next()
        .unwrap_or("default")
        .trim_end_matches(".yaml")
        .trim_end_matches(".yml");
    if base.is_empty() || base == "default.yaml" {
        "default".into()
    } else {
        base.into()
    }
}

fn apply_overrides(cfg: &mut InstanceConfig, o: &CreateOverrides) -> anyhow::Result<()> {
    if let Some(c) = o.cpus {
        cfg.cpus = Some(c);
    }
    if let Some(m) = &o.memory {
        cfg.memory = Some(m.parse()?);
    }
    if let Some(d) = &o.disk {
        cfg.disk = Some(d.parse()?);
    }
    for m in &o.mounts {
        // LOCATION[:w] like Lima's --mount.
        let (location, writable) = match m.rsplit_once(':') {
            Some((l, "w")) => (l, Some(true)),
            Some((l, "r")) => (l, Some(false)),
            _ => (m.as_str(), None),
        };
        cfg.mounts.push(Mount {
            location: location.to_owned(),
            mount_point: None,
            writable,
        });
    }
    Ok(())
}

/// Downloads the image (cached, digest-verified), decompresses, converts
/// qcow2 to a sparse raw file, and grows it to the configured size.
pub fn prepare_disk(inst: &Instance, resolved: &ResolvedConfig) -> anyhow::Result<()> {
    let disk = inst.disk();
    if disk.exists() {
        return Ok(());
    }
    let image = resolved
        .images
        .first()
        .ok_or_else(|| anyhow::anyhow!("no image configured"))?;
    let cache = Cache::open_default()?;

    let (sha256, sha512) = match &image.digest {
        Some(d) if d.algorithm == DigestAlgorithm::Sha256 => (Some(d.hex.parse::<Digest>()?), None),
        Some(d) if d.algorithm == DigestAlgorithm::Sha512 => (None, Some(d.hex.clone())),
        _ => (None, None),
    };
    eprintln!("fetching {}", image.location);
    let mut shown = 0u64;
    let path = cache.fetch(&image.location, sha256.as_ref(), |done, total| {
        if done / (64 << 20) > shown {
            shown = done / (64 << 20);
            match total {
                Some(t) => eprint!("\r  {} / {} MiB", done >> 20, t >> 20),
                None => eprint!("\r  {} MiB", done >> 20),
            }
        }
    })?;
    if shown > 0 {
        eprintln!();
    }
    if let Some(want) = sha512 {
        let got = vmuc_image::digest::sha512_hex_of_file(&path)?;
        if got != want {
            bail!(
                "sha512 mismatch for {}: expected {want}, got {got}",
                image.location
            );
        }
    }
    let path = decompress_if_needed(&path)?;

    let tmp = disk.with_extension("img.partial");
    let _ = fs::remove_file(&tmp);
    if is_qcow2(&path)? {
        eprintln!("converting qcow2 to raw");
        vmuc_image::convert_to_raw(&path, &tmp, |_, _| {})?;
    } else {
        eprintln!("copying raw image");
        sparse_copy(&path, &tmp)?;
    }
    let want = resolved.disk.bytes();
    let f = fs::OpenOptions::new().write(true).open(&tmp)?;
    let have = f.metadata()?.len();
    if want > have {
        f.set_len(want)?;
    } else if want < have {
        eprintln!(
            "note: image is {} MiB, larger than disk: {} MiB; keeping the image size",
            have >> 20,
            want >> 20
        );
    }
    drop(f);
    fs::rename(&tmp, &disk)?;
    eprintln!(
        "disk ready: {} ({} MiB allocated)",
        disk.display(),
        vmuc_image::allocated_bytes(&disk)? >> 20
    );
    Ok(())
}

fn is_qcow2(path: &Path) -> io::Result<bool> {
    let mut magic = [0u8; 4];
    let n = fs::File::open(path)?.read(&mut magic)?;
    Ok(n == 4 && &magic == b"QFI\xfb")
}

fn sparse_copy(src: &Path, dst: &Path) -> io::Result<()> {
    let mut input = fs::File::open(src)?;
    let out = fs::File::create(dst)?;
    let mut writer = SparseWriter::new(out);
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = input.read(&mut buf)?;
        if n == 0 {
            break;
        }
        writer.write_all(&buf[..n])?;
    }
    let (mut out, len) = writer.finish()?;
    out.set_len(len)?;
    out.seek(SeekFrom::Start(0))?;
    Ok(())
}

/// Mount tags are stable per (location, mount point) so the guest fstab
/// and the VMM agree without talking.
pub fn mount_tag(location: &Path, mount_point: &str) -> String {
    let key = format!("{}:{}", location.display(), mount_point);
    format!("vmuc-{}", &Digest::of_bytes(key.as_bytes()).hex()[..8])
}

pub fn seed_mounts(resolved: &ResolvedConfig) -> Vec<SeedMount> {
    resolved
        .mounts
        .iter()
        .map(|m| SeedMount {
            tag: mount_tag(&m.location, &m.mount_point),
            mount_point: m.mount_point.clone(),
            writable: m.writable,
        })
        .collect()
}

/// Renders `cidata.iso` for one boot and returns the boot ID.
pub fn write_seed(
    home: &VmucHome,
    inst: &Instance,
    resolved: &ResolvedConfig,
    agent: Option<&Path>,
    with_mounts: bool,
) -> anyhow::Result<String> {
    let boot_id = vmuc_config::new_boot_id();
    let pub_keys = crate::keys::authorized_keys(home, resolved.ssh.load_dot_ssh_pub_keys)?;
    let agent_bytes = agent.map(fs::read).transpose()?;
    let mounts = if with_mounts {
        seed_mounts(resolved)
    } else {
        Vec::new()
    };
    let files = vmuc_config::render_seed(
        resolved,
        &boot_id,
        &pub_keys,
        agent_bytes.as_deref(),
        &mounts,
    );
    let iso_files: Vec<vmuc_image::SeedFile> = files
        .into_iter()
        .map(|f| vmuc_image::SeedFile::new(f.path, f.data).with_mode(f.mode))
        .collect();
    let tmp = inst.seed().with_extension("iso.partial");
    vmuc_image::write_iso_file(&tmp, vmuc_config::cidata::SEED_LABEL, &iso_files)?;
    fs::rename(&tmp, inst.seed())?;
    Ok(boot_id)
}

/// Everything a VMM (or the QEMU harness) needs to boot this instance.
#[derive(Debug, serde::Serialize)]
pub struct BootInfo {
    pub name: String,
    /// Guest login user (the host user when valid, else `vmuc`).
    pub user: String,
    pub cpus: u32,
    pub memory_bytes: u64,
    pub disk: PathBuf,
    pub seed: PathBuf,
    pub root_partition: Option<u32>,
    /// Informational: what the root is called when the disk is `/dev/vda`.
    pub root_device: String,
    /// What the kernel actually mounts: `PARTUUID=…` for a GPT image so
    /// device enumeration order (QEMU numbers virtio-mmio devices in
    /// reverse) cannot swap the seed and the disk; `/dev/vda` otherwise.
    pub root_arg: String,
    pub mac_address: String,
    pub cmdline: String,
}

pub fn boot_info(inst: &Instance, resolved: &ResolvedConfig) -> anyhow::Result<BootInfo> {
    let gpt = vmuc_image::Gpt::read(&inst.disk())?;
    let root = gpt
        .as_ref()
        .map(vmuc_image::Gpt::find_root_partition)
        .transpose()?
        .and_then(|idx| {
            gpt.as_ref()
                .and_then(|g| g.partitions.iter().find(|p| p.index == idx).cloned())
        });
    let root_partition = root.as_ref().map(|p| p.index);
    let root_device = match root_partition {
        Some(n) => format!("/dev/vda{n}"),
        None => "/dev/vda".to_owned(),
    };
    let root_arg = match &root {
        Some(p) => format!("PARTUUID={}", p.unique_guid.to_string().to_lowercase()),
        None => "/dev/vda".to_owned(),
    };
    // Free page reporting's delay is hard-coded to 2 s in Linux 6.18
    // (`mm/page_reporting.c`); there is no command-line knob for it.
    // No earlycon: every early-console byte is a PL011 exit, and hvc0
    // replays the kernel log once it is up. Free page reporting from order
    // 5 (128 KiB, eight host pages) instead of the 2 MiB pageblock: the
    // bulk path is as fast, and an idle guest hands back ~200 MiB more
    // (docs/experiments/2026-10-11-reporting-order.md).
    let mut cmdline =
        format!("console=hvc0 root={root_arg} rw panic=-1 page_reporting.page_reporting_order=5");
    if let Some(extra) = resolved
        .images
        .first()
        .and_then(|i| i.kernel.as_ref())
        .and_then(|k| k.cmdline.as_deref())
    {
        cmdline.push(' ');
        cmdline.push_str(extra);
    }
    Ok(BootInfo {
        name: inst.name.clone(),
        user: resolved.user.name.clone(),
        cpus: resolved.cpus,
        memory_bytes: resolved.memory.bytes(),
        disk: inst.disk(),
        seed: inst.seed(),
        root_partition,
        root_device,
        root_arg,
        mac_address: resolved.network.mac_address.clone(),
        cmdline,
    })
}

pub fn delete(home: &VmucHome, name: &str) -> anyhow::Result<()> {
    let dir = home.instance_dir(name)?;
    if !dir.exists() {
        bail!("instance {name:?} does not exist");
    }
    fs::remove_dir_all(&dir).with_context(|| dir.display().to_string())?;
    Ok(())
}

/// `Stopped` without a live VMM, else the phase its latest event shows.
pub fn status(dir: &Path) -> &'static str {
    use vmuc_proto::EventKind;
    if crate::state::running_pid(dir).is_none() {
        return "Stopped";
    }
    let events = crate::state::read_events(dir);
    match events
        .iter()
        .rev()
        .map(|e| &e.kind)
        .find(|k| !matches!(k, EventKind::Requirement { .. }))
    {
        Some(EventKind::Running { .. }) => "Running",
        Some(EventKind::Degraded { .. }) => "Degraded",
        Some(EventKind::Exiting { .. }) => "Stopping",
        _ => "Starting",
    }
}

/// The guest's addresses from the latest `running` event, when running.
pub fn addresses(dir: &Path) -> Vec<std::net::IpAddr> {
    use vmuc_proto::EventKind;
    if crate::state::running_pid(dir).is_none() {
        return Vec::new();
    }
    crate::state::read_events(dir)
        .into_iter()
        .rev()
        .find_map(|e| match e.kind {
            EventKind::Running { addrs } if !addrs.is_empty() => Some(addrs),
            _ => None,
        })
        .unwrap_or_default()
}

#[derive(Debug, serde::Serialize)]
pub struct ListRow {
    pub name: String,
    pub status: &'static str,
    pub addrs: Vec<std::net::IpAddr>,
    pub cpus: u32,
    pub memory: String,
    pub disk: String,
    pub dir: PathBuf,
}

pub fn list(home: &VmucHome) -> anyhow::Result<Vec<ListRow>> {
    let mut rows = Vec::new();
    for name in home.instance_names()? {
        let inst = Instance::open(home, &name)?;
        let r = inst.resolved(home)?;
        rows.push(ListRow {
            status: status(&inst.dir),
            addrs: addresses(&inst.dir),
            name,
            cpus: r.cpus,
            memory: r.memory.to_string(),
            disk: r.disk.to_string(),
            dir: inst.dir,
        });
    }
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_name_from_locator() {
        assert_eq!(default_name("template:ubuntu"), "ubuntu");
        assert_eq!(
            default_name("template:_images/ubuntu-24.04"),
            "ubuntu-24.04"
        );
        assert_eq!(default_name("/tmp/foo.yaml"), "foo");
        assert_eq!(default_name("https://x/y/docker.yaml"), "docker");
    }

    #[test]
    fn mount_tag_is_stable_and_short() {
        let a = mount_tag(Path::new("/Users/me"), "/Users/me");
        assert_eq!(a, mount_tag(Path::new("/Users/me"), "/Users/me"));
        assert_ne!(a, mount_tag(Path::new("/Users/me"), "/mnt"));
        assert_eq!(a.len(), 13);
    }

    #[test]
    fn sparse_copy_round_trips() {
        let dir = std::env::temp_dir().join(format!("vmuc-sparse-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let src = dir.join("src");
        let mut data = vec![0u8; 3 << 20];
        data[1 << 20] = 7;
        data[(3 << 20) - 1] = 9;
        fs::write(&src, &data).unwrap();
        let dst = dir.join("dst");
        sparse_copy(&src, &dst).unwrap();
        assert_eq!(fs::read(&dst).unwrap(), data);
        let _ = fs::remove_dir_all(&dir);
    }
}
