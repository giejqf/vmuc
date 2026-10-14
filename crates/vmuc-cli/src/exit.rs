// SPDX-License-Identifier: Apache-2.0
//! How a command ends for whoever called it (ADR 0010): an exit code by
//! the kind of failure, the failure as JSON on stderr with `--json`, and
//! with `--json` a result as one JSON document on stdout.

use std::fmt;
use std::process::ExitCode;

use vmuc_config::VmucHome;

/// What went wrong, as far as a caller acting on it cares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Anything below.
    Failed,
    /// A bad argument or configuration.
    Invalid,
    NotFound,
    /// Running, stopped or suspended when the command needs otherwise.
    WrongState,
    Exists,
    TimedOut,
}

/// The exit status of `exec` and `shell` when vmuc fails before the
/// program runs (otherwise they exit with the program's).
pub const NOT_RUN: u8 = 125;

impl Kind {
    pub fn code(self) -> u8 {
        match self {
            Self::Failed => 1,
            Self::Invalid => 2,
            Self::NotFound => 3,
            Self::WrongState => 4,
            Self::Exists => 5,
            Self::TimedOut => 6,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Failed => "failed",
            Self::Invalid => "invalid",
            Self::NotFound => "not_found",
            Self::WrongState => "wrong_state",
            Self::Exists => "exists",
            Self::TimedOut => "timed_out",
        }
    }

    /// The kind behind an exit code: a VMM is this binary, and exits
    /// with these codes too.
    pub fn from_code(code: i32) -> Self {
        [
            Self::Invalid,
            Self::NotFound,
            Self::WrongState,
            Self::Exists,
            Self::TimedOut,
        ]
        .into_iter()
        .find(|k| i32::from(k.code()) == code)
        .unwrap_or(Self::Failed)
    }

    /// An error of this kind.
    pub fn err(self, message: impl Into<String>) -> anyhow::Error {
        Failure {
            kind: self,
            message: message.into(),
        }
        .into()
    }

    /// The kind of `e`: that of a [`Failure`] in it, `not_found` for a
    /// missing template, `invalid` for any other configuration error.
    pub fn of(e: &anyhow::Error) -> Self {
        for cause in e.chain() {
            if let Some(f) = cause.downcast_ref::<Failure>() {
                return f.kind;
            }
            if let Some(c) = cause.downcast_ref::<vmuc_config::Error>() {
                return match c {
                    vmuc_config::Error::TemplateNotFound { .. } => Self::NotFound,
                    vmuc_config::Error::Io { .. } | vmuc_config::Error::NoHomeDir => Self::Failed,
                    _ => Self::Invalid,
                };
            }
        }
        Self::Failed
    }
}

/// A failure of a known [`Kind`].
#[derive(Debug)]
pub struct Failure {
    pub kind: Kind,
    pub message: String,
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Failure {}

/// `anyhow::bail!` with a [`Kind`].
macro_rules! fail {
    ($kind:expr, $($arg:tt)+) => {
        return Err($kind.err(format!($($arg)+)))
    };
}
pub(crate) use fail;

/// Prints `e` for the caller and returns the exit status: text, or with
/// `json` `{"error": {"kind", "message"}}`, on stderr. `program` is set
/// for `exec` and `shell`, whose own failures exit [`NOT_RUN`].
pub fn report(e: &anyhow::Error, json: bool, program: bool) -> ExitCode {
    let kind = Kind::of(e);
    if json {
        let error = serde_json::json!({ "kind": kind.name(), "message": format!("{e:#}") });
        eprintln!("{}", serde_json::json!({ "error": error }));
    } else {
        eprintln!("Error: {e:?}");
    }
    ExitCode::from(if program { NOT_RUN } else { kind.code() })
}

/// With `--json`, a command's result: one document on stdout.
pub fn print_json(value: &serde_json::Value) -> anyhow::Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

/// With `--json`, an instance command's result: the instance as `list`
/// shows it, plus the command's own fields from `extra` (an object).
pub fn print_instance(home: &VmucHome, name: &str, extra: serde_json::Value) -> anyhow::Result<()> {
    let mut v = serde_json::to_value(crate::instance::row(home, name)?)?;
    if let (Some(o), serde_json::Value::Object(x)) = (v.as_object_mut(), extra) {
        o.extend(x);
    }
    print_json(&v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_survive_context_and_config_errors_have_theirs() {
        use anyhow::Context;
        let e = Err::<(), _>(Kind::WrongState.err("dev is running"))
            .context("stopping dev")
            .unwrap_err();
        assert_eq!(Kind::of(&e), Kind::WrongState);
        assert_eq!(format!("{e:#}"), "stopping dev: dev is running");

        let missing = vmuc_config::Error::TemplateNotFound {
            name: "nope".into(),
            searched: vec![],
        };
        assert_eq!(Kind::of(&missing.into()), Kind::NotFound);
        let bad = vmuc_config::Error::InvalidByteSize("lots".into());
        assert_eq!(Kind::of(&bad.into()), Kind::Invalid);
        assert_eq!(Kind::of(&anyhow::anyhow!("disk full")), Kind::Failed);
    }

    #[test]
    fn codes_are_distinct_and_named() {
        let all = [
            Kind::Failed,
            Kind::Invalid,
            Kind::NotFound,
            Kind::WrongState,
            Kind::Exists,
            Kind::TimedOut,
        ];
        let codes: std::collections::HashSet<_> = all.iter().map(|k| k.code()).collect();
        assert_eq!(codes.len(), all.len());
        assert!(all.iter().all(|k| k.code() != 0 && k.code() != NOT_RUN));
        assert_eq!(Kind::WrongState.name(), "wrong_state");
        assert!(all.iter().all(|&k| Kind::from_code(k.code().into()) == k));
        assert_eq!(Kind::from_code(101), Kind::Failed, "a panic");
    }
}
