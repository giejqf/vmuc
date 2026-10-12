// SPDX-License-Identifier: Apache-2.0
//! Drawing: the instance table on top, the selected instance's details
//! and its console tail below, the status line at the bottom.

use std::time::Duration;

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Cell, LineGauge, Paragraph, Row, Sparkline, Table, TableState};

use crate::app::App;
use crate::source::Instance;
use crate::text::{clean, mib, newest, uptime};

const ACCENT: Color = Color::Cyan;

pub fn draw(f: &mut Frame, app: &App, console: &[String]) {
    let rows = u16::try_from(app.instances.len()).unwrap_or(u16::MAX);
    let [list, below, status] = Layout::vertical([
        Constraint::Length(rows.clamp(1, 12) + 3),
        Constraint::Min(6),
        Constraint::Length(1),
    ])
    .areas(f.area());
    draw_list(f, list, app);
    if app.show_console {
        let [detail, tail] =
            Layout::horizontal([Constraint::Percentage(45), Constraint::Percentage(55)])
                .areas(below);
        draw_detail(f, detail, app);
        draw_console(f, tail, app, console);
    } else {
        draw_detail(f, below, app);
    }
    f.render_widget(Paragraph::new(app.status_line.as_str()).dim(), status);
}

fn draw_list(f: &mut Frame, area: Rect, app: &App) {
    let header = Row::new([
        "NAME",
        "STATUS",
        "CPU",
        "VCPUS",
        "MEMORY",
        "FOOTPRINT",
        "GUEST USED",
        "ADDRESS",
    ])
    .style(Style::new().add_modifier(Modifier::BOLD));
    let rows = app.instances.iter().map(|i| {
        let h = app.histories.get(&i.name);
        let stat = i.live.as_ref().map(|l| &l.stat);
        Row::new([
            Cell::from(i.name.clone()),
            Cell::from(i.status.clone()).style(status_style(&i.status)),
            Cell::from(
                h.and_then(crate::history::History::cpu_percent)
                    .map_or("-".into(), |p| format!("{p:.1}%")),
            ),
            Cell::from(i.cpus.to_string()),
            Cell::from(i.memory.clone()),
            Cell::from(stat.map_or("-".into(), |s| mib(s.host_footprint))),
            Cell::from(stat.filter(|s| s.guest_total > 0).map_or("-".into(), |s| {
                mib(s.guest_total.saturating_sub(s.guest_available))
            })),
            Cell::from(i.addrs.first().map_or("-".into(), ToString::to_string)),
        ])
    });
    let name_width = app
        .instances
        .iter()
        .map(|i| i.name.chars().count())
        .max()
        .unwrap_or(0);
    let widths = [
        Constraint::Length(u16::try_from(name_width.max(4) + 2).unwrap_or(u16::MAX)),
        Constraint::Length(9),
        Constraint::Length(7),
        Constraint::Length(6),
        Constraint::Length(8),
        Constraint::Length(10),
        Constraint::Length(11),
        Constraint::Min(15),
    ];
    let table = Table::new(rows, widths)
        .header(header)
        .row_highlight_style(Style::new().bg(Color::DarkGray))
        .block(Block::bordered().title(" vmuc ".fg(ACCENT).bold()));
    let mut state = TableState::default().with_selected(Some(app.selected));
    f.render_stateful_widget(table, area, &mut state);
}

fn status_style(status: &str) -> Style {
    match status {
        "Running" => Style::new().fg(Color::Green),
        "Stopped" => Style::new().fg(Color::DarkGray),
        "Degraded" => Style::new().fg(Color::Red),
        _ => Style::new().fg(Color::Yellow),
    }
}

fn draw_detail(f: &mut Frame, area: Rect, app: &App) {
    let Some(inst) = app.selected_instance() else {
        let empty = Paragraph::new("No instances. `vmuc create ubuntu` makes one.")
            .block(Block::bordered());
        f.render_widget(empty, area);
        return;
    };
    let block = Block::bordered().title(format!(" {} ", inst.name).fg(ACCENT).bold());
    let inner = block.inner(area);
    f.render_widget(block, area);
    let (Some(live), Some(h)) = (&inst.live, app.histories.get(&inst.name)) else {
        f.render_widget(Paragraph::new(idle_lines(inst)), inner);
        return;
    };
    let s = &live.stat;
    let [info, foot_label, foot, gauge, rel_label, rel, rest] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(2),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(2),
        Constraint::Min(0),
    ])
    .areas(inner);
    let st = &live.status;
    let boot = st.boot_id.get(..8).unwrap_or(&st.boot_id);
    let info_line = format!(
        "up {} · pid {} · boot {boot}",
        uptime(st.uptime_secs),
        st.pid
    );
    f.render_widget(Paragraph::new(info_line), info);
    f.render_widget(
        Paragraph::new(format!("host footprint  {}", mib(s.host_footprint))),
        foot_label,
    );
    f.render_widget(
        Sparkline::default()
            .data(newest(h.footprint_mib(), foot.width))
            .max(st.memory >> 20)
            .fg(ACCENT),
        foot,
    );
    let ratio = if s.guest_total == 0 {
        0.0
    } else {
        let used = s.guest_total.saturating_sub(s.guest_available);
        Duration::from_nanos(used).as_secs_f64() / Duration::from_nanos(s.guest_total).as_secs_f64()
    };
    let label = if s.guest_total == 0 {
        "guest: waiting for stats".to_owned()
    } else {
        format!(
            "guest {} used of {}",
            mib(s.guest_total.saturating_sub(s.guest_available)),
            mib(s.guest_total)
        )
    };
    f.render_widget(
        LineGauge::default()
            .ratio(ratio.clamp(0.0, 1.0))
            .label(label)
            .filled_style(Style::new().fg(Color::Blue)),
        gauge,
    );
    let rel_text = format!(
        "released {} · p50 {} µs · p99 {} µs",
        mib(s.released),
        s.release_p50_us,
        s.release_p99_us
    );
    f.render_widget(Paragraph::new(rel_text), rel_label);
    f.render_widget(
        Sparkline::default()
            .data(newest(h.released_kib_per_s(), rel.width))
            .fg(Color::Green),
        rel,
    );
    f.render_widget(Paragraph::new(live_lines(live)), rest);
}

/// Forwards and errors of a running instance.
fn live_lines(live: &crate::source::Live) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for fw in &live.forwards {
        lines.push(Line::from(
            format!("forward {:?} {} → {}", fw.proto, fw.host, fw.guest).to_lowercase(),
        ));
    }
    for e in &live.status.errors {
        lines.push(Line::from(Span::styled(
            e.clone(),
            Style::new().fg(Color::Red),
        )));
    }
    lines
}

fn idle_lines(inst: &Instance) -> Vec<Line<'static>> {
    vec![
        Line::from(inst.status.clone()).style(status_style(&inst.status)),
        Line::from(format!(
            "{} vCPUs · {} memory · {} disk",
            inst.cpus, inst.memory, inst.disk
        )),
        Line::from(""),
        Line::from("s to start".dim()),
    ]
}

fn draw_console(f: &mut Frame, area: Rect, app: &App, console: &[String]) {
    let title = app
        .selected_name()
        .map_or(" console ".to_owned(), |n| format!(" {n} console "));
    let block = Block::bordered().title(title);
    let height = usize::from(block.inner(area).height);
    let start = console.len().saturating_sub(height);
    let lines: Vec<Line> = console[start..]
        .iter()
        .map(|l| Line::from(clean(l)))
        .collect();
    f.render_widget(Paragraph::new(lines).block(block), area);
}

#[cfg(test)]
mod tests;
