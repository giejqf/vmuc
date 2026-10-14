// SPDX-License-Identifier: Apache-2.0
//! `vmuc stat`: what the host pays for an instance's memory, and how much
//! of it free page reporting has handed back.

use std::time::Duration;

use anyhow::bail;
use vmuc_config::VmucHome;
use vmuc_proto::{ControlRequest, ControlResponse, Stat};

use crate::control;
use crate::instance::Instance;

/// `15.0 GiB`, `812 MiB`, `0 B`: whole units below 10 of the next one.
pub fn human(bytes: u64) -> String {
    const KIB: u64 = 1 << 10;
    const MIB: u64 = 1 << 20;
    const GIB: u64 = 1 << 30;
    if bytes >= 10 * GIB {
        format!("{}.{} GiB", bytes / GIB, (bytes % GIB) * 10 / GIB)
    } else if bytes >= 10 * MIB {
        format!("{} MiB", bytes / MIB)
    } else if bytes >= 10 * KIB {
        format!("{} KiB", bytes / KIB)
    } else {
        format!("{bytes} B")
    }
}

fn fetch(inst: &Instance) -> anyhow::Result<Stat> {
    match control::request(&inst.dir, &ControlRequest::Stat)? {
        ControlResponse::Stat(s) => Ok(s),
        ControlResponse::Error { message } => bail!("{message}"),
        other => bail!("unexpected reply: {other:?}"),
    }
}

fn render(name: &str, s: &Stat) -> String {
    let guest = if s.guest_total == 0 {
        "(waiting for the guest's first stats)".to_owned()
    } else {
        format!(
            "{} total, {} available",
            human(s.guest_total),
            human(s.guest_available)
        )
    };
    format!(
        "{name}\n  host footprint   {}\n  guest memory     {guest}\n  reported free    {}\n  \
         released         {} (p50 {} µs, p99 {} µs per range)\n  \
         host CPU time    {:.1} s\n  network          {} in, {} out{}\n",
        human(s.host_footprint),
        human(s.reported),
        human(s.released),
        s.release_p50_us,
        s.release_p99_us,
        Duration::from_nanos(s.host_cpu_nanos).as_secs_f64(),
        human(s.net_rx_bytes),
        human(s.net_tx_bytes),
        match (s.net_dropped, s.net_rings) {
            (0, false) => String::new(),
            (0, true) => " (shared rings)".to_owned(),
            (n, rings) => format!(
                ", {n} frames dropped{}",
                if rings { " (shared rings)" } else { "" }
            ),
        },
    )
}

pub fn run(home: &VmucHome, name: &str, watch: bool, json: bool) -> anyhow::Result<()> {
    let inst = Instance::open(home, name)?;
    loop {
        let s = fetch(&inst)?;
        if json {
            println!("{}", serde_json::to_string(&s)?);
        } else {
            if watch {
                print!("\x1b[H\x1b[2J");
            }
            print!("{}", render(name, &s));
        }
        if !watch {
            return Ok(());
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn human_sizes() {
        assert_eq!(human(0), "0 B");
        assert_eq!(human(5000), "5000 B");
        assert_eq!(human(812 << 20), "812 MiB");
        assert_eq!(human(3 << 29), "1536 MiB");
        assert_eq!(human(15 << 30), "15.0 GiB");
    }
}
