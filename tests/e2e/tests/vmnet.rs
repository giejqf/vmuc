// SPDX-License-Identifier: Apache-2.0
//! Guest networking through vmnet's shared mode, macOS doing DHCP, DNS
//! and NAT: the guest gets an address `vmuc list` shows, the host reaches
//! it by that address, it reaches a service on the host through its
//! gateway, its DNS server is that gateway, and it reaches the internet
//! when the host does; the root helper ends with the VM. Needs Apple
//! silicon and the vmnet helper with its sudoers rule (`vmuc sudoers`);
//! without them the test says so and passes.

use std::io::{BufRead, BufReader};
use std::net::{TcpListener, TcpStream, ToSocketAddrs};
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use vmuc_e2e::cli::{Cleanup, run, vmuc};

/// Whether `vmuc info` finds the helper and its sudoers rule.
fn helper_installed(bin: &Path, home: &Path) -> bool {
    let out = run(bin, home, &["info", "--json"]);
    let info: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    info["install"]["sudoersRule"] == true && info["install"]["vmnetHelper"]["problem"].is_null()
}

fn helpers_running() -> usize {
    let out = Command::new("pgrep")
        .args(["-x", "vmuc-vmnet-helper"])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).lines().count()
}

/// Whether this Mac reaches the internet (else the guest cannot either).
fn host_online() -> bool {
    let Ok(mut addrs) = ("example.com", 443).to_socket_addrs() else {
        return false;
    };
    addrs.any(|a| a.is_ipv4() && TcpStream::connect_timeout(&a, Duration::from_secs(3)).is_ok())
}

fn shell(bin: &Path, home: &Path, script: &str) -> String {
    let out = run(bin, home, &["shell", "natt", "--", script]);
    assert!(out.status.success(), "shell failed: {script}");
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

#[test]
#[ignore = "boots Ubuntu: Apple silicon only"]
fn the_guest_is_on_a_nat_network_with_the_host() {
    let _vm = vmuc_e2e::vm_lock();
    let bin = &vmuc();
    let home = tempfile::Builder::new()
        .prefix("vmuc-e2e")
        .tempdir_in("/tmp")
        .unwrap();
    let h = home.path();
    let _cleanup = Cleanup::new(bin, h, "natt");
    if !helper_installed(bin, h) {
        eprintln!("skipping: the vmnet helper is not installed (`vmuc sudoers`)");
        return;
    }
    let helpers = helpers_running();
    let create = [
        "create", "ubuntu", "--name", "natt", "--cpus", "2", "--memory", "2GiB",
    ];
    assert!(run(bin, h, &create).status.success(), "create failed");
    let started = run(bin, h, &["start", "natt", "--timeout", "180"]);
    assert!(started.status.success());
    assert!(
        !String::from_utf8_lossy(&started.stderr).contains("networking unavailable"),
        "no NIC"
    );
    assert_eq!(helpers_running(), helpers + 1, "one helper for the VM");

    let guest = shell(
        bin,
        h,
        "for i in $(seq 120); do test -e /run/vmuc-boot-done && break; sleep 1; done; \
         ip -4 -o addr show eth0 | awk '{print $4}' | cut -d/ -f1",
    );
    let gateway = shell(bin, h, "ip -4 route show default | awk '{print $3}'");
    assert!(
        !guest.is_empty() && !gateway.is_empty(),
        "{guest:?} via {gateway:?}"
    );
    let list = String::from_utf8_lossy(&run(bin, h, &["list"]).stdout).into_owned();
    assert!(list.contains(&guest), "vmuc list shows {guest}: {list}");

    // The host reaches the guest by its address.
    let sshd = format!("{guest}:22").parse().unwrap();
    TcpStream::connect_timeout(&sshd, Duration::from_secs(5)).expect("host to guest");

    // The guest reaches the host through its gateway.
    let listener = TcpListener::bind("0.0.0.0:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let accept = std::thread::spawn(move || {
        let (s, peer) = listener.accept().unwrap();
        let mut line = String::new();
        BufReader::new(s).read_line(&mut line).unwrap();
        (peer.ip().to_string(), line)
    });
    shell(
        bin,
        h,
        &format!("echo from-guest > /dev/tcp/{gateway}/{port}"),
    );
    let (peer, line) = accept.join().unwrap();
    assert_eq!(line.trim(), "from-guest");
    assert_eq!(peer, guest, "the host sees the guest's own address");

    // DNS is the host's resolver, at the gateway.
    let dns = shell(bin, h, "resolvectl dns eth0");
    assert!(dns.contains(&gateway), "{dns}");
    if host_online() {
        let out = shell(
            bin,
            h,
            "getent ahostsv4 example.com | head -1; \
             curl -4 -s -o /dev/null -m 10 -w '%{http_code}' https://example.com",
        );
        assert!(
            out.ends_with("200"),
            "the guest reaches the internet: {out}"
        );
    } else {
        eprintln!("this Mac is offline: not checking the guest's internet access");
    }

    assert!(run(bin, h, &["stop", "natt"]).status.success());
    for _ in 0..50 {
        if helpers_running() == helpers {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert_eq!(helpers_running(), helpers, "the helper ends with its VM");
    assert!(run(bin, h, &["delete", "natt"]).status.success());
}
