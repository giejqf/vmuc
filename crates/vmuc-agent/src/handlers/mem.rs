// SPDX-License-Identifier: Apache-2.0
//! Memory telemetry and the two knobs the host pressure responder uses.

use std::fs;
use std::io;

use vmuc_proto::{AgentResponse, MemInfo, Psi};

pub fn meminfo() -> io::Result<AgentResponse> {
    Ok(AgentResponse::MemInfo(parse_meminfo(&fs::read_to_string(
        "/proc/meminfo",
    )?)))
}

pub fn psi() -> io::Result<AgentResponse> {
    Ok(AgentResponse::Psi(parse_psi(&fs::read_to_string(
        "/proc/pressure/memory",
    )?)))
}

/// `sync` then `echo 3 > drop_caches`. Dropped page cache and inodes are
/// what free page reporting then hands back to the host.
pub fn drop_caches() -> io::Result<AgentResponse> {
    // SAFETY: sync() has no arguments and cannot fail.
    unsafe { libc::sync() };
    fs::write("/proc/sys/vm/drop_caches", "3\n")?;
    Ok(AgentResponse::Ok)
}

/// `echo 1 > compact_memory`: coalesce free pages into runs large enough
/// (pageblock order) for the reporting path to notice.
pub fn compact() -> io::Result<AgentResponse> {
    fs::write("/proc/sys/vm/compact_memory", "1\n")?;
    Ok(AgentResponse::Ok)
}

fn parse_meminfo(text: &str) -> MemInfo {
    let mut m = MemInfo::default();
    for line in text.lines() {
        let Some((key, rest)) = line.split_once(':') else {
            continue;
        };
        let kb: u64 = rest
            .split_whitespace()
            .next()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        let bytes = kb * 1024;
        match key {
            "MemTotal" => m.total = bytes,
            "MemFree" => m.free = bytes,
            "MemAvailable" => m.available = bytes,
            "Buffers" => m.buffers = bytes,
            "Cached" => m.cached = bytes,
            "SwapTotal" => m.swap_total = bytes,
            "SwapFree" => m.swap_free = bytes,
            "Shmem" => m.shmem = bytes,
            "AnonPages" => m.anon_pages = bytes,
            _ => {}
        }
    }
    m
}

fn parse_psi(text: &str) -> Psi {
    let mut p = Psi::default();
    for line in text.lines() {
        let mut it = line.split_whitespace();
        let kind = it.next().unwrap_or("");
        let mut avg10 = 0.0;
        let mut avg60 = 0.0;
        for kv in it {
            if let Some(v) = kv.strip_prefix("avg10=") {
                avg10 = v.parse().unwrap_or(0.0);
            } else if let Some(v) = kv.strip_prefix("avg60=") {
                avg60 = v.parse().unwrap_or(0.0);
            }
        }
        match kind {
            "some" => (p.some_avg10, p.some_avg60) = (avg10, avg60),
            "full" => (p.full_avg10, p.full_avg60) = (avg10, avg60),
            _ => {}
        }
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meminfo_parses_kb_to_bytes() {
        let m = parse_meminfo(
            "MemTotal:        8000000 kB\nMemFree:   100 kB\nMemAvailable:  200 kB\nShmem: 7 kB\nBogus line\n",
        );
        assert_eq!(m.total, 8_000_000 * 1024);
        assert_eq!(m.free, 100 * 1024);
        assert_eq!(m.available, 200 * 1024);
        assert_eq!(m.shmem, 7 * 1024);
        assert_eq!(m.cached, 0);
    }

    #[test]
    fn psi_parses_averages() {
        let p = parse_psi(
            "some avg10=1.50 avg60=0.75 avg300=0.10 total=12345\nfull avg10=0.20 avg60=0.05 avg300=0.00 total=99\n",
        );
        assert!((p.some_avg10 - 1.5).abs() < f32::EPSILON);
        assert!((p.some_avg60 - 0.75).abs() < f32::EPSILON);
        assert!((p.full_avg10 - 0.2).abs() < f32::EPSILON);
    }

    #[test]
    fn live_meminfo_reads() {
        let AgentResponse::MemInfo(m) = meminfo().unwrap() else {
            panic!("expected meminfo");
        };
        assert!(m.total > 0);
    }
}
