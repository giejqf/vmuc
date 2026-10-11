// SPDX-License-Identifier: Apache-2.0
//! The host terminal as a serial console: raw mode while a VM runs in
//! the foreground, and a stdin pump with QEMU's escape (`Ctrl-A x` quits,
//! `Ctrl-A Ctrl-A` sends a literal `Ctrl-A`).

use std::io::Read;
use std::os::fd::AsRawFd;

/// Puts stdin in raw mode until dropped; a no-op when stdin is not a tty.
pub struct RawMode {
    saved: Option<libc::termios>,
}

impl RawMode {
    pub fn enable() -> Self {
        let fd = std::io::stdin().as_raw_fd();
        // SAFETY: zeroed termios is a valid out-buffer for tcgetattr.
        let mut t: libc::termios = unsafe { std::mem::zeroed() };
        // SAFETY: valid fd and out-pointer.
        if unsafe { libc::isatty(fd) } != 1 || unsafe { libc::tcgetattr(fd, &raw mut t) } != 0 {
            return Self { saved: None };
        }
        let saved = t;
        // SAFETY: `t` is an initialised termios.
        unsafe { libc::cfmakeraw(&raw mut t) };
        // Keep output post-processing so guest "\n" still returns the carriage.
        t.c_oflag |= libc::OPOST;
        // SAFETY: valid fd and termios.
        unsafe { libc::tcsetattr(fd, libc::TCSANOW, &raw const t) };
        Self { saved: Some(saved) }
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        if let Some(t) = self.saved {
            // SAFETY: restoring the attributes read in `enable`.
            unsafe { libc::tcsetattr(std::io::stdin().as_raw_fd(), libc::TCSANOW, &raw const t) };
        }
    }
}

/// What a chunk of keyboard input means after escape processing.
#[derive(Debug, Default)]
pub struct Escape {
    pending: bool,
}

impl Escape {
    const CTRL_A: u8 = 0x01;

    /// Filters `input` into `out` (bytes for the guest); `true` when the
    /// user asked to quit.
    pub fn feed(&mut self, input: &[u8], out: &mut Vec<u8>) -> bool {
        for &b in input {
            if self.pending {
                self.pending = false;
                match b {
                    b'x' | b'X' => return true,
                    Self::CTRL_A => out.push(Self::CTRL_A),
                    _ => out.extend([Self::CTRL_A, b]),
                }
            } else if b == Self::CTRL_A {
                self.pending = true;
            } else {
                out.push(b);
            }
        }
        false
    }
}

/// Reads stdin forever, handing guest-bound bytes to `deliver` and
/// calling `quit` on `Ctrl-A x` or end of input. Runs on its own thread;
/// the process exits without joining it.
pub fn pump_stdin(deliver: impl Fn(&[u8]), quit: impl Fn()) {
    let mut stdin = std::io::stdin().lock();
    let mut esc = Escape::default();
    let mut buf = [0u8; 4096];
    let mut out = Vec::with_capacity(buf.len());
    loop {
        let n = match stdin.read(&mut buf) {
            Ok(0) | Err(_) => return,
            Ok(n) => n,
        };
        out.clear();
        let stop = esc.feed(&buf[..n], &mut out);
        if !out.is_empty() {
            deliver(&out);
        }
        if stop {
            quit();
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Escape;

    #[test]
    fn escape_sequences() {
        let mut e = Escape::default();
        let mut out = Vec::new();
        assert!(!e.feed(b"ls\r", &mut out));
        assert_eq!(out, b"ls\r");
        out.clear();
        assert!(!e.feed(&[1, 1, b'a'], &mut out));
        assert_eq!(out, [1, b'a']);
        out.clear();
        // Split across reads.
        assert!(!e.feed(&[b'q', 1], &mut out));
        assert!(e.feed(b"x", &mut out));
        assert_eq!(out, b"q");
        let mut e = Escape::default();
        out.clear();
        assert!(!e.feed(&[1, b'c'], &mut out));
        assert_eq!(out, [1, b'c']);
    }
}
