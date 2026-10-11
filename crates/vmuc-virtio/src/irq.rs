// SPDX-License-Identifier: Apache-2.0
//! Interrupt lines from devices to the guest.
//!
//! Every device interrupt in vmuc is a level-triggered SPI on Apple's
//! in-kernel GICv3: the device asserts the line while it has something
//! to report and deasserts it when the guest acknowledges. Asserting is
//! the whole wake-up; nothing else kicks a vCPU.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

/// One level-triggered interrupt line.
pub trait Interrupt: Send + Sync {
    fn set_level(&self, asserted: bool);
}

/// Records the line level, for device unit tests.
#[derive(Debug, Default)]
pub struct TestIrq {
    level: AtomicBool,
    history: Mutex<Vec<bool>>,
}

impl TestIrq {
    pub fn level(&self) -> bool {
        self.level.load(Ordering::SeqCst)
    }

    /// Every level ever set, in order.
    pub fn history(&self) -> Vec<bool> {
        self.history
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

impl Interrupt for TestIrq {
    fn set_level(&self, asserted: bool) {
        self.level.store(asserted, Ordering::SeqCst);
        self.history
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(asserted);
    }
}
