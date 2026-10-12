// SPDX-License-Identifier: Apache-2.0
//! Instance YAML schema, defaults layering, templates, validation.
//!
//! The crate is host-only and VM-agnostic: it turns YAML into an
//! [`InstanceConfig`], flattens `base:` templates ([`template`]), layers
//! built-in < user default < instance < override into a fully concrete
//! [`ResolvedConfig`] ([`defaults`]), knows where everything lives on disk
//! ([`paths`]) and renders the cloud-init NoCloud seed ([`cidata`]).
//!
//! The design follows Lima's `limayaml` / `limatmpl` / `cidata` packages;
//! no code is shared.

mod assign;
pub mod cidata;
pub mod defaults;
pub mod paths;
pub mod schema;
pub mod template;

use std::path::PathBuf;

pub use cidata::{SeedFile, new_boot_id, render_seed};
pub use defaults::{Builtin, ResolvedConfig, fill_defaults};
pub use paths::{VmucHome, validate_instance_name};
pub use schema::{ByteSize, Digest, InstanceConfig};
pub use template::{Locator, Resolution, Template, TemplateStore};

/// Errors produced by this crate.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{origin}: invalid YAML: {source}")]
    Yaml {
        origin: String,
        #[source]
        source: serde_yaml::Error,
    },
    #[error("cannot serialize config as YAML: {0}")]
    YamlSerialize(#[source] serde_yaml::Error),
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("--set {assignment:?}: {reason}")]
    InvalidAssignment { assignment: String, reason: String },
    #[error("invalid byte size {0:?} (expected e.g. \"8GiB\", \"512M\", \"1024\")")]
    InvalidByteSize(String),
    #[error("invalid digest {0:?} (expected \"sha256:<64 hex>\" or \"sha512:<128 hex>\")")]
    InvalidDigest(String),
    #[error("invalid instance name {name:?}: {reason}")]
    InvalidInstanceName { name: String, reason: String },
    #[error(
        "control socket path {path} is {len} bytes; unix socket paths are limited to 103 bytes on macOS. Use a shorter instance name or a shorter $VMUC_HOME"
    )]
    SocketPathTooLong { path: PathBuf, len: usize },
    #[error("invalid template name {0:?}: must not be empty, absolute, or contain \"..\"")]
    InvalidTemplateName(String),
    #[error("template {name:?} not found (searched {searched:?} and the built-in templates)")]
    TemplateNotFound {
        name: String,
        searched: Vec<PathBuf>,
    },
    #[error("template nesting deeper than {max} levels while embedding {locator}")]
    TemplateTooDeep { max: usize, locator: String },
    #[error("base template loop detected: {0} is already being embedded")]
    TemplateCycle(String),
    #[error("base {0} is a URL; fetch it first and embed the downloaded bytes")]
    RemoteBase(String),
    #[error("locator \"-\" requires stdin bytes")]
    StdinRequired,
    #[error("cannot determine the home directory")]
    NoHomeDir,
    #[error("{field}: {reason}")]
    Invalid { field: String, reason: String },
}

impl Error {
    pub(crate) fn invalid(field: impl Into<String>, reason: impl Into<String>) -> Self {
        Self::Invalid {
            field: field.into(),
            reason: reason.into(),
        }
    }

    pub(crate) fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }
}

/// Result alias for this crate.
pub type Result<T> = std::result::Result<T, Error>;
