// SPDX-License-Identifier: Apache-2.0
//! Guest memory: the RAM mapping, and the reclaimer that hands freed
//! guest pages back to macOS.

mod ram;
pub mod reclaim;

pub use ram::GuestRam;
pub use reclaim::{ReclaimError, ReclaimStats, Reclaimer};
