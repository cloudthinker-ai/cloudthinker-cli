use cloudthinker_client::{DoctorCheck, DoctorReport, DoctorStatus, OriginSource, display_origin};
use owo_colors::{AnsiColors, OwoColorize};
use serde::Serialize;

use super::worker_console::plural;
use super::{emit_json, print_lines, terminal_text, truncate};
use crate::engine::exit::ExitCode;

const NAME_WIDTH: usize = 32;
const MARK_GUTTER: usize = "  ? ".len();
const LABEL_GAP: &str = "  ";

#[derive(Serialize)]
struct DoctorEnvelope {
    #[serde(flatten)]
    report: DoctorReport,
    exit_code: u8,
}

pub fn emit_doctor(
    mut report: DoctorReport,
    exit_code: ExitCode,
    json: bool,
) -> Result<(), String> {
    report.cli_version = terminal_text(&report.cli_version);
    report.origin = report.origin.as_deref().map(terminal_text);
    for check in &mut report.checks {
        check.summary = terminal_text(&check.summary);
        check.action = check.action.as_deref().map(terminal_text);
        check.connection_name = check.connection_name.as_deref().map(terminal_text);
    }
    if json {
        return emit_json(&DoctorEnvelope {
            report,
            exit_code: exit_code as u8,
        });
    }
    let color = supports_color::on(supports_color::Stream::Stdout).is_some();
    print_lines(&doctor_lines(&report, color))
}

fn doctor_lines(report: &DoctorReport, color: bool) -> Vec<String> {
    let paint = Paint(color);
    let (connections, checks): (Vec<&DoctorCheck>, Vec<&DoctorCheck>) = report
        .checks
        .iter()
        .partition(|check| check.connection_id.is_some());
    let label_width = checks
        .iter()
        .map(|check| label(check.id).chars().count())
        .max()
        .unwrap_or(0);
    let indent = " ".repeat(MARK_GUTTER + label_width + LABEL_GAP.len());
    let mut last_skip: Option<&str> = None;
    let mut lines = Vec::new();
    for check in &checks {
        let http = check
            .http_status
            .map_or_else(String::new, |status| format!(" (HTTP {status})"));
        let repeated_skip =
            check.status == DoctorStatus::Skipped && last_skip == Some(check.summary.as_str());
        last_skip = (check.status == DoctorStatus::Skipped).then_some(check.summary.as_str());
        let value = if check.id == "configuration" && check.status == DoctorStatus::Healthy {
            format!(
                "{} {}",
                report
                    .origin
                    .as_deref()
                    .map_or(check.summary.as_str(), display_origin),
                paint.dim(source_label(report.origin_source))
            )
        } else if repeated_skip {
            paint.dim("Skipped for the same reason")
        } else {
            check.summary.clone()
        };
        lines.push(format!(
            "  {} {}{LABEL_GAP}{value}{}",
            paint.mark(check.status),
            paint.dim_unless(
                check.status != DoctorStatus::Skipped,
                &format!("{:<label_width$}", label(check.id))
            ),
            paint.dim(&http),
        ));
        if let Some(action) = &check.action {
            lines.push(format!("{indent}{} {action}", paint.arrow()));
        }
        if check.id == "connections" {
            lines.extend(connection_lines(&connections, &indent, paint));
        }
    }
    lines.push(String::new());
    lines.push(verdict(report, &checks, paint));
    if report.connections.checked > 0 {
        lines.push(
            paint.dim("Connections show their last recorded test. Doctor starts no new tests."),
        );
    }
    lines
}

fn connection_lines(connections: &[&DoctorCheck], indent: &str, paint: Paint) -> Vec<String> {
    let mut problems: Vec<&DoctorCheck> = connections
        .iter()
        .copied()
        .filter(|check| check.status != DoctorStatus::Healthy)
        .collect();
    problems.sort_by(|a, b| {
        (severity(a.status), a.connection_name.as_deref())
            .cmp(&(severity(b.status), b.connection_name.as_deref()))
    });
    let names: Vec<String> = problems
        .iter()
        .map(|check| {
            truncate(
                check.connection_name.as_deref().unwrap_or("Connection"),
                NAME_WIDTH,
            )
        })
        .collect();
    let width = names
        .iter()
        .map(|name| name.chars().count())
        .max()
        .unwrap_or(0);
    let mut lines = Vec::new();
    for (check, name) in problems.iter().zip(&names) {
        let recorded = match (check.connection_status, check.test_phase) {
            (Some(status), Some(phase)) => paint.dim(&format!(
                " (status {status}, test {})",
                phase.replace('_', " ")
            )),
            _ => String::new(),
        };
        let pad = width - name.chars().count();
        lines.push(format!(
            "{indent}{} {name}{} {}{recorded}",
            paint.mark(check.status),
            " ".repeat(pad),
            check.summary,
        ));
    }
    let mut actions: Vec<&str> = problems
        .iter()
        .filter_map(|check| check.action.as_deref())
        .collect();
    actions.sort_unstable();
    actions.dedup();
    lines.extend(
        actions
            .into_iter()
            .map(|action| format!("{indent}{} {action}", paint.arrow())),
    );
    lines
}

fn verdict(report: &DoctorReport, checks: &[&DoctorCheck], paint: Paint) -> String {
    let count = |wanted: &[DoctorStatus]| {
        checks
            .iter()
            .filter(|check| wanted.contains(&check.status))
            .count()
    };
    let problems = count(&[DoctorStatus::Error, DoctorStatus::Unknown]);
    let warnings = count(&[DoctorStatus::Warn]);
    let skipped = match count(&[DoctorStatus::Skipped]) {
        0 => String::new(),
        n => format!(" {} skipped.", plural(n, "check", "checks")),
    };
    match report.overall {
        DoctorStatus::Healthy | DoctorStatus::Skipped if skipped.is_empty() => {
            paint.bold("All checks passed.")
        }
        DoctorStatus::Healthy | DoctorStatus::Skipped => {
            paint.bold(&format!("All checks that ran passed.{skipped}"))
        }
        DoctorStatus::Warn => paint.bold(&format!(
            "Passed with {}.{skipped}",
            plural(warnings, "warning", "warnings")
        )),
        DoctorStatus::Error | DoctorStatus::Unknown => paint.bold(&format!(
            "{} attention. Apply each fix, then run `cloudthinker doctor` again.",
            plural(problems, "check needs", "checks need")
        )),
    }
}

fn label(id: &str) -> &str {
    match id {
        "cli" => "CLI",
        "configuration" => "Server",
        "credentials" => "Login",
        "api" => "API",
        "identity" => "Account",
        "workspace" => "Workspace",
        "connections" => "Connections",
        "release" => "Release",
        other => other,
    }
}

fn source_label(source: OriginSource) -> &'static str {
    match source {
        OriginSource::Flag => "(from --url)",
        OriginSource::Environment => "(from CLOUDTHINKER_URL)",
        OriginSource::Saved => "(saved by login)",
        OriginSource::BuiltIn => "(default)",
    }
}

fn severity(status: DoctorStatus) -> u8 {
    match status {
        DoctorStatus::Error => 0,
        DoctorStatus::Unknown => 1,
        DoctorStatus::Warn => 2,
        DoctorStatus::Skipped => 3,
        DoctorStatus::Healthy => 4,
    }
}

#[derive(Clone, Copy)]
struct Paint(bool);

impl Paint {
    fn mark(self, status: DoctorStatus) -> String {
        let (symbol, color) = match status {
            DoctorStatus::Healthy => ("✓", AnsiColors::Green),
            DoctorStatus::Warn => ("!", AnsiColors::Yellow),
            DoctorStatus::Error => ("✗", AnsiColors::Red),
            DoctorStatus::Unknown => ("?", AnsiColors::Yellow),
            DoctorStatus::Skipped => ("-", AnsiColors::BrightBlack),
        };
        if self.0 {
            symbol.color(color).bold().to_string()
        } else {
            symbol.to_string()
        }
    }

    fn arrow(self) -> String {
        if self.0 {
            "→".cyan().to_string()
        } else {
            "→".to_string()
        }
    }

    fn dim(self, text: &str) -> String {
        self.dim_unless(false, text)
    }

    fn dim_unless(self, keep: bool, text: &str) -> String {
        if self.0 && !keep && !text.is_empty() {
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
