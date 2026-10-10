// SPDX-License-Identifier: Apache-2.0
//! `TimeSync`: step the guest clock when it has drifted past a threshold.
//! Guests that sleep with the host wake up with a stale CLOCK_REALTIME;
//! the host pings this every few seconds.

use std::io;
use std::time::{SystemTime, UNIX_EPOCH};

use vmuc_proto::AgentResponse;

pub fn time_sync(host_unix_nanos: i64, max_drift_ms: u32) -> io::Result<AgentResponse> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?;
    let guest_nanos = i64::try_from(now.as_nanos()).unwrap_or(i64::MAX);
    let drift_ms = (host_unix_nanos - guest_nanos) / 1_000_000;
    let adjusted = drift_ms.unsigned_abs() > u64::from(max_drift_ms);
    if adjusted {
        let ts = libc::timespec {
            tv_sec: host_unix_nanos / 1_000_000_000,
            tv_nsec: host_unix_nanos % 1_000_000_000,
        };
        // SAFETY: `ts` is a valid timespec; CLOCK_REALTIME is settable by root.
        if unsafe { libc::clock_settime(libc::CLOCK_REALTIME, &raw const ts) } != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(AgentResponse::TimeSync { drift_ms, adjusted })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_drift_is_not_adjusted() {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
        let r = time_sync(i64::try_from(now.as_nanos()).unwrap(), 5_000).unwrap();
        assert!(matches!(
            r,
            AgentResponse::TimeSync {
                adjusted: false,
                ..
            }
        ));
    }
}
