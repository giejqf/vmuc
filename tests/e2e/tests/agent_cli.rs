// SPDX-License-Identifier: Apache-2.0
//! ADR 0010: what scripts and agents rely on. Results as JSON on stdout,
//! failures as JSON on stderr with exit codes by kind, start, stop and
//! suspend idempotent, `exec` passing arguments as given and exiting with
//! the program's status, `edit` without an editor. Needs Apple silicon,
//! the kernel artifact and the musl agent.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Output, Stdio};

use vmuc_e2e::cli::{Cleanup, run, vmuc};

/// `vmuc --json ARGS`: the exit code and stdout as JSON (null when empty).
fn json(bin: &Path, home: &Path, args: &[&str]) -> (i32, serde_json::Value) {
    let mut all = vec!["--json"];
    all.extend_from_slice(args);
    let out = run(bin, home, &all);
    let doc = serde_json::from_slice(&out.stdout).unwrap_or(serde_json::Value::Null);
    (out.status.code().unwrap_or(-1), doc)
}

/// A failure's exit code and the `kind` of the JSON error, the last line
/// of stderr (progress may come before it).
fn failure(bin: &Path, home: &Path, args: &[&str]) -> (i32, String) {
    let mut all = vec!["--json"];
    all.extend_from_slice(args);
    let out = run(bin, home, &all);
    assert!(out.stdout.is_empty(), "no result on a failure");
    let stderr = String::from_utf8_lossy(&out.stderr);
    let last = stderr.lines().last().unwrap_or_default();
    let err: serde_json::Value = serde_json::from_str(last).expect("a JSON error");
    let kind = err["error"]["kind"].as_str().unwrap().to_owned();
    (out.status.code().unwrap_or(-1), kind)
}

fn exec(bin: &Path, home: &Path, args: &[&str], stdin: &[u8]) -> Output {
    let mut child = Command::new(bin)
        .arg("exec")
        .args(args)
        .env("VMUC_HOME", home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(stdin).unwrap();
    child.wait_with_output().unwrap()
}

#[test]
#[ignore = "boots Ubuntu: Apple silicon only"]
fn scripts_and_agents_get_json_codes_and_exact_arguments() {
    let _vm = vmuc_e2e::vm_lock();
    let bin = &vmuc();
    let home = tempfile::Builder::new()
        .prefix("vmuc-e2e")
        .tempdir_in("/tmp")
        .unwrap();
    let h = home.path();
    let _cleanup = Cleanup::new(bin, h, "ag");
    let create = [
        "create", "ubuntu", "--name", "ag", "--cpus", "2", "--memory", "2GiB",
    ];
    let (code, doc) = json(bin, h, &create);
    assert_eq!((code, doc["status"].as_str()), (0, Some("Stopped")));
    assert_eq!(failure(bin, h, &create), (5, "exists".into()));
    assert_eq!(
        failure(bin, h, &["list", "ag", "nope"]),
        (3, "not_found".into())
    );
    assert_eq!(
        failure(bin, h, &["edit", "ag"]),
        (2, "invalid".into()),
        "no tty"
    );

    let (code, doc) = json(bin, h, &["start", "ag", "--timeout", "180"]);
    assert_eq!((code, doc["status"].as_str()), (0, Some("Running")));
    assert!(doc["ready_seconds"].as_f64().unwrap() > 0.0);
    let (code, doc) = json(bin, h, &["start", "ag"]);
    assert_eq!((code, doc["was_running"].as_bool()), (0, Some(true)));
    assert_eq!(
        failure(bin, h, &["delete", "ag"]),
        (4, "wrong_state".into())
    );
    assert_eq!(
        failure(bin, h, &["resume", "ag"]),
        (4, "wrong_state".into())
    );

    let out = exec(
        bin,
        h,
        &["ag", "--", "printf", "[%s]", "a  b", "$HOME", "x;y", ""],
        b"",
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "[a  b][$HOME][x;y][]");
    let out = exec(
        bin,
        h,
        &["ag", "--", "python3", "-c", "import sys; sys.exit(7)"],
        b"",
    );
    assert_eq!(out.status.code(), Some(7), "the program's status");
    let out = exec(bin, h, &["ag", "--", "cat"], b"through stdin");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "through stdin");
    let out = exec(bin, h, &["nope", "--", "true"], b"");
    assert_eq!(
        out.status.code(),
        Some(125),
        "vmuc failed before the program"
    );

    let (code, doc) = json(bin, h, &["edit", "ag", "--set", "memory=3GiB"]);
    assert_eq!((code, doc["changed"].as_bool()), (0, Some(true)));
    let (_, doc) = json(bin, h, &["edit", "ag", "--memory", "3GiB"]);
    assert_eq!(doc["changed"].as_bool(), Some(false));

    let (code, doc) = json(bin, h, &["suspend", "ag"]);
    assert_eq!((code, doc["status"].as_str()), (0, Some("Suspended")));
    assert!(doc["saved_bytes"].as_u64().unwrap() > 0);
    let (code, doc) = json(bin, h, &["suspend", "ag"]);
    assert_eq!((code, doc["saved_bytes"].as_u64()), (0, Some(0)), "already");
    assert_eq!(failure(bin, h, &["stop", "ag"]), (4, "wrong_state".into()));
    // The memory changed while it ran: it cannot resume as it was.
    assert_eq!(
        failure(bin, h, &["resume", "ag"]),
        (4, "wrong_state".into())
    );
    let (code, _) = json(bin, h, &["edit", "ag", "--memory", "2GiB"]);
    assert_eq!(code, 0);
    let (code, doc) = json(bin, h, &["resume", "ag"]);
    assert_eq!((code, doc["resumed"].as_bool()), (0, Some(true)));

    let (code, doc) = json(bin, h, &["stop", "ag"]);
    assert_eq!((code, doc["status"].as_str()), (0, Some("Stopped")));
    let (code, _) = json(bin, h, &["stop", "ag"]);
    assert_eq!(code, 0, "stopping a stopped instance");
    assert_eq!(
        failure(bin, h, &["start", "ag", "--timeout", "1"]),
        (6, "timed_out".into())
    );
    // It kept booting; a second start waits for it to be ready.
    let (code, doc) = json(bin, h, &["start", "ag", "--timeout", "180"]);
    assert_eq!(code, 0);
    assert_eq!(doc["was_running"].as_bool(), Some(true));
    assert_eq!(doc["status"].as_str(), Some("Running"));
    let (code, _) = json(bin, h, &["stop", "ag"]);
    assert_eq!(code, 0);
    let (code, doc) = json(bin, h, &["delete", "ag"]);
    assert_eq!((code, doc["deleted"].as_bool()), (0, Some(true)));
}
