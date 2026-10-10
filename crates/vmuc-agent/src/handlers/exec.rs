// SPDX-License-Identifier: Apache-2.0
//! `Exec`: run a command as the agent's user (root) with captured output.
//! Used by the host for readiness probes and provisioning hooks.

use std::io::{self, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use vmuc_proto::AgentResponse;

pub fn exec(
    argv: &[String],
    stdin: Option<&str>,
    timeout: Option<u32>,
) -> io::Result<AgentResponse> {
    let Some((prog, rest)) = argv.split_first() else {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "empty argv"));
    };
    let mut child = Command::new(prog)
        .args(rest)
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    if let (Some(input), Some(mut pipe)) = (stdin, child.stdin.take()) {
        let input = input.to_owned();
        std::thread::spawn(move || {
            let _ = pipe.write_all(input.as_bytes());
        });
    }

    // Drain both pipes on threads so a chatty command cannot deadlock us.
    let mut out = child.stdout.take().expect("piped stdout");
    let mut err = child.stderr.take().expect("piped stderr");
    let out_t = std::thread::spawn(move || {
        let mut v = Vec::new();
        let _ = io::copy(&mut out, &mut v);
        v
    });
    let err_t = std::thread::spawn(move || {
        let mut v = Vec::new();
        let _ = io::copy(&mut err, &mut v);
        v
    });

    let deadline = timeout.map(|s| Instant::now() + Duration::from_secs(u64::from(s)));
    let status = loop {
        if let Some(st) = child.try_wait()? {
            break st;
        }
        if deadline.is_some_and(|d| Instant::now() >= d) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(io::Error::new(io::ErrorKind::TimedOut, "command timed out"));
        }
        std::thread::sleep(Duration::from_millis(10));
    };

    let stdout = out_t.join().unwrap_or_default();
    let stderr = err_t.join().unwrap_or_default();
    Ok(AgentResponse::Exec {
        status: status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runs_and_captures() {
        let r = exec(
            &[
                "sh".into(),
                "-c".into(),
                "echo hi; echo err >&2; exit 3".into(),
            ],
            None,
            Some(5),
        )
        .unwrap();
        assert_eq!(
            r,
            AgentResponse::Exec {
                status: 3,
                stdout: "hi\n".into(),
                stderr: "err\n".into()
            }
        );
    }

    #[test]
    fn feeds_stdin() {
        let r = exec(&["cat".into()], Some("abc"), Some(5)).unwrap();
        assert!(matches!(r, AgentResponse::Exec { stdout, .. } if stdout == "abc"));
    }

    #[test]
    fn times_out() {
        let e = exec(&["sleep".into(), "5".into()], None, Some(1)).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::TimedOut);
    }
}
