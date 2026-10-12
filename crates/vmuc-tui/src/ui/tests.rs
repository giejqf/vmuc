// SPDX-License-Identifier: Apache-2.0
//! The screen rendered into a test buffer.

use std::time::Duration;

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use vmuc_proto::{Forward, ForwardState, InstanceStatus, Phase, Proto, Stat};

use super::*;
use crate::source::Live;

fn running(name: &str, cpu_ms: u64) -> Instance {
    Instance {
        name: name.into(),
        status: "Running".into(),
        cpus: 4,
        memory: "4GiB".into(),
        disk: "20GiB".into(),
        addrs: vec!["192.168.64.5".parse().unwrap()],
        live: Some(Live {
            status: InstanceStatus {
                name: name.into(),
                phase: Phase::Running,
                boot_id: "0123456789abcdef".into(),
                pid: 4242,
                cpus: 4,
                memory: 4 << 30,
                addrs: vec![],
                uptime_secs: 75,
                errors: vec!["agent: lost".into()],
            },
            stat: Stat {
                host_footprint: 480 << 20,
                guest_total: 4000 << 20,
                guest_available: 3000 << 20,
                released: 2 << 30,
                host_cpu_nanos: cpu_ms * 1_000_000,
                ..Stat::default()
            },
            forwards: vec![Forward {
                proto: Proto::Tcp,
                host: "127.0.0.1:8000".into(),
                guest: "0.0.0.0:8000".into(),
                state: ForwardState::Forwarding,
            }],
        }),
    }
}

fn stopped(name: &str) -> Instance {
    Instance {
        name: name.into(),
        status: "Stopped".into(),
        cpus: 2,
        memory: "2GiB".into(),
        disk: "10GiB".into(),
        ..Instance::default()
    }
}

fn render(app: &App, console: &[String], w: u16, h: u16) -> String {
    let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
    t.draw(|f| draw(f, app, console)).unwrap();
    let buf = t.backend().buffer();
    (0..h)
        .map(|y| (0..w).map(|x| buf[(x, y)].symbol()).collect::<String>() + "\n")
        .collect()
}

#[test]
fn the_list_and_details_show_live_numbers() {
    let mut app = App::new();
    app.refresh(
        vec![running("dev", 1000), stopped("old")],
        Duration::from_secs(1),
    );
    app.refresh(
        vec![running("dev", 1250), stopped("old")],
        Duration::from_secs(2),
    );
    let console = vec!["\x1b[0;32m  OK  \x1b[0m] Started ssh.service".to_owned()];
    let screen = render(&app, &console, 120, 30);
    for want in [
        "dev",
        "Running",
        "25.0%",
        "480 MiB",
        "1000 MiB",
        "192.168.64.5",
        "old",
        "Stopped",
        "up 1m15s",
        "pid 4242",
        "boot 01234567",
        "guest 1000 MiB used of 4000 MiB",
        "released 2048 MiB",
        "forward tcp 127.0.0.1:8000",
        "agent: lost",
        "OK  ] Started ssh",
        "q quit",
    ] {
        assert!(screen.contains(want), "{want:?} missing from\n{screen}");
    }
}

#[test]
fn a_stopped_selection_says_how_to_start_it() {
    let mut app = App::new();
    app.refresh(vec![stopped("old")], Duration::ZERO);
    let screen = render(&app, &[], 100, 20);
    assert!(screen.contains("s to start"), "{screen}");
    assert!(
        screen.contains("2 vCPUs · 2GiB memory · 10GiB disk"),
        "{screen}"
    );
}

#[test]
fn empty_and_tiny_screens_render() {
    let app = App::new();
    assert!(render(&app, &[], 80, 20).contains("vmuc create ubuntu"));
    let mut app = App::new();
    app.refresh(vec![running("dev", 1)], Duration::ZERO);
    render(&app, &["x".into()], 20, 5);
}
