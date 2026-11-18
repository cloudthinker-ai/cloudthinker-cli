use cloudthinker_client::doctor::{Area, Check, Fix, Report, Status};
use owo_colors::{AnsiColors, OwoColorize};
use serde::Serialize;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::{emit_json, print_lines, terminal_text};

const WIDTH: usize = 80;
const MIN_FIX: usize = 24;

fn columns(text: &str) -> usize {
    text.width()
}

fn fit(text: &str, room: usize) -> String {
    if columns(text) <= room {
        return text.to_string();
    }
    let mut used = 0;
    let mut kept = String::new();
    for character in text.chars() {
        let size = character.width().unwrap_or(0);
        if used + size + 1 > room {
            break;
        }
        used += size;
        kept.push(character);
    }
    format!("{}…", kept.trim_end())
}

fn fit_row(summary: &str, fix: &str, room: usize) -> (String, String) {
    let summary_width = columns(summary);
    let fix_width = columns(fix);
    if summary_width + fix_width <= room {
        return (summary.to_string(), fix.to_string());
    }
    let fix = fit(
        fix,
        fix_width.min(room.saturating_sub(summary_width).max(MIN_FIX)),
    );
    (fit(summary, room.saturating_sub(columns(&fix))), fix)
}

pub fn emit_doctor<E, W: Serialize>(
    mut report: Report<E>,
    json: bool,
    wire: impl FnOnce(Report<E>) -> W,
) -> Result<(), String> {
    report.map_text(terminal_text);
    if json {
        return emit_json(&wire(report));
    }
    let color = supports_color::on(supports_color::Stream::Stdout).is_some();
    print_lines(&doctor_lines(&report, color))
}

fn doctor_lines<E>(report: &Report<E>, color: bool) -> Vec<String> {
    let paint = Paint(color);
    let mut rows: Vec<&Check<E>> = report
        .checks
        .iter()
        .filter(|check| matches!(check.status, Status::Fail | Status::Unknown | Status::Warn))
        .filter(|check| check.subject.is_some() || !has_items(report, check.area))
        .collect();
    rows.sort_by_key(|check| check.status);
    if rows.is_empty() {
        let line = fit(
            &format!("{} · {} · all checks passed", report.title, report.version),
            WIDTH - 2,
        );
        return vec![format!("{} {line}", paint.mark(Status::Pass))];
    }
    let mut header = format!("{} · {}", report.title, report.version);
    if let Some(server) = &report.server {
        header.push_str(&format!(" · {server}"));
    }
    let mut lines = vec![paint.bold(&fit(&header, WIDTH))];
    let mut healthy = healthy_names(report);
    if let Some(tally) = &report.tally {
        healthy.push(tally.clone());
    }
    if !healthy.is_empty() {
        lines.push(format!(
            "{} {}",
            paint.mark(Status::Pass),
            healthy.join(" · ")
        ));
    }
    let names: Vec<String> = rows
        .iter()
        .map(|check| fit(check.subject.as_deref().unwrap_or(check.label), 24))
        .collect();
    let width = names.iter().map(|name| columns(name)).max().unwrap_or(0);
    for (check, name) in rows.iter().zip(names) {
        let head = format!(
            "{} {name}{}  ",
            paint.mark(check.status),
            " ".repeat(width - columns(&name))
        );
        let fix = check
            .fix
            .as_ref()
            .and_then(Fix::command)
            .map(|command| format!(" · {command}"))
            .unwrap_or_default();
        let (summary, fix) = fit_row(&check.summary, &fix, WIDTH.saturating_sub(width + 4));
        lines.push(format!("{head}{summary}{}", paint.dim(&fix)));
    }
    lines
}

fn has_items<E>(report: &Report<E>, area: Area) -> bool {
    report
        .checks
        .iter()
        .any(|check| check.area == area && check.subject.is_some())
}

fn healthy_names<E>(report: &Report<E>) -> Vec<String> {
    let checks: Vec<&Check<E>> = report
        .checks
        .iter()
        .filter(|check| check.subject.is_none() && check.status != Status::Skipped)
        .collect();
    let mut areas: Vec<Area> = Vec::new();
    for check in &checks {
        if !areas.contains(&check.area) {
            areas.push(check.area);
        }
    }
    if areas.len() < 2 {
        let mut names: Vec<String> = Vec::new();
        for check in &checks {
            let clean = checks
                .iter()
                .filter(|other| other.label == check.label)
                .all(|other| other.status == Status::Pass);
            if clean && !names.iter().any(|name| name == check.label) {
                names.push(check.label.to_string());
            }
        }
        return names;
    }
    areas
        .into_iter()
        .filter(|area| report.tally.is_none() || *area != Area::Connections)
        .filter(|area| {
            checks
                .iter()
                .filter(|check| check.area == *area)
                .all(|check| check.status == Status::Pass)
                && !report.checks.iter().any(|check| {
                    check.area == *area && check.subject.is_some() && check.status != Status::Pass
                })
        })
        .map(|area| area.label().to_string())
        .collect()
}

#[derive(Clone, Copy)]
struct Paint(bool);

impl Paint {
    fn mark(self, status: Status) -> String {
        let (symbol, color) = match status {
            Status::Pass => ("✓", AnsiColors::Green),
            Status::Warn => ("!", AnsiColors::Yellow),
            Status::Fail => ("✗", AnsiColors::Red),
            Status::Unknown => ("?", AnsiColors::Yellow),
            Status::Skipped => ("-", AnsiColors::BrightBlack),
        };
        if self.0 {
            symbol.color(color).bold().to_string()
        } else {
            symbol.to_string()
        }
    }

    fn dim(self, text: &str) -> String {
        if self.0 {
            text.dimmed().to_string()
        } else {
            text.to_string()
        }
    }

    fn bold(self, text: &str) -> String {
        if self.0 {
            text.bold().to_string()
        } else {
            text.to_string()
        }
    }
}
