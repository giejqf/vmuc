// SPDX-License-Identifier: Apache-2.0
//! `vmuc-vmnet-helper`: the only part of vmuc that runs as root.
//!
//! Started by the VMM through `sudo -n` with a connected `SOCK_DGRAM`
//! unix socket as stdin (sudo keeps fds 0-2). It opens one vmnet
//! interface, sends a [`Hello`] datagram describing it, then relays
//! Ethernet frames both ways, one per datagram, until the VMM closes the
//! socket. It parses nothing but its two command-line flags; frames are
//! opaque to it. The guest keeps the MAC address vmuc gave it
//! (`vmnet_allocate_mac_address_key = false`).

use serde::{Deserialize, Serialize};

/// What the helper tells the VMM before the first frame.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hello {
    pub mtu: u64,
    pub max_packet_size: u64,
    /// The DHCP range vmnet hands out (shared mode); the first address is
    /// the host's side of the bridge.
    pub start: Option<String>,
    pub end: Option<String>,
    pub mask: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    Shared,
    Bridged(String),
}

impl Mode {
    /// `--mode shared` or `--mode bridged --interface en0`; nothing else.
    pub fn from_args(args: &[String]) -> Result<Self, String> {
        match args {
            [m, shared] if m == "--mode" && shared == "shared" => Ok(Self::Shared),
            [m, bridged, i, name]
                if m == "--mode" && bridged == "bridged" && i == "--interface" =>
            {
                let ok = !name.is_empty()
                    && name.len() <= 15
                    && name.bytes().all(|b| b.is_ascii_alphanumeric());
                if ok {
                    Ok(Self::Bridged(name.clone()))
                } else {
                    Err(format!("bad interface name {name:?}"))
                }
            }
            _ => Err(
                "usage: vmuc-vmnet-helper --mode shared | --mode bridged --interface NAME".into(),
            ),
        }
    }

    /// The arguments that select this mode (what sudoers must allow).
    pub fn to_args(&self) -> Vec<String> {
        match self {
            Self::Shared => vec!["--mode".into(), "shared".into()],
            Self::Bridged(i) => vec![
                "--mode".into(),
                "bridged".into(),
                "--interface".into(),
                i.clone(),
            ],
        }
    }
}

#[cfg(target_os = "macos")]
pub use relay::run;

#[cfg(target_os = "macos")]
mod relay {
    use std::ffi::c_void;
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::net::UnixDatagram;

    use super::{Hello, Mode};
    use crate::vmnet::{
        VMNET_BRIDGED_MODE, VMNET_INTERFACE_PACKETS_AVAILABLE, VMNET_SHARED_MODE, VMNET_SUCCESS,
        cstring, dict_string, dispatch_queue_create, dispatch_semaphore_create,
        dispatch_semaphore_signal, dispatch_semaphore_t, dispatch_semaphore_wait, dispatch_time,
        event_block, interface_ref, status_block, status_params_block,
        vmnet_allocate_mac_address_key, vmnet_end_address_key, vmnet_interface_set_event_callback,
        vmnet_max_packet_size_key, vmnet_mtu_key, vmnet_operation_mode_key, vmnet_read,
        vmnet_return_t, vmnet_shared_interface_name_key, vmnet_start_address_key,
        vmnet_start_interface, vmnet_stop_interface, vmnet_subnet_mask_key, vmnet_write, vmpktdesc,
        xpc_dictionary_create, xpc_dictionary_get_uint64, xpc_dictionary_set_bool,
        xpc_dictionary_set_string, xpc_dictionary_set_uint64, xpc_object_t, xpc_release,
    };

    const BATCH: usize = 64;

    struct Start {
        sem: dispatch_semaphore_t,
        status: vmnet_return_t,
        hello: Option<Hello>,
    }

    unsafe extern "C" fn on_start(ctx: *mut c_void, status: vmnet_return_t, params: xpc_object_t) {
        // SAFETY: `ctx` is the `Start` that `run` keeps alive while it waits.
        let s = unsafe { &mut *ctx.cast::<Start>() };
        s.status = status;
        if status == VMNET_SUCCESS && !params.is_null() {
            // SAFETY: `params` is valid for the duration of this callback.
            s.hello = Some(unsafe {
                Hello {
                    mtu: xpc_dictionary_get_uint64(params, vmnet_mtu_key),
                    max_packet_size: xpc_dictionary_get_uint64(params, vmnet_max_packet_size_key),
                    start: dict_string(params, vmnet_start_address_key),
                    end: dict_string(params, vmnet_end_address_key),
                    mask: dict_string(params, vmnet_subnet_mask_key),
                }
            });
        }
        // SAFETY: the semaphore outlives the wait in `run`.
        unsafe { dispatch_semaphore_signal(s.sem) };
    }

    /// Frames from vmnet to the VMM, on vmnet's dispatch queue.
    struct ToVmm {
        iface: interface_ref,
        fd: i32,
        bufs: Vec<Vec<u8>>,
    }

    unsafe extern "C" fn on_packets(ctx: *mut c_void, _mask: u32, _event: xpc_object_t) {
        // SAFETY: `ctx` is the leaked `ToVmm`, used only on this serial queue.
        let t = unsafe { &mut *ctx.cast::<ToVmm>() };
        loop {
            let mut iov: Vec<libc::iovec> = t
                .bufs
                .iter_mut()
                .map(|b| libc::iovec {
                    iov_base: b.as_mut_ptr().cast(),
                    iov_len: b.len(),
                })
                .collect();
            let mut pkts: Vec<vmpktdesc> = iov
                .iter_mut()
                .map(|v| vmpktdesc {
                    vm_pkt_size: v.iov_len,
                    vm_pkt_iov: v,
                    vm_pkt_iovcnt: 1,
                    vm_flags: 0,
                })
                .collect();
            let mut count = pkts.len() as i32;
            // SAFETY: every descriptor points at one of our buffers.
            let st = unsafe { vmnet_read(t.iface, pkts.as_mut_ptr(), &raw mut count) };
            if st != VMNET_SUCCESS || count <= 0 {
                return;
            }
            for p in &pkts[..count as usize] {
                // SAFETY: vmnet filled `vm_pkt_size` bytes of the buffer.
                unsafe { libc::send(t.fd, (*p.vm_pkt_iov).iov_base, p.vm_pkt_size, 0) };
            }
        }
    }

    unsafe extern "C" fn on_stop(ctx: *mut c_void, _status: vmnet_return_t) {
        // SAFETY: `ctx` is the semaphore `run` waits on.
        unsafe { dispatch_semaphore_signal(ctx) };
    }

    /// Starts the interface, sends the hello, relays until the VMM goes.
    pub fn run(mode: &Mode) -> Result<(), String> {
        // SAFETY: fd 0 is the socket the VMM handed us; we own it from here.
        let socket = unsafe { UnixDatagram::from_raw_fd(0) };
        let mut ty: libc::c_int = 0;
        let mut len = size_of::<libc::c_int>() as libc::socklen_t;
        // SAFETY: valid out-pointers for SO_TYPE.
        let rc = unsafe {
            libc::getsockopt(
                0,
                libc::SOL_SOCKET,
                libc::SO_TYPE,
                (&raw mut ty).cast(),
                &raw mut len,
            )
        };
        if rc != 0 || ty != libc::SOCK_DGRAM {
            return Err("stdin must be a SOCK_DGRAM unix socket".into());
        }
        // SAFETY (this block): plain xpc/dispatch/vmnet calls on objects we
        // create; the C strings and `Start` outlive every use.
        unsafe {
            let desc = xpc_dictionary_create(std::ptr::null(), std::ptr::null(), 0);
            let iface_name;
            match mode {
                Mode::Shared => {
                    xpc_dictionary_set_uint64(desc, vmnet_operation_mode_key, VMNET_SHARED_MODE);
                }
                Mode::Bridged(name) => {
                    xpc_dictionary_set_uint64(desc, vmnet_operation_mode_key, VMNET_BRIDGED_MODE);
                    iface_name = cstring(name);
                    xpc_dictionary_set_string(
                        desc,
                        vmnet_shared_interface_name_key,
                        iface_name.as_ptr(),
                    );
                }
            }
            xpc_dictionary_set_bool(desc, vmnet_allocate_mac_address_key, false);
            let label = cstring("vmuc.vmnet");
            let queue = dispatch_queue_create(label.as_ptr(), std::ptr::null());
            let sem = dispatch_semaphore_create(0);
            let mut start = Start {
                sem,
                status: 0,
                hello: None,
            };
            let handler = status_params_block(on_start, (&raw mut start).cast());
            let iface = vmnet_start_interface(desc, queue, handler);
            dispatch_semaphore_wait(sem, dispatch_time(0, 30_000_000_000));
            xpc_release(desc);
            let Some(hello) = start.hello.filter(|_| !iface.is_null()) else {
                return Err(format!(
                    "vmnet_start_interface failed with status {}",
                    start.status
                ));
            };
            let line = serde_json::to_vec(&hello).map_err(|e| e.to_string())?;
            socket.send(&line).map_err(|e| format!("hello: {e}"))?;

            let size = usize::try_from(hello.max_packet_size)
                .unwrap_or(1514)
                .max(1514);
            let to_vmm = Box::leak(Box::new(ToVmm {
                iface,
                fd: socket.as_raw_fd(),
                bufs: (0..BATCH).map(|_| vec![0u8; size]).collect(),
            }));
            let cb = event_block(on_packets, std::ptr::from_mut(to_vmm).cast());
            if vmnet_interface_set_event_callback(
                iface,
                VMNET_INTERFACE_PACKETS_AVAILABLE,
                queue,
                cb,
            ) != VMNET_SUCCESS
            {
                return Err("vmnet_interface_set_event_callback failed".into());
            }

            // Frames from the VMM to vmnet, on this thread, until it closes.
            let mut frame = vec![0u8; size];
            loop {
                let n = match socket.recv(&mut frame) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => n,
                };
                let mut iov = libc::iovec {
                    iov_base: frame.as_mut_ptr().cast(),
                    iov_len: n,
                };
                let mut pkt = vmpktdesc {
                    vm_pkt_size: n,
                    vm_pkt_iov: &raw mut iov,
                    vm_pkt_iovcnt: 1,
                    vm_flags: 0,
                };
                let mut count = 1;
                vmnet_write(iface, &raw mut pkt, &raw mut count);
            }
            let stopped = dispatch_semaphore_create(0);
            let done = status_block(on_stop, stopped);
            if vmnet_stop_interface(iface, queue, done) == VMNET_SUCCESS {
                dispatch_semaphore_wait(stopped, dispatch_time(0, 5_000_000_000));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn accepts_exactly_the_two_forms() {
        assert_eq!(
            Mode::from_args(&args(&["--mode", "shared"])),
            Ok(Mode::Shared)
        );
        assert_eq!(
            Mode::from_args(&args(&["--mode", "bridged", "--interface", "en0"])),
            Ok(Mode::Bridged("en0".into()))
        );
        for bad in [
            &["--mode", "host"][..],
            &["--mode", "shared", "--extra"],
            &["--mode", "bridged", "--interface", "en0;rm"],
            &["--mode", "bridged", "--interface", ""],
            &[],
        ] {
            assert!(Mode::from_args(&args(bad)).is_err(), "{bad:?}");
        }
        assert_eq!(
            Mode::Bridged("en1".into()).to_args(),
            args(&["--mode", "bridged", "--interface", "en1"])
        );
    }

    #[test]
    fn hello_round_trips() {
        let h = Hello {
            mtu: 1500,
            max_packet_size: 1514,
            start: Some("192.168.64.1".into()),
            end: Some("192.168.64.254".into()),
            mask: Some("255.255.255.0".into()),
        };
        let s = serde_json::to_string(&h).unwrap();
        assert_eq!(serde_json::from_str::<Hello>(&s).unwrap(), h);
    }
}
