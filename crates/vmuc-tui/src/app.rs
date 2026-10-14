// SPDX-License-Identifier: Apache-2.0
//! Screen state and key handling, free of the terminal so tests can drive
//! it: the instance list, the selection (kept by name across refreshes),
//! each instance's sample history and the status line.

use std::collections::HashMap;
use std::time::Duration;

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::history::History;
use crate::source::{Action, Instance};

/// What a key press asks the run loop to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    None,
    Quit,
    Act(String, Action),
}

/// The name being typed for a duplicate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prompt {
    pub source: String,
    pub name: String,
}

#[derive(Debug, Default)]
pub struct App {
    pub instances: Vec<Instance>,
    pub selected: usize,
    pub histories: HashMap<String, History>,
    pub status_line: String,
    pub show_console: bool,
    /// While set, keys edit the duplicate's name.
    pub prompt: Option<Prompt>,
}

impl App {
    pub fn new() -> Self {
        Self {
            show_console: true,
            status_line: "↑↓ select · s start/resume · z suspend · x stop · X force stop · \
                          d duplicate · ⏎ shell · r reclaim · c console · q quit"
                .into(),
            ..Self::default()
        }
    }

    /// Takes a fresh listing sampled at `at`, keeping the selection on the
    /// same instance and history only for instances that still exist.
    pub fn refresh(&mut self, instances: Vec<Instance>, at: Duration) {
        let keep = self.selected_name().map(str::to_owned);
        for i in &instances {
            if let Some(live) = &i.live {
                self.histories
                    .entry(i.name.clone())
                    .or_default()
                    .push(at, live.stat);
            }
        }
        self.histories
            .retain(|name, _| instances.iter().any(|i| &i.name == name));
        self.instances = instances;
        self.selected = keep
            .and_then(|n| self.instances.iter().position(|i| i.name == n))
            .unwrap_or(self.selected)
            .min(self.instances.len().saturating_sub(1));
    }

    pub fn selected_instance(&self) -> Option<&Instance> {
        self.instances.get(self.selected)
    }

    pub fn selected_name(&self) -> Option<&str> {
        self.selected_instance().map(|i| i.name.as_str())
    }

    pub fn key(&mut self, k: KeyEvent) -> Command {
        if k.code == KeyCode::Char('c') && k.modifiers.contains(KeyModifiers::CONTROL) {
            return Command::Quit;
        }
        if self.prompt.is_some() {
            return self.prompt_key(k);
        }
        let suspended = self
            .selected_instance()
            .is_some_and(|i| i.status == "Suspended");
        let act = |a| {
            self.selected_name()
                .map_or(Command::None, |n| Command::Act(n.to_owned(), a))
        };
        match (k.code, k.modifiers) {
            (KeyCode::Char('q') | KeyCode::Esc, _) => Command::Quit,
            (KeyCode::Char('s'), _) if suspended => act(Action::Resume),
            (KeyCode::Char('s'), _) => act(Action::Start),
            (KeyCode::Char('z'), _) => act(Action::Suspend),
            (KeyCode::Char('d'), _) => {
                if let Some(source) = self.selected_name().map(str::to_owned) {
                    let name = self.free_name(&source);
                    self.prompt = Some(Prompt { source, name });
                }
                Command::None
            }
            (KeyCode::Char('x'), _) => act(Action::Stop),
            (KeyCode::Char('X'), _) => act(Action::ForceStop),
            (KeyCode::Char('r'), _) => act(Action::Reclaim),
            (KeyCode::Enter, _) => act(Action::Shell),
            (KeyCode::Char('c'), _) => {
                self.show_console = !self.show_console;
                Command::None
            }
            (KeyCode::Up | KeyCode::Char('k'), _) => {
                self.selected = self.selected.saturating_sub(1);
                Command::None
            }
            (KeyCode::Down | KeyCode::Char('j'), _) => {
                self.selected = (self.selected + 1).min(self.instances.len().saturating_sub(1));
                Command::None
            }
            _ => Command::None,
        }
    }

    /// Editing the duplicate's name: letters, digits, `-`, `_` and `.`.
    fn prompt_key(&mut self, k: KeyEvent) -> Command {
        let Some(p) = self.prompt.as_mut() else {
            return Command::None;
        };
        match k.code {
            KeyCode::Esc => self.prompt = None,
            KeyCode::Enter if !p.name.is_empty() => {
                let p = self.prompt.take().expect("prompting");
                return Command::Act(p.source, Action::Duplicate(p.name));
            }
            KeyCode::Backspace => {
                p.name.pop();
            }
            KeyCode::Char(c) if c.is_ascii_alphanumeric() || "-_.".contains(c) => p.name.push(c),
            _ => {}
        }
        Command::None
    }

    /// `SOURCE-2`, or the first of `-3`, `-4`, … no instance has; among
    /// n + 1 suffixes one is free.
    fn free_name(&self, source: &str) -> String {
        (2..=self.instances.len() + 2)
            .map(|i| format!("{source}-{i}"))
            .find(|n| !self.instances.iter().any(|i| &i.name == n))
            .expect("n instances take at most n names")
    }
}

#[cfg(test)]
mod tests {
    use vmuc_proto::{InstanceStatus, Phase, Stat};

    use super::*;
    use crate::source::Live;

    fn inst(name: &str, running: bool) -> Instance {
        Instance {
            name: name.into(),
            status: if running { "Running" } else { "Stopped" }.into(),
            live: running.then(|| Live {
                status: InstanceStatus {
                    name: name.into(),
                    phase: Phase::Running,
                    boot_id: "b".into(),
                    pid: 1,
                    cpus: 2,
                    memory: 2 << 30,
                    addrs: vec![],
                    uptime_secs: 5,
                    errors: vec![],
                },
                stat: Stat::default(),
                forwards: vec![],
            }),
            ..Instance::default()
        }
    }

    fn key(c: KeyCode) -> KeyEvent {
        KeyEvent::new(c, KeyModifiers::NONE)
    }

    #[test]
    fn selection_follows_the_instance_across_refreshes() {
        let mut app = App::new();
        app.refresh(vec![inst("a", false), inst("b", true)], Duration::ZERO);
        app.key(key(KeyCode::Down));
        assert_eq!(app.selected_name(), Some("b"));
        app.refresh(
            vec![inst("0", false), inst("a", false), inst("b", true)],
            Duration::from_secs(1),
        );
        assert_eq!(app.selected_name(), Some("b"));
        app.refresh(vec![inst("a", false)], Duration::from_secs(2));
        assert_eq!(app.selected_name(), Some("a"), "clamped when b goes away");
        assert!(!app.histories.contains_key("b"));
        app.refresh(vec![], Duration::from_secs(3));
        assert_eq!(app.selected_instance(), None);
        assert_eq!(
            app.key(key(KeyCode::Char('s'))),
            Command::None,
            "nothing to act on"
        );
    }

    #[test]
    fn s_resumes_a_suspended_instance_and_z_suspends() {
        let mut app = App::new();
        let mut a = inst("a", false);
        a.status = "Suspended".into();
        app.refresh(vec![a, inst("b", true)], Duration::ZERO);
        assert_eq!(
            app.key(key(KeyCode::Char('s'))),
            Command::Act("a".into(), Action::Resume)
        );
        app.key(key(KeyCode::Down));
        assert_eq!(
            app.key(key(KeyCode::Char('s'))),
            Command::Act("b".into(), Action::Start)
        );
        assert_eq!(
            app.key(key(KeyCode::Char('z'))),
            Command::Act("b".into(), Action::Suspend)
        );
    }

    #[test]
    fn d_asks_for_the_duplicate_name() {
        let mut app = App::new();
        app.refresh(vec![inst("a", false), inst("a-2", false)], Duration::ZERO);
        assert_eq!(app.key(key(KeyCode::Char('d'))), Command::None);
        let p = app.prompt.clone().unwrap();
        assert_eq!((p.source.as_str(), p.name.as_str()), ("a", "a-3"));
        app.key(key(KeyCode::Backspace));
        for c in "x y!".chars() {
            app.key(key(KeyCode::Char(c)));
        }
        assert_eq!(
            app.prompt.as_ref().unwrap().name,
            "a-xy",
            "no spaces or `!`"
        );
        assert_eq!(
            app.key(key(KeyCode::Char('q'))),
            Command::None,
            "q is a letter here"
        );
        assert_eq!(
            app.key(key(KeyCode::Enter)),
            Command::Act("a".into(), Action::Duplicate("a-xyq".into()))
        );
        assert_eq!(app.prompt, None);

        app.key(key(KeyCode::Char('d')));
        assert_eq!(app.key(key(KeyCode::Esc)), Command::None, "esc cancels");
        assert_eq!(app.prompt, None);
        app.key(key(KeyCode::Char('d')));
        for _ in 0..5 {
            app.key(key(KeyCode::Backspace));
        }
        assert_eq!(app.key(key(KeyCode::Enter)), Command::None, "no empty name");
        assert!(app.prompt.is_some());
    }

    #[test]
    fn keys_map_to_actions_on_the_selection() {
        let mut app = App::new();
        app.refresh(vec![inst("a", true)], Duration::ZERO);
        assert_eq!(app.histories["a"].latest(), Some(&Stat::default()));
        let on_a = |a| Command::Act("a".into(), a);
        assert_eq!(app.key(key(KeyCode::Char('s'))), on_a(Action::Start));
        assert_eq!(app.key(key(KeyCode::Char('x'))), on_a(Action::Stop));
        assert_eq!(app.key(key(KeyCode::Char('X'))), on_a(Action::ForceStop));
        assert_eq!(app.key(key(KeyCode::Char('r'))), on_a(Action::Reclaim));
        assert_eq!(app.key(key(KeyCode::Enter)), on_a(Action::Shell));
        assert_eq!(app.key(key(KeyCode::Up)), Command::None);
        assert!(app.show_console);
        app.key(key(KeyCode::Char('c')));
        assert!(!app.show_console);
        assert_eq!(app.key(key(KeyCode::Char('q'))), Command::Quit);
        let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(app.key(ctrl_c), Command::Quit);
    }
}
