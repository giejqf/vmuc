// SPDX-License-Identifier: Apache-2.0
//! Host self-check behind `vmuc info`: is this a machine vmuc can run on?

use std::fmt;

/// What `vmuc info` reports about the host.
#[derive(Debug)]
pub struct HostInfo {
    pub version: &'static str,
    pub os: &'static str,
    pub arch: &'static str,
    pub os_version: Option<String>,
    /// `kern.hv_support`: the kernel says Hypervisor.framework is usable.
    pub hv_support: Option<bool>,
    /// This build links Hypervisor.framework at all.
    pub native: bool,
    pub supported: bool,
    pub reasons: Vec<String>,
}

impl HostInfo {
    pub fn collect() -> Self {
        let os_version = sysctl_string("kern.osproductversion");
        let hv_support = sysctl_u32("kern.hv_support").map(|v| v != 0);
        let mut reasons = Vec::new();
        if !vmuc_hv::NATIVE {
            reasons.push(format!(
                "this build targets {}/{}; vmuc runs VMs only on macOS/aarch64",
                std::env::consts::OS,
                std::env::consts::ARCH
            ));
        }
        if let Some(v) = &os_version
            && !meets_minimum(v, 15)
        {
            reasons.push(format!("macOS {v} is older than the required 15.0"));
        }
        if hv_support == Some(false) {
            reasons.push("kern.hv_support is 0: Hypervisor.framework is unavailable here".into());
        }
        Self {
            version: env!("CARGO_PKG_VERSION"),
            os: std::env::consts::OS,
            arch: std::env::consts::ARCH,
            os_version,
            hv_support,
            native: vmuc_hv::NATIVE,
            supported: reasons.is_empty(),
            reasons,
        }
    }

    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "version": self.version,
            "os": self.os,
            "arch": self.arch,
            "osVersion": self.os_version,
            "hvSupport": self.hv_support,
            "native": self.native,
            "supported": self.supported,
            "reasons": self.reasons,
        })
    }
}

impl fmt::Display for HostInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "vmuc {}", self.version)?;
        writeln!(f, "host:        {}/{}", self.os, self.arch)?;
        if let Some(v) = &self.os_version {
            writeln!(f, "os version:  {v}")?;
        }
        match self.hv_support {
            Some(true) => writeln!(f, "hypervisor:  available")?,
            Some(false) => writeln!(f, "hypervisor:  unavailable")?,
            None => writeln!(f, "hypervisor:  unknown (not macOS)")?,
        }
        writeln!(
            f,
            "supported:   {}",
            if self.supported { "yes" } else { "no" }
        )?;
        for r in &self.reasons {
            writeln!(f, "  - {r}")?;
        }
        Ok(())
    }
}

/// `true` when a dotted version string's major part is at least `major`.
fn meets_minimum(version: &str, major: u32) -> bool {
    version
        .split('.')
        .next()
        .and_then(|m| m.parse::<u32>().ok())
        .is_some_and(|m| m >= major)
}

#[cfg(target_os = "macos")]
fn sysctl_string(name: &str) -> Option<String> {
    let cname = std::ffi::CString::new(name).ok()?;
    let mut len: libc::size_t = 0;
    // SAFETY: querying the length with a NULL buffer is the documented idiom.
    if unsafe {
        libc::sysctlbyname(
            cname.as_ptr(),
            std::ptr::null_mut(),
            &raw mut len,
            std::ptr::null_mut(),
            0,
        )
    } != 0
    {
        return None;
    }
    let mut buf = vec![0u8; len];
    // SAFETY: `buf` has `len` bytes; sysctl writes at most `len`.
    if unsafe {
        libc::sysctlbyname(
            cname.as_ptr(),
            buf.as_mut_ptr().cast(),
            &raw mut len,
            std::ptr::null_mut(),
            0,
        )
    } != 0
    {
        return None;
    }
    buf.truncate(len);
    while buf.last() == Some(&0) {
        buf.pop();
    }
    String::from_utf8(buf).ok()
}

#[cfg(target_os = "macos")]
fn sysctl_u32(name: &str) -> Option<u32> {
    let cname = std::ffi::CString::new(name).ok()?;
    let mut value: u32 = 0;
    let mut len: libc::size_t = size_of::<u32>();
    // SAFETY: `value` is a valid u32 out-buffer of `len` bytes.
    let rc = unsafe {
        libc::sysctlbyname(
            cname.as_ptr(),
            (&raw mut value).cast(),
            &raw mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    (rc == 0).then_some(value)
}

#[cfg(not(target_os = "macos"))]
fn sysctl_string(_name: &str) -> Option<String> {
    None
}

#[cfg(not(target_os = "macos"))]
fn sysctl_u32(_name: &str) -> Option<u32> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minimum_version_check() {
        assert!(meets_minimum("15.0", 15));
        assert!(meets_minimum("26.0.1", 15));
        assert!(!meets_minimum("14.7.1", 15));
        assert!(!meets_minimum("garbage", 15));
    }

    #[test]
    fn collect_does_not_panic_and_reports_platform() {
        let info = HostInfo::collect();
        assert_eq!(info.native, vmuc_hv::NATIVE);
        if !vmuc_hv::NATIVE {
            assert!(!info.supported);
        }
        let _ = info.to_json();
        let _ = info.to_string();
    }
}
