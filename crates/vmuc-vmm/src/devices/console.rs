// SPDX-License-Identifier: Apache-2.0
//! Console output: guest bytes from the UART (and later hvc0) go to a
//! buffer, and a writer thread copies them to every sink (the terminal,
//! `console.log`, attached `console.sock` clients). A vCPU exit costs a
//! mutex push, not a `write(2)` per byte, and output arrives in batches.

use std::io::Write;
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread::{self, JoinHandle};

#[derive(Default)]
struct State {
    buf: Vec<u8>,
    closed: bool,
}

#[derive(Default)]
struct Shared {
    state: Mutex<State>,
    ready: Condvar,
}

/// Cheap handle the devices push guest output into.
#[derive(Clone, Default)]
pub struct ConsoleOut {
    shared: Arc<Shared>,
}

impl std::fmt::Debug for ConsoleOut {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ConsoleOut")
    }
}

impl ConsoleOut {
    /// Starts the writer thread. It exits after [`ConsoleOut::close`]
    /// once everything pushed before has been written.
    pub fn spawn(mut sinks: Vec<Box<dyn Write + Send>>) -> (Self, JoinHandle<()>) {
        let out = Self::default();
        let shared = Arc::clone(&out.shared);
        let handle = thread::Builder::new()
            .name("console-out".into())
            .spawn(move || {
                let mut batch = Vec::new();
                loop {
                    let closed = {
                        let mut st = shared.state.lock().unwrap_or_else(PoisonError::into_inner);
                        while st.buf.is_empty() && !st.closed {
                            st = shared
                                .ready
                                .wait(st)
                                .unwrap_or_else(PoisonError::into_inner);
                        }
                        std::mem::swap(&mut batch, &mut st.buf);
                        st.closed
                    };
                    for sink in &mut sinks {
                        let _ = sink.write_all(&batch).and_then(|()| sink.flush());
                    }
                    batch.clear();
                    if closed {
                        return;
                    }
                }
            })
            .expect("spawn console writer");
        (out, handle)
    }

    pub fn push(&self, bytes: &[u8]) {
        let mut st = self
            .shared
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let was_empty = st.buf.is_empty();
        st.buf.extend_from_slice(bytes);
        if was_empty {
            self.shared.ready.notify_one();
        }
    }

    /// Lets the writer thread drain and exit.
    pub fn close(&self) {
        self.shared
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .closed = true;
        self.shared.ready.notify_one();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Default)]
    struct Collect(Arc<Mutex<Vec<u8>>>);

    impl Write for Collect {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn everything_pushed_before_close_reaches_every_sink() {
        let (a, b) = (Collect::default(), Collect::default());
        let (out, h) = ConsoleOut::spawn(vec![Box::new(a.clone()), Box::new(b.clone())]);
        for i in 0..100u8 {
            out.push(&[i]);
        }
        out.close();
        h.join().unwrap();
        let want: Vec<u8> = (0..100).collect();
        assert_eq!(*a.0.lock().unwrap(), want);
        assert_eq!(*b.0.lock().unwrap(), want);
    }
}
