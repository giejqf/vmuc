// SPDX-License-Identifier: Apache-2.0
//! M6: virtio-net moves frames in a real Linux guest. The busybox guest
//! configures 10.0.2.15/24 and pings 10.0.2.2; this test plays 10.0.2.2
//! on the other end of the device's datagram socket, answering ARP and
//! ICMP echo. No vmnet (and no root) involved; see the crate docs.

use std::os::unix::net::UnixDatagram;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::Duration;

use vmuc_e2e::{Capture, artifact, vm_lock, watchdog};
use vmuc_virtio::devices::net::Net;
use vmuc_vmm::{VmBuilder, VmConfig, VmExit};

const PEER_MAC: [u8; 6] = [0x52, 0x54, 0x00, 0x12, 0x34, 0x56];
const PEER_IP: [u8; 4] = [10, 0, 2, 2];
const GUEST_MAC: [u8; 6] = [0x0a, 0x56, 0x4d, 0x00, 0x00, 0x01];

fn checksum(data: &[u8]) -> u16 {
    let mut sum: u32 = data
        .chunks(2)
        .map(|c| u32::from(u16::from_be_bytes([c[0], *c.get(1).unwrap_or(&0)])))
        .sum();
    while sum >> 16 != 0 {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    !(sum as u16)
}

/// Answers ARP for PEER_IP and ICMP echo to it; counts echo replies.
fn reply(f: &[u8]) -> Option<Vec<u8>> {
    if f.len() < 42 {
        return None;
    }
    let mut r = f.to_vec();
    r[0..6].copy_from_slice(&f[6..12]);
    r[6..12].copy_from_slice(&PEER_MAC);
    match [f[12], f[13]] {
        // ARP request for us.
        [0x08, 0x06] if f[20..22] == [0, 1] && f[38..42] == PEER_IP => {
            r[21] = 2;
            r[22..28].copy_from_slice(&PEER_MAC);
            r[28..32].copy_from_slice(&PEER_IP);
            r[32..38].copy_from_slice(&f[22..28]);
            r[38..42].copy_from_slice(&f[28..32]);
            Some(r)
        }
        // ICMP echo request to us.
        [0x08, 0x00] if f[23] == 1 && f[30..34] == PEER_IP && f[34] == 8 => {
            let ihl = usize::from(f[14] & 0xf) * 4;
            r[26..30].copy_from_slice(&f[30..34]);
            r[30..34].copy_from_slice(&f[26..30]);
            let icmp = 14 + ihl;
            r[icmp] = 0;
            r[icmp + 2..icmp + 4].fill(0);
            let c = checksum(&r[icmp..]);
            r[icmp + 2..icmp + 4].copy_from_slice(&c.to_be_bytes());
            Some(r)
        }
        _ => None,
    }
}

#[test]
#[ignore = "boots a VM: Apple silicon only"]
fn guest_pings_a_peer_through_virtio_net() {
    let _vm = vm_lock();
    let (kernel, initrd) = (artifact("Image"), artifact("busybox-initramfs.cpio"));
    let (ours, peer) = UnixDatagram::pair().unwrap();
    peer.set_read_timeout(Some(Duration::from_millis(100)))
        .unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let echoes = Arc::new(AtomicU32::new(0));
    let peer_thread = {
        let (stop, echoes) = (Arc::clone(&stop), Arc::clone(&echoes));
        std::thread::spawn(move || {
            let mut buf = vec![0u8; 2048];
            while !stop.load(Ordering::Relaxed) {
                let Ok(n) = peer.recv(&mut buf) else { continue };
                if let Some(r) = reply(&buf[..n]) {
                    if r[12..14] == [0x08, 0x00] {
                        echoes.fetch_add(1, Ordering::Relaxed);
                    }
                    peer.send(&r).unwrap();
                }
            }
        })
    };

    let cap = Capture::default();
    let (out, writer) = cap.console();
    let mut b = VmBuilder::new(VmConfig {
        cpus: 2,
        ram_size: 512 << 20,
        cmdline: "console=ttyAMA0 panic=-1 quiet vmuc.test=net".into(),
    })
    .unwrap();
    b.add_platform_devices(out.clone()).unwrap();
    b.add_virtio(Box::new(Net::new(GUEST_MAC, ours).unwrap()))
        .unwrap();
    let vm = b.boot(&kernel, Some(&initrd)).unwrap();
    watchdog(vm.machine(), Duration::from_secs(30));
    assert_eq!(vm.wait(), VmExit::PowerOff);
    stop.store(true, Ordering::Relaxed);
    peer_thread.join().unwrap();
    out.close();
    writer.join().unwrap();
    let text = cap.text();
    assert!(text.contains("VMUC_NET_OK"), "console:\n{text}");
    assert_eq!(echoes.load(Ordering::Relaxed), 3, "three echo replies");
}
