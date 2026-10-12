// SPDX-License-Identifier: Apache-2.0
//! What the TUI watches and drives, behind a trait: the `vmuc` binary
//! implements it over instance directories and control sockets, tests
//! with fixed data.

use std::net::IpAddr;

use vmuc_proto::{Forward, InstanceStatus, Stat};

/// One instance as the list shows it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Instance {
    pub name: String,
    /// "Running", "Stopped", "Starting", ...
    pub status: String,
    pub cpus: u32,
    /// Configured memory and disk, as the config spells them.
    pub memory: String,
    pub disk: String,
    pub addrs: Vec<IpAddr>,
    /// Live data, when the VMM answered on its control socket.
    pub live: Option<Live>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Live {
    pub status: InstanceStatus,
    pub stat: Stat,
    pub forwards: Vec<Forward>,
}

/// What a key asks the source to do to the selected instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Start,
    Stop,
    ForceStop,
    /// Drop guest caches so free page reporting can hand memory back.
    Reclaim,
    /// An interactive shell; the TUI gives up the terminal meanwhile.
    Shell,
}

impl Action {
    pub fn needs_terminal(self) -> bool {
        self == Self::Shell
    }
}

pub trait Source {
    /// Every instance, running or not.
    fn instances(&mut self) -> Vec<Instance>;
    /// The last `lines` lines of the instance's serial console log.
    fn console_tail(&mut self, name: &str, lines: usize) -> Vec<String>;
    /// Starts `action`; returns a line for the status bar. Long actions
    /// (start, stop) run in the background and report through `notices`.
    fn act(&mut self, name: &str, action: Action) -> String;
    /// Outcomes of background actions since the last call.
    fn notices(&mut self) -> Vec<String>;
}
