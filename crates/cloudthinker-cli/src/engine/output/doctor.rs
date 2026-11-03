use cloudthinker_client::DoctorReport;
use serde::Serialize;

use super::{emit_json, print_lines, terminal_text};
use crate::engine::exit::ExitCode;

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
    }
    if json {
        return emit_json(&DoctorEnvelope {
            report,
            exit_code: exit_code as u8,
        });
    }
    let mut lines = vec![format!(
        "Doctor: {} (exit {})",
        report.overall.label(),
        exit_code as u8
    )];
    if let Some(origin) = &report.origin {
        lines.push(format!("CloudThinker: {origin}"));
    }
    for check in &report.checks {
        let subject = check
            .connection_id
            .map_or_else(|| check.id.to_string(), |id| format!("connection {id}"));
        let http = check
            .http_status
            .map_or_else(String::new, |status| format!(" (HTTP {status})"));
        lines.push(format!(
            "[{}] {subject}{http}: {}",
            check.status.label(),
            check.summary
        ));
        if let Some(action) = &check.action {
            lines.push(format!("  {action}"));
        }
        if let (Some(status), Some(phase)) = (check.connection_status, check.test_phase) {
            lines.push(format!("  Recorded status: {status}; test phase: {phase}."));
        }
    }
    print_lines(&lines)
}
