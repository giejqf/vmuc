// SPDX-License-Identifier: Apache-2.0
//! ratatui front end for watching and driving instances (`vmuc tui`).
//!
//! A client like the CLI: everything it shows comes from a [`Source`],
//! which the `vmuc` binary implements over instance directories and
//! control sockets.

pub mod app;
pub mod history;
pub mod source;
pub mod text;

pub use app::{App, Command};
pub use source::{Action, Instance, Live, Source};
