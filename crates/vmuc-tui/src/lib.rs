// SPDX-License-Identifier: Apache-2.0
//! ratatui front end for watching and driving instances (`vmuc tui`).
//!
//! A client like the CLI: everything it shows comes from a [`Source`],
//! which the `vmuc` binary implements over instance directories and
//! control sockets. It refreshes once a second and otherwise sleeps on
//! the keyboard, so it costs nothing while you look at it.

pub mod app;
pub mod history;
pub mod source;
pub mod text;
pub mod ui;

use std::io;
use std::time::{Duration, Instant};

use ratatui::DefaultTerminal;
use ratatui::backend::{Backend, ClearType};
use ratatui::crossterm::event::{self, Event, KeyEventKind};

pub use app::{App, Command};
pub use source::{Action, Instance, Live, Source};

/// How often instance data is refreshed.
pub const REFRESH: Duration = Duration::from_secs(1);
/// Console lines fetched for the tail pane.
const CONSOLE_LINES: usize = 200;

/// Runs the TUI until the user quits. Takes over the terminal (raw mode,
/// alternate screen) and gives it back on return, on panic, and for the
/// length of an interactive shell.
pub fn run(source: &mut dyn Source) -> io::Result<()> {
    let mut terminal = ratatui::init();
    let result = event_loop(&mut terminal, source);
    ratatui::restore();
    result
}

fn event_loop(terminal: &mut DefaultTerminal, source: &mut dyn Source) -> io::Result<()> {
    let epoch = Instant::now();
    let mut app = App::new();
    let mut next = Instant::now();
    let mut console = Vec::new();
    loop {
        if Instant::now() >= next {
            app.refresh(source.instances(), epoch.elapsed());
            if let Some(n) = source.notices().pop() {
                app.status_line = n;
            }
            console = match (app.show_console, app.selected_name()) {
                (true, Some(name)) => source.console_tail(name, CONSOLE_LINES),
                _ => Vec::new(),
            };
            next = Instant::now() + REFRESH;
        }
        terminal.draw(|f| ui::draw(f, &app, &console))?;
        if !event::poll(next.saturating_duration_since(Instant::now()))? {
            continue;
        }
        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        match app.key(key) {
            Command::Quit => return Ok(()),
            Command::Act(name, action) if action.needs_terminal() => {
                ratatui::restore();
                app.status_line = source.act(&name, action);
                *terminal = ratatui::init();
                // Wipe what the shell left. Not Terminal::clear, which asks
                // the terminal for the cursor position and fails where
                // nothing answers.
                terminal.backend_mut().clear_region(ClearType::All)?;
                next = Instant::now();
            }
            Command::Act(name, action) => {
                app.status_line = source.act(&name, action);
                next = Instant::now();
            }
            Command::None => {
                // Selection or pane changes: show the new console now.
                next = next.min(Instant::now() + Duration::from_millis(50));
            }
        }
    }
}
