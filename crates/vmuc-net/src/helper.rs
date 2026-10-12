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
    /// Every datagram starts with a 12-byte `virtio_net_hdr` and TCP
    /// segmentation offload is on (macOS 15.4+). Absent from older
    /// helpers, which send bare frames.
    #[serde(default)]
    pub virtio_header: bool,
    /// The ring layout (`ring::LAYOUT`) the helper can relay through
    /// (ADR 0001); the VMM answers with the region's fd when it matches.
    /// 0, absent: no rings.
    #[serde(default)]
    pub ring_layout: u32,
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
mod relay;
#[cfg(target_os = "macos")]
mod rings;

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "macos")]
    #[test]
    fn the_relay_batches_waits_out_full_sockets_and_notices_the_vmm_go() {
        use std::os::fd::AsRawFd;
        use std::os::unix::net::UnixDatagram;
        use std::time::{Duration, Instant};

        use relay::{Batch, recv_batch, send_frame};

        let (helper, vmm) = UnixDatagram::pair().unwrap();
        helper
            .set_read_timeout(Some(Duration::from_millis(100)))
            .unwrap();
        let fd = helper.as_raw_fd();
        let mut bufs = vec![vec![0u8; 2048]; 4];
        let mut lens = vec![0; 4];
        for f in [&b"one"[..], b"two", b"three"] {
            vmm.send(f).unwrap();
        }
        assert_eq!(recv_batch(fd, &mut bufs, &mut lens), Batch::Frames(3));
        assert_eq!(&bufs[2][..lens[2]], b"three");
        assert_eq!(recv_batch(fd, &mut bufs, &mut lens), Batch::Idle);

        // A full socket: the frame waits until the VMM reads.
        while vmm.send(&[0u8; 1024]).is_ok() {}
        let reader = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(5));
            let mut b = [0u8; 2048];
            while helper.recv(&mut b).is_ok_and(|n| n != 3) {}
            helper
        });
        let t = Instant::now();
        send_frame(vmm.as_raw_fd(), b"end");
        let helper = reader.join().unwrap();
        assert!(t.elapsed() < Duration::from_secs(1));

        // The VMM goes while the vmnet thread is sending: the send takes
        // the ECONNRESET, and the waiting side must still notice.
        drop(vmm);
        let fd = helper.as_raw_fd();
        send_frame(fd, b"late");
        assert_eq!(recv_batch(fd, &mut bufs, &mut lens), Batch::Closed);
    }

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
            virtio_header: true,
            ring_layout: 2,
        };
        let s = serde_json::to_string(&h).unwrap();
        assert_eq!(serde_json::from_str::<Hello>(&s).unwrap(), h);
        // An older helper's hello has no virtio_header: bare frames.
        let old = r#"{"mtu":1500,"max_packet_size":1514,"start":null,"end":null,"mask":null}"#;
        let old = serde_json::from_str::<Hello>(old).unwrap();
        assert!(!old.virtio_header && old.ring_layout == 0);
    }
}
