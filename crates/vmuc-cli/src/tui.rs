// SPDX-License-Identifier: Apache-2.0
//! `vmuc tui`: the terminal UI over this machine's instances. Data comes
//! from instance directories and control sockets, as for `vmuc list` and
//! `vmuc stat`; start and stop run this binary in the background and
//! report back, a shell runs in the foreground.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;

use vmuc_config::VmucHome;
use vmuc_config::paths::instance as files;
use vmuc_proto::{ControlRequest, ControlResponse, Stat};
use vmuc_tui::{Action, Instance, Live, Source};

use crate::{control, instance};

/// A VMM slower than this to answer is shown without live data.
const ANSWER_WITHIN: Duration = Duration::from_millis(500);
/// How much of the console log the tail pane reads.
const TAIL_BYTES: u64 = 64 << 10;

pub fn run(home: &VmucHome) -> anyhow::Result<()> {
    let (tx, rx) = mpsc::channel();
    let mut source = Local {
        home: home.clone(),
        exe: std::env::current_exe()?,
        tx,
        rx,
    };
    vmuc_tui::run(&mut source)?;
    Ok(())
}

struct Local {
    home: VmucHome,
    exe: PathBuf,
    tx: Sender<String>,
    rx: Receiver<String>,
}

impl Local {
    fn dir(&self, name: &str) -> anyhow::Result<PathBuf> {
        Ok(self.home.instance_dir(name)?)
    }

    /// Runs `vmuc ARGS…` on a thread; its last line of output becomes a
    /// notice when it finishes.
    fn background(&self, args: &[&str]) {
        let mut cmd = Command::new(&self.exe);
        cmd.args(args).stdin(Stdio::null());
        let tx = self.tx.clone();
        let what = args.join(" ");
        std::thread::spawn(move || {
            let line = match cmd.output() {
                Ok(out) => {
                    let text = String::from_utf8_lossy(&out.stderr).into_owned()
                        + &String::from_utf8_lossy(&out.stdout);
                    let last = text.lines().rfind(|l| !l.trim().is_empty()).unwrap_or("");
                    let last = last.trim().to_owned();
                    if out.status.success() {
                        last
                    } else {
                        format!("vmuc {what}: {last}")
                    }
                }
                Err(e) => format!("vmuc {what}: {e}"),
            };
            let _ = tx.send(line);
        });
    }
}

impl Source for Local {
    fn instances(&mut self) -> Vec<Instance> {
        let rows = match instance::list(&self.home) {
            Ok(rows) => rows,
            Err(e) => {
                let _ = self.tx.send(format!("listing instances: {e}"));
                return Vec::new();
            }
        };
        rows.into_iter()
            .map(|r| Instance {
                live: (!matches!(r.status, "Stopped" | "Suspended"))
                    .then(|| live(&r.dir))
                    .flatten(),
                name: r.name,
                status: r.status.to_owned(),
                cpus: r.cpus,
                memory: r.memory,
                disk: r.disk,
                addrs: r.addrs,
            })
            .collect()
    }

    fn console_tail(&mut self, name: &str, lines: usize) -> Vec<String> {
        self.dir(name)
            .ok()
            .and_then(|d| tail(&d.join(files::CONSOLE_LOG), lines).ok())
            .unwrap_or_default()
    }

    fn act(&mut self, name: &str, action: Action) -> String {
        match action {
            Action::Start => {
                self.background(&["start", name]);
                format!("starting {name}…")
            }
            Action::Resume => {
                self.background(&["resume", name]);
                format!("resuming {name}…")
            }
            Action::Suspend => {
                self.background(&["suspend", name]);
                format!("suspending {name}…")
            }
            Action::Duplicate(copy) => {
                self.background(&["duplicate", name, &copy]);
                format!("duplicating {name} as {copy}…")
            }
            Action::Stop => {
                self.background(&["stop", name]);
                format!("stopping {name}…")
            }
            Action::ForceStop => {
                self.background(&["stop", "--force", name]);
                format!("forcing {name} off…")
            }
            Action::Reclaim => {
                let asked = self.dir(name).and_then(|d| {
                    control::request_within(&d, &ControlRequest::ReclaimNow, ANSWER_WITHIN)
                });
                match asked {
                    Ok(ControlResponse::Ok) => format!("{name}: the guest is dropping caches"),
                    Ok(ControlResponse::Error { message }) => format!("{name}: {message}"),
                    Ok(other) => format!("{name}: unexpected reply {other:?}"),
                    Err(e) => format!("{name}: {e}"),
                }
            }
            Action::Shell => match Command::new(&self.exe).args(["shell", name]).status() {
                Ok(s) if s.success() => format!("left the shell on {name}"),
                Ok(s) => format!("vmuc shell {name}: {s}"),
                Err(e) => format!("vmuc shell {name}: {e}"),
            },
        }
    }

    fn notices(&mut self) -> Vec<String> {
        self.rx.try_iter().collect()
    }
}

/// Status, memory accounting and forwards from a running VMM.
fn live(dir: &Path) -> Option<Live> {
    let ask = |req| control::request_within(dir, &req, ANSWER_WITHIN).ok();
    let Some(ControlResponse::Status(status)) = ask(ControlRequest::Status) else {
        return None;
    };
    let stat = match ask(ControlRequest::Stat) {
        Some(ControlResponse::Stat(s)) => s,
        _ => Stat::default(),
    };
    let forwards = match ask(ControlRequest::Forwards) {
        Some(ControlResponse::Forwards(f)) => f,
        _ => Vec::new(),
    };
    Some(Live {
        status,
        stat,
        forwards,
    })
}

/// The last `lines` lines of a file, reading at most its last 64 KiB.
fn tail(path: &Path, lines: usize) -> std::io::Result<Vec<String>> {
    let mut f = File::open(path)?;
    let len = f.metadata()?.len();
    f.seek(SeekFrom::Start(len.saturating_sub(TAIL_BYTES)))?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf)?;
    let text = String::from_utf8_lossy(&buf);
    let mut all: Vec<&str> = text.lines().collect();
    if len > TAIL_BYTES && !all.is_empty() {
        all.remove(0); // probably cut mid-line
    }
    Ok(all[all.len().saturating_sub(lines)..]
        .iter()
        .map(|l| (*l).to_owned())
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tail_returns_the_last_whole_lines() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("console.log");
        std::fs::write(&p, "one\ntwo\nthree\n").unwrap();
        assert_eq!(tail(&p, 2).unwrap(), vec!["two", "three"]);
        assert_eq!(tail(&p, 10).unwrap().len(), 3);
        let long = (0..20_000).fold(String::new(), |mut s, i| {
            use std::fmt::Write;
            let _ = writeln!(s, "line {i}");
            s
        });
        std::fs::write(&p, long).unwrap();
        let t = tail(&p, 5000).unwrap();
        assert!(
            t[0].starts_with("line "),
            "no partial first line: {:?}",
            t[0]
        );
        assert_eq!(t.last().unwrap(), "line 19999");
        assert!(tail(&dir.path().join("missing"), 3).is_err());
    }
}
