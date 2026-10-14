// SPDX-License-Identifier: Apache-2.0
//! Where vmuc keeps state on the host, and instance-name validation.
//!
//! Layout (docs/plan.md "Process and state"):
//!
//! ```text
//! $VMUC_HOME (default ~/.vmuc)/
//!   _config/{user,user.pub,default.yaml,override.yaml}
//!   _templates/
//!   <name>/vmuc.yaml vmuc-version disk.img cidata.iso vmm.pid vmm.sock
//!          events.jsonl vmm.log console.log console.sock ssh.config boot-session
//! cache: ~/Library/Caches/vmuc/ (dirs::cache_dir()/vmuc)
//! ```

use std::path::{Path, PathBuf};

use crate::{Error, Result};

/// Environment variable overriding the home directory.
pub const HOME_ENV: &str = "VMUC_HOME";
/// Colon-separated extra template directories.
pub const TEMPLATES_PATH_ENV: &str = "VMUC_TEMPLATES_PATH";

pub const CONFIG_DIR: &str = "_config";
pub const TEMPLATES_DIR: &str = "_templates";
pub const USER_DEFAULT_CONFIG: &str = "default.yaml";
pub const OVERRIDE_CONFIG: &str = "override.yaml";
pub const SSH_KEY: &str = "user";
pub const SSH_PUB_KEY: &str = "user.pub";

/// Per-instance file names.
pub mod instance {
    pub const CONFIG: &str = "vmuc.yaml";
    pub const VERSION: &str = "vmuc-version";
    pub const DISK: &str = "disk.img";
    pub const CIDATA: &str = "cidata.iso";
    pub const VMM_PID: &str = "vmm.pid";
    pub const VMM_SOCK: &str = "vmm.sock";
    pub const EVENTS: &str = "events.jsonl";
    pub const VMM_LOG: &str = "vmm.log";
    pub const CONSOLE_LOG: &str = "console.log";
    pub const CONSOLE_SOCK: &str = "console.sock";
    pub const SSH_CONFIG: &str = "ssh.config";
    pub const BOOT_SESSION: &str = "boot-session";
    /// A suspended instance's saved state (ADR 0007).
    pub const SUSPEND_DIR: &str = "suspend";
    /// The machine ID a duplicate's first boot gives its guest, until the
    /// agent answers on such a boot (ADR 0009).
    pub const NEW_MACHINE_ID: &str = "new-machine-id";
}

/// Longest instance name accepted.
pub const MAX_INSTANCE_NAME_LEN: usize = 63;
/// `sockaddr_un.sun_path` on macOS holds 104 bytes including the NUL.
pub const MAX_SOCKET_PATH_LEN: usize = 103;

/// The vmuc home directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VmucHome(PathBuf);

impl VmucHome {
    /// `$VMUC_HOME`, else `~/.vmuc`.
    pub fn detect() -> Result<Self> {
        if let Some(home) = std::env::var_os(HOME_ENV).filter(|v| !v.is_empty()) {
            return Ok(Self(PathBuf::from(home)));
        }
        dirs::home_dir()
            .map(|h| Self(h.join(".vmuc")))
            .ok_or(Error::NoHomeDir)
    }

    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self(path.into())
    }

    pub fn root(&self) -> &Path {
        &self.0
    }

    pub fn config_dir(&self) -> PathBuf {
        self.0.join(CONFIG_DIR)
    }

    pub fn templates_dir(&self) -> PathBuf {
        self.0.join(TEMPLATES_DIR)
    }

    pub fn user_default_config(&self) -> PathBuf {
        self.config_dir().join(USER_DEFAULT_CONFIG)
    }

    pub fn override_config(&self) -> PathBuf {
        self.config_dir().join(OVERRIDE_CONFIG)
    }

    pub fn ssh_key(&self) -> PathBuf {
        self.config_dir().join(SSH_KEY)
    }

    pub fn ssh_pub_key(&self) -> PathBuf {
        self.config_dir().join(SSH_PUB_KEY)
    }

    /// `~/Library/Caches/vmuc` on macOS.
    pub fn cache_dir() -> Result<PathBuf> {
        dirs::cache_dir()
            .map(|c| c.join("vmuc"))
            .ok_or(Error::NoHomeDir)
    }

    /// The instance directory after validating the name and that every
    /// socket path in it (the longest is `console.sock`) fits the
    /// platform limit.
    pub fn instance_dir(&self, name: &str) -> Result<PathBuf> {
        validate_instance_name(name)?;
        let dir = self.0.join(name);
        validate_socket_path(&dir.join(instance::CONSOLE_SOCK))?;
        Ok(dir)
    }

    /// Lists instance directories (those holding a `vmuc.yaml`), sorted.
    pub fn instance_names(&self) -> Result<Vec<String>> {
        let entries = match std::fs::read_dir(&self.0) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(Error::io(&self.0, e)),
        };
        let mut names = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|e| Error::io(&self.0, e))?;
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            if validate_instance_name(&name).is_ok()
                && entry.path().join(instance::CONFIG).is_file()
            {
                names.push(name);
            }
        }
        names.sort();
        Ok(names)
    }
}

/// `[a-zA-Z0-9][a-zA-Z0-9_.-]*`, at most 63 characters.
pub fn validate_instance_name(name: &str) -> Result<()> {
    let err = |reason: &str| Error::InvalidInstanceName {
        name: name.to_owned(),
        reason: reason.to_owned(),
    };
    let mut chars = name.chars();
    match chars.next() {
        None => return Err(err("must not be empty")),
        Some(c) if !c.is_ascii_alphanumeric() => {
            return Err(err("must start with a letter or digit"));
        }
        Some(_) => {}
    }
    if let Some(bad) = chars.find(|c| !(c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-')))
    {
        return Err(err(&format!(
            "contains {bad:?}; only letters, digits, '_', '.' and '-' are allowed"
        )));
    }
    if name.len() > MAX_INSTANCE_NAME_LEN {
        return Err(err(&format!(
            "is {} characters; the maximum is {MAX_INSTANCE_NAME_LEN}",
            name.len()
        )));
    }
    Ok(())
}

/// A unix socket path must fit `sun_path` (104 bytes with NUL on macOS).
pub fn validate_socket_path(path: &Path) -> Result<()> {
    let len = path.as_os_str().len();
    if len > MAX_SOCKET_PATH_LEN {
        return Err(Error::SocketPathTooLong {
            path: path.to_path_buf(),
            len,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_follows_the_plan() {
        let h = VmucHome::new("/Users/alice/.vmuc");
        assert_eq!(h.config_dir(), PathBuf::from("/Users/alice/.vmuc/_config"));
        assert_eq!(
            h.templates_dir(),
            PathBuf::from("/Users/alice/.vmuc/_templates")
        );
        assert_eq!(
            h.user_default_config(),
            PathBuf::from("/Users/alice/.vmuc/_config/default.yaml")
        );
        assert_eq!(
            h.override_config(),
            PathBuf::from("/Users/alice/.vmuc/_config/override.yaml")
        );
        assert_eq!(
            h.ssh_pub_key(),
            PathBuf::from("/Users/alice/.vmuc/_config/user.pub")
        );
        assert_eq!(
            h.instance_dir("default").unwrap(),
            PathBuf::from("/Users/alice/.vmuc/default")
        );
    }

    #[test]
    fn instance_names() {
        for ok in ["a", "default", "web-1", "x.y_z", "9lives", &"a".repeat(63)] {
            validate_instance_name(ok).unwrap_or_else(|e| panic!("{ok}: {e}"));
        }
        for (bad, msg) in [
            ("", "empty"),
            ("_config", "start with"),
            ("-x", "start with"),
            (".hidden", "start with"),
            ("a/b", "'/'"),
            ("a b", "' '"),
            ("aünïcode", "'ü'"),
            (&"a".repeat(64), "maximum"),
        ] {
            let err = validate_instance_name(bad).unwrap_err().to_string();
            assert!(err.contains(msg), "{bad:?}: {err}");
        }
    }

    #[test]
    fn socket_path_limit() {
        let short = VmucHome::new("/Users/alice/.vmuc");
        assert!(short.instance_dir("default").is_ok());
        // 103 bytes exactly is fine, 104 is not.
        let base = "/".to_owned() + &"d".repeat(79); // 80 bytes
        let home = VmucHome::new(&base);
        let name = "n".repeat(103 - 80 - 1 - 1 - instance::CONSOLE_SOCK.len());
        assert!(home.instance_dir(&name).is_ok());
        let name = name + "n";
        let err = home.instance_dir(&name).unwrap_err().to_string();
        assert!(err.contains("104 bytes"), "{err}");
        assert!(err.contains("shorter instance name"), "{err}");
    }

    #[test]
    fn home_from_env_or_default() {
        // Only the pure helpers are exercised: the environment is process-global.
        let h = VmucHome::new("/x");
        assert_eq!(h.root(), Path::new("/x"));
    }

    #[test]
    fn instance_names_lists_only_instance_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let home = VmucHome::new(tmp.path());
        assert_eq!(home.instance_names().unwrap(), [] as [String; 0]);
        std::fs::create_dir_all(tmp.path().join("_config")).unwrap();
        std::fs::create_dir_all(tmp.path().join("b")).unwrap();
        std::fs::write(tmp.path().join("b").join(instance::CONFIG), "").unwrap();
        std::fs::create_dir_all(tmp.path().join("a")).unwrap();
        std::fs::write(tmp.path().join("a").join(instance::CONFIG), "").unwrap();
        std::fs::create_dir_all(tmp.path().join("empty")).unwrap();
        assert_eq!(home.instance_names().unwrap(), ["a", "b"]);
        let missing = VmucHome::new(tmp.path().join("missing"));
        assert_eq!(missing.instance_names().unwrap(), [] as [String; 0]);
    }
}
