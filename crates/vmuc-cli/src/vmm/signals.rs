// SPDX-License-Identifier: Apache-2.0
//! SIGTERM, SIGINT and SIGHUP are blocked in every thread and taken by
//! one thread with `sigwait`: the first asks the guest to power off, a
//! second forces the VM down.

use std::sync::Arc;

use super::Shared;

/// Proof that the signals are blocked (call before spawning threads).
#[derive(Clone, Copy)]
pub struct Blocked(libc::sigset_t);

pub fn block() -> std::io::Result<Blocked> {
    // SAFETY: sigemptyset/sigaddset initialise a local set; pthread_sigmask
    // only changes this thread's mask, inherited by threads it spawns.
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&raw mut set);
        for sig in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP] {
            libc::sigaddset(&raw mut set, sig);
        }
        let rc = libc::pthread_sigmask(libc::SIG_BLOCK, &raw const set, std::ptr::null_mut());
        if rc != 0 {
            return Err(std::io::Error::from_raw_os_error(rc));
        }
        Ok(Blocked(set))
    }
}

pub fn spawn(blocked: Blocked, shared: Arc<Shared>) -> std::io::Result<()> {
    std::thread::Builder::new()
        .name("signals".into())
        .spawn(move || {
            let mut asked = false;
            loop {
                let mut sig = 0;
                // SAFETY: the set is initialised and blocked in this thread.
                if unsafe { libc::sigwait(&raw const blocked.0, &raw mut sig) } != 0 {
                    return;
                }
                tracing::info!(sig, forced = asked, "stop requested by signal");
                shared.stop(asked);
                asked = true;
            }
        })?;
    Ok(())
}
