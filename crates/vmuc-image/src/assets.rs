// SPDX-License-Identifier: Apache-2.0
//! The pinned guest kernel release.
//!
//! The kernel is built by `make kernel` on Linux and published as a GitHub
//! release asset; `vmuc-cli` downloads it through [`crate::Cache`] and
//! verifies it against [`KERNEL_SHA256`].

use std::path::{Path, PathBuf};

/// Upstream kernel version the guest kernel is built from.
pub const KERNEL_VERSION: &str = "6.12.0";

/// Release tag that carries the kernel asset.
pub const KERNEL_TAG: &str = "kernel-v6.12.0";

/// Download URL of the kernel image for the pinned release.
pub const KERNEL_URL: &str =
    "https://github.com/giejqf/vmuc/releases/download/kernel-v6.12.0/vmuc-kernel-6.12.0-arm64.img";

/// Expected sha256 of [`KERNEL_URL`], in plain hex.
///
/// The value here is a placeholder. The kernel release job rewrites this
/// constant with the digest of the asset it publishes in the same commit
/// that bumps [`KERNEL_VERSION`].
pub const KERNEL_SHA256: &str = "0000000000000000000000000000000000000000000000000000000000000000";

/// Where the kernel image lives inside a cache directory once fetched.
///
/// This is a stable, human-readable path (`kernel/vmuc-kernel-<ver>-arm64.img`)
/// so `vmuc info` can show it; it is distinct from the by-URL entry that
/// [`crate::Cache::fetch`] maintains.
pub fn kernel_path(cache_dir: &Path) -> PathBuf {
    cache_dir
        .join("kernel")
        .join(format!("vmuc-kernel-{KERNEL_VERSION}-arm64.img"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_matches_version() {
        assert!(KERNEL_URL.contains(&format!("/{KERNEL_TAG}/")));
        assert!(KERNEL_URL.ends_with(&format!("vmuc-kernel-{KERNEL_VERSION}-arm64.img")));
        assert_eq!(KERNEL_SHA256.len(), 64);
    }

    #[test]
    fn kernel_path_under_cache() {
        let p = kernel_path(Path::new("/c"));
        assert_eq!(p, PathBuf::from("/c/kernel/vmuc-kernel-6.12.0-arm64.img"));
    }
}
