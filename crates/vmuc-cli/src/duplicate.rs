// SPDX-License-Identifier: Apache-2.0
//! `vmuc duplicate`: a stopped instance's configuration and disk under a
//! new name, the disk cloned copy-on-write, and a new machine ID for the
//! copy's guest on its first boot (ADR 0009).

use std::fs;
use std::path::Path;

use anyhow::Context;
use vmuc_config::VmucHome;
use vmuc_config::paths::instance as files;
use vmuc_image::CloneKind;

use crate::exit::{Kind, fail};
use crate::instance::{self, CreateOverrides, Instance};
use crate::state;

/// Copies `source` to a new instance `name`, `overrides` applied to the
/// copy's configuration. The source must be stopped.
pub fn duplicate(
    home: &VmucHome,
    source: &str,
    name: &str,
    overrides: &CreateOverrides,
) -> anyhow::Result<(Instance, CloneKind)> {
    let src = Instance::open(home, source)?;
    if let Some(pid) = state::running_pid(&src.dir) {
        fail!(
            Kind::WrongState,
            "{source} is running (pid {pid}); stop it first"
        );
    }
    if crate::suspend::exists(&src.dir) {
        fail!(
            Kind::WrongState,
            "{source} is suspended, and its disk lacks what its memory holds; \
             resume and stop it, or discard the state with `vmuc stop --force {source}`"
        );
    }
    let mut cfg = src.config()?;
    // The copy's MAC follows its own name, and with it its DHCP lease.
    cfg.network.mac_address = None;
    instance::apply_overrides(&mut cfg, overrides)?;
    if !overrides.set.is_empty() {
        cfg = cfg.with_assignments(&overrides.set)?;
    }
    let dir = home.instance_dir(name)?;
    if dir.exists() {
        fail!(
            Kind::Exists,
            "instance {name:?} already exists at {}",
            dir.display()
        );
    }
    // Validate before touching the filesystem.
    let resolved = instance::resolve(home, name, &cfg)?;
    let have = fs::metadata(src.disk())
        .with_context(|| src.disk().display().to_string())?
        .len();
    let want = overrides.disk.as_ref().map(|_| resolved.disk.bytes());
    if want.is_some_and(|w| w < have) {
        fail!(
            Kind::Invalid,
            "{source}'s disk is {} MiB; a copy's disk can grow, not shrink",
            have >> 20
        );
    }

    fs::create_dir_all(&dir).with_context(|| dir.display().to_string())?;
    let copy = Instance {
        name: name.to_owned(),
        dir,
    };
    let made = fill(&src, &copy, &cfg, want.filter(|&w| w > have));
    if made.is_err() {
        let _ = fs::remove_dir_all(&copy.dir);
    }
    Ok((copy, made?))
}

fn fill(
    src: &Instance,
    copy: &Instance,
    cfg: &vmuc_config::InstanceConfig,
    grow_to: Option<u64>,
) -> anyhow::Result<CloneKind> {
    fs::write(copy.dir.join(files::CONFIG), cfg.to_yaml()?)?;
    if let Ok(version) = fs::read(src.dir.join(files::VERSION)) {
        fs::write(copy.dir.join(files::VERSION), version)?;
    }
    let tmp = copy.disk().with_extension("img.partial");
    let kind = vmuc_image::clone_file(&src.disk(), &tmp)?;
    if let Some(len) = grow_to {
        fs::OpenOptions::new()
            .write(true)
            .open(&tmp)?
            .set_len(len)?;
    }
    fs::rename(&tmp, copy.disk())?;
    fs::write(
        copy.dir.join(files::NEW_MACHINE_ID),
        vmuc_config::new_machine_id() + "\n",
    )?;
    Ok(kind)
}

/// The kernel parameter that gives a duplicate's guest its own machine ID,
/// while one is still to be set: systemd writes it to `/etc/machine-id`.
pub fn machine_id_param(dir: &Path) -> Option<String> {
    let text = fs::read_to_string(dir.join(files::NEW_MACHINE_ID)).ok()?;
    let id = text.trim();
    let hex = |b: u8| b.is_ascii_digit() || (b'a'..=b'f').contains(&b);
    (id.len() == 32 && id.bytes().all(hex) && id.bytes().any(|b| b != b'0'))
        .then(|| format!("systemd.machine_id={id}"))
}

/// The guest's agent answered, so systemd has kept the machine ID.
pub fn machine_id_kept(dir: &Path) {
    let _ = fs::remove_file(dir.join(files::NEW_MACHINE_ID));
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONFIG: &str = "images:\n- location: https://example.com/x.img\ncpus: 2\n\
                          network:\n  macAddress: 0a:00:00:00:00:01\n";

    fn source(home: &tempfile::TempDir) -> Instance {
        let dir = home.path().join("src");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(files::CONFIG), CONFIG).unwrap();
        fs::write(dir.join(files::VERSION), "0.1.0\n").unwrap();
        fs::write(dir.join(files::CONSOLE_LOG), "boot\n").unwrap();
        let mut disk = vec![0u8; 1 << 20];
        disk[4096] = 42;
        fs::write(dir.join(files::DISK), &disk).unwrap();
        Instance {
            name: "src".into(),
            dir,
        }
    }

    #[test]
    fn a_copy_has_the_config_and_disk_but_its_own_identity() {
        let tmp = tempfile::tempdir().unwrap();
        let home = VmucHome::new(tmp.path());
        let src = source(&tmp);
        let o = CreateOverrides {
            cpus: Some(4),
            ..CreateOverrides::default()
        };
        let (copy, _) = duplicate(&home, "src", "dup", &o).unwrap();
        let cfg = copy.config().unwrap();
        assert_eq!(cfg.cpus, Some(4));
        assert_eq!(cfg.network.mac_address, None, "the MAC follows the name");
        assert_eq!(
            fs::read(copy.disk()).unwrap(),
            fs::read(src.disk()).unwrap()
        );
        assert_eq!(
            fs::read_to_string(copy.dir.join(files::VERSION)).unwrap(),
            "0.1.0\n"
        );
        assert!(!copy.dir.join(files::CONSOLE_LOG).exists(), "runs stay");
        let param = machine_id_param(&copy.dir).unwrap();
        assert!(param.starts_with("systemd.machine_id="));
        machine_id_kept(&copy.dir);
        assert_eq!(machine_id_param(&copy.dir), None);
        assert_eq!(
            fs::read_to_string(src.dir.join(files::CONFIG)).unwrap(),
            CONFIG,
            "the source is untouched"
        );
    }

    #[test]
    fn a_disk_can_grow_not_shrink() {
        let tmp = tempfile::tempdir().unwrap();
        let home = VmucHome::new(tmp.path());
        let src = source(&tmp);
        let disk = fs::OpenOptions::new().write(true).open(src.disk()).unwrap();
        disk.set_len(3 << 30).unwrap();
        let size = |disk: &str| CreateOverrides {
            disk: Some(disk.into()),
            ..CreateOverrides::default()
        };
        let err = duplicate(&home, "src", "small", &size("2GiB")).unwrap_err();
        assert!(err.to_string().contains("not shrink"), "{err}");
        assert!(!tmp.path().join("small").exists());
        let (copy, _) = duplicate(&home, "src", "big", &size("4GiB")).unwrap();
        assert_eq!(fs::metadata(copy.disk()).unwrap().len(), 4 << 30);
        let (same, _) = duplicate(&home, "src", "same", &CreateOverrides::default()).unwrap();
        assert_eq!(
            fs::metadata(same.disk()).unwrap().len(),
            3 << 30,
            "no --disk, no change"
        );
    }

    #[test]
    fn only_a_stopped_source_is_copied_and_never_over_an_instance() {
        let tmp = tempfile::tempdir().unwrap();
        let home = VmucHome::new(tmp.path());
        let src = source(&tmp);
        let none = CreateOverrides::default();
        state::write_pid(&src.dir).unwrap();
        let err = duplicate(&home, "src", "a", &none).unwrap_err();
        assert!(err.to_string().contains("is running"), "{err}");
        fs::remove_file(src.dir.join(files::VMM_PID)).unwrap();

        let saved = src.dir.join(files::SUSPEND_DIR);
        fs::create_dir(&saved).unwrap();
        fs::write(saved.join("suspend.json"), b"{}").unwrap();
        let err = duplicate(&home, "src", "a", &none).unwrap_err();
        assert!(err.to_string().contains("is suspended"), "{err}");
        fs::remove_dir_all(&saved).unwrap();

        duplicate(&home, "src", "a", &none).unwrap();
        let err = duplicate(&home, "src", "a", &none).unwrap_err();
        assert!(err.to_string().contains("already exists"), "{err}");
    }

    #[test]
    fn only_a_well_formed_machine_id_is_passed() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        assert_eq!(machine_id_param(dir), None);
        for bad in ["", "xyz", &"0".repeat(32), &"A".repeat(32), &"a".repeat(31)] {
            fs::write(dir.join(files::NEW_MACHINE_ID), bad).unwrap();
            assert_eq!(machine_id_param(dir), None, "{bad:?}");
        }
        let id = vmuc_config::new_machine_id();
        fs::write(dir.join(files::NEW_MACHINE_ID), format!("{id}\n")).unwrap();
        assert_eq!(
            machine_id_param(dir),
            Some(format!("systemd.machine_id={id}"))
        );
    }
}
