// SPDX-License-Identifier: Apache-2.0
//! FSEvents, hand-written: one stream over the shared directories,
//! delivered on its own serial dispatch queue, without the changes this
//! process makes (the guest's own writes through virtiofs).

use std::ffi::{CStr, OsStr, c_char, c_void};
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;
use std::ptr;
use std::sync::mpsc::Sender;
use std::time::Duration;

use super::HostEvent;

type CFRef = *const c_void;

/// `FSEventStreamContext`.
#[repr(C)]
struct Context {
    version: isize,
    info: *mut c_void,
    retain: CFRef,
    release: CFRef,
    copy_description: CFRef,
}

type Callback = extern "C" fn(CFRef, *mut c_void, usize, *mut c_void, *const u32, *const u64);

/// `kFSEventStreamEventIdSinceNow`.
const SINCE_NOW: u64 = u64::MAX;
const UTF8: u32 = 0x0800_0100;
/// NoDefer (a change after a quiet spell comes at once, a burst after
/// the latency), IgnoreSelf, FileEvents (one event per file).
const CREATE_FLAGS: u32 = 0x2 | 0x8 | 0x10;

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    static kCFTypeArrayCallBacks: c_void;
    fn CFStringCreateWithBytes(a: CFRef, b: *const u8, n: isize, enc: u32, ext: u8) -> CFRef;
    fn CFArrayCreate(a: CFRef, values: *const CFRef, n: isize, callbacks: *const c_void) -> CFRef;
    fn CFRelease(cf: CFRef);
}

#[link(name = "CoreServices", kind = "framework")]
unsafe extern "C" {
    fn FSEventStreamCreate(
        a: CFRef,
        callback: Callback,
        context: *const Context,
        paths: CFRef,
        since: u64,
        latency: f64,
        flags: u32,
    ) -> CFRef;
    fn FSEventStreamSetDispatchQueue(stream: CFRef, queue: CFRef);
    fn FSEventStreamStart(stream: CFRef) -> u8;
    fn FSEventStreamStop(stream: CFRef);
    fn FSEventStreamInvalidate(stream: CFRef);
    fn FSEventStreamRelease(stream: CFRef);
}

// libdispatch, part of libSystem.
unsafe extern "C" {
    fn dispatch_queue_create(label: *const c_char, attr: CFRef) -> CFRef;
    fn dispatch_sync_f(queue: CFRef, context: *mut c_void, work: extern "C" fn(*mut c_void));
    fn dispatch_release(object: CFRef);
}

/// Watches directories; each batch of events goes to `tx`. Stops when
/// dropped.
#[derive(Debug)]
pub struct Watcher {
    stream: CFRef,
    queue: CFRef,
    tx: *mut Sender<Vec<HostEvent>>,
}

// SAFETY: the stream and queue are thread-safe CoreFoundation and
// dispatch objects; `tx` is only touched by the callback (on the queue)
// and freed in drop after the queue has drained.
unsafe impl Send for Watcher {}

impl Watcher {
    pub fn start(
        roots: &[PathBuf],
        latency: Duration,
        tx: Sender<Vec<HostEvent>>,
    ) -> io::Result<Self> {
        let paths = cf_paths(roots)?;
        let tx = Box::into_raw(Box::new(tx));
        let context = Context {
            version: 0,
            info: tx.cast(),
            retain: ptr::null(),
            release: ptr::null(),
            copy_description: ptr::null(),
        };
        // SAFETY: a valid context (copied by the call) and path array.
        let stream = unsafe {
            FSEventStreamCreate(
                ptr::null(),
                callback,
                &raw const context,
                paths,
                SINCE_NOW,
                latency.as_secs_f64(),
                CREATE_FLAGS,
            )
        };
        // SAFETY: our array; the stream keeps its own copy.
        unsafe { CFRelease(paths) };
        if stream.is_null() {
            // SAFETY: never shared: the stream does not exist.
            drop(unsafe { Box::from_raw(tx) });
            return Err(io::Error::other("FSEventStreamCreate failed"));
        }
        // SAFETY: a NULL attribute makes a serial queue.
        let queue = unsafe { dispatch_queue_create(c"vmuc.fsevents".as_ptr(), ptr::null()) };
        let w = Self { stream, queue, tx };
        // SAFETY: our stream and queue.
        unsafe { FSEventStreamSetDispatchQueue(stream, queue) };
        // SAFETY: as above; on failure drop releases everything.
        if unsafe { FSEventStreamStart(stream) } == 0 {
            return Err(io::Error::other("FSEventStreamStart failed"));
        }
        Ok(w)
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        // SAFETY: our stream and queue. Stopping a stream that never
        // started is allowed; the empty sync call waits out a callback in
        // flight on the serial queue before the sender goes.
        unsafe {
            FSEventStreamStop(self.stream);
            FSEventStreamInvalidate(self.stream);
            FSEventStreamRelease(self.stream);
            dispatch_sync_f(self.queue, ptr::null_mut(), drained);
            dispatch_release(self.queue);
            drop(Box::from_raw(self.tx));
        }
    }
}

extern "C" fn drained(_: *mut c_void) {}

extern "C" fn callback(
    _stream: CFRef,
    info: *mut c_void,
    n: usize,
    paths: *mut c_void,
    flags: *const u32,
    _ids: *const u64,
) {
    // SAFETY: `info` is the watcher's sender, alive until the queue has
    // drained; FSEvents passes `n` C strings (no UseCFTypes) and flags.
    let (tx, paths, flags) = unsafe {
        (
            &*info.cast::<Sender<Vec<HostEvent>>>(),
            std::slice::from_raw_parts(paths.cast::<*const c_char>(), n),
            std::slice::from_raw_parts(flags, n),
        )
    };
    let batch = paths
        .iter()
        .zip(flags)
        .map(|(&p, &flags)| HostEvent {
            // SAFETY: a NUL-terminated path from FSEvents.
            path: OsStr::from_bytes(unsafe { CStr::from_ptr(p) }.to_bytes()).into(),
            flags,
        })
        .collect();
    let _ = tx.send(batch);
}

/// A CFArray of CFStrings, owned by the caller.
fn cf_paths(roots: &[PathBuf]) -> io::Result<CFRef> {
    let strings: Vec<CFRef> = roots
        .iter()
        .map(|r| {
            let b = r.as_os_str().as_bytes();
            // SAFETY: `b` is valid for its length.
            unsafe { CFStringCreateWithBytes(ptr::null(), b.as_ptr(), b.len() as isize, UTF8, 0) }
        })
        .collect();
    let array = if strings.iter().any(|s| s.is_null()) {
        ptr::null()
    } else {
        // SAFETY: valid CFStrings; the array retains them.
        unsafe {
            CFArrayCreate(
                ptr::null(),
                strings.as_ptr(),
                strings.len() as isize,
                &raw const kCFTypeArrayCallBacks,
            )
        }
    };
    for s in strings.into_iter().filter(|s| !s.is_null()) {
        // SAFETY: our own reference.
        unsafe { CFRelease(s) };
    }
    if array.is_null() {
        return Err(io::Error::other("a shared path is not valid UTF-8"));
    }
    Ok(array)
}
