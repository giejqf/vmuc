// SPDX-License-Identifier: Apache-2.0
//! Image handling for vmuc: downloads and a content-addressed cache, sha256
//! digests, a read-only qcow2 reader that converts to sparse raw files, GPT
//! parsing to find the guest root partition, an ISO 9660 writer for
//! cloud-init NoCloud seeds, and sparse-file helpers.
//!
//! Nothing in this crate touches a hypervisor. It is unit-tested on Linux
//! against `qemu-img`, `sfdisk` and `isoinfo` as external oracles, and it
//! compiles unchanged for `aarch64-apple-darwin` (the only host vmuc runs on).
//!
//! Module overview:
//!
//! - [`sparse`]: create sparse files, punch holes, measure allocated bytes.
//! - [`digest`]: streaming sha256 and the `sha256:<hex>` [`Digest`] type.
//! - [`cache`]: the download [`Cache`] with per-entry locking and verification,
//!   plus gzip/xz decompression of cached images.
//! - [`qcow2`]: the [`Qcow2`] reader and [`convert_to_raw`].
//! - [`gpt`]: [`Gpt`] parsing and [`root_partition_index`].
//! - [`iso9660`]: [`write_iso`] for `cidata` seeds with Rock Ridge names.
//! - [`assets`]: the pinned guest kernel release.

pub mod assets;
pub mod cache;
pub mod digest;
pub mod gpt;
pub mod iso9660;
pub mod qcow2;
pub mod sparse;

pub use cache::{Cache, decompress_if_needed};
pub use digest::Digest;
pub use gpt::{Gpt, Guid, Partition, root_partition_index};
pub use iso9660::{SeedFile, write_iso, write_iso_file};
pub use qcow2::{Qcow2, convert_to_raw};
pub use sparse::{SparseFileExt, allocated_bytes, create_sparse};

/// Errors produced by this crate.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// An operating system or file error.
    #[error(transparent)]
    Io(#[from] std::io::Error),

    /// A downloaded or cached file did not hash to the pinned digest.
    #[error("digest mismatch for {url}: expected {expected}, got {actual}")]
    DigestMismatch {
        /// Source of the data.
        url: String,
        /// The digest the caller pinned.
        expected: Digest,
        /// The digest the bytes actually hash to.
        actual: Digest,
    },

    /// A digest string was not `sha256:<64 hex>` or `<64 hex>`.
    #[error("invalid sha256 digest {0:?}")]
    InvalidDigest(String),

    /// The image uses a feature this crate deliberately does not implement
    /// (backing files, encryption, zstd compression, MBR partitioning, ...).
    #[error("unsupported image feature: {0}")]
    Unsupported(String),

    /// The image is structurally invalid (bad magic, out-of-range offsets, ...).
    #[error("invalid image: {0}")]
    InvalidImage(String),

    /// The GPT (or protective MBR) failed validation.
    #[error("invalid GPT: {0}")]
    InvalidGpt(String),

    /// A partitioned disk has no partition that qualifies as the Linux root.
    #[error("no Linux root partition found in the GPT")]
    NoRootPartition,

    /// A URL used a scheme other than `http`, `https` or `file`.
    #[error("unsupported URL: {0}")]
    UnsupportedUrl(String),

    /// The HTTP transfer failed (connection, TLS, non-2xx status, ...).
    #[cfg(feature = "fetch")]
    #[error("fetching {url}: {source}")]
    Http {
        /// The URL being fetched.
        url: String,
        /// The transport error.
        #[source]
        source: Box<ureq::Error>,
    },

    /// A decompressor rejected the input.
    #[error("decompressing {path}: {reason}")]
    Decompress {
        /// The compressed file.
        path: std::path::PathBuf,
        /// What the decoder complained about.
        reason: String,
    },

    /// The ISO 9660 writer was given input it cannot represent.
    #[error("iso9660: {0}")]
    Iso(String),
}

/// Convenience alias used throughout the crate.
pub type Result<T> = std::result::Result<T, Error>;
