//! Rendering: one JSON path, plus human helpers that respect stdout purity.
//!
//! Invariant (CA-CLI-10): in human mode `chat -p` writes ONLY the answer to
//! stdout so the result is pipeable. Progress, status, warnings, and errors all
//! go to stderr. `--json` writes exactly one envelope to stdout.

use std::io::Write;

use cloudthinker_client::{
    ReviewFinding, ReviewSeverityCounts, ReviewStatus, ReviewVerdict, ReviewView, RunStatus,
    RunView,
};
use owo_colors::OwoColorize;
use serde::Serialize;
use uuid::Uuid;

/// The `--json` envelope, using the API's field names verbatim.
#[derive(Debug, Serialize)]
pub struct ChatEnvelope {
    pub run_id: Uuid,
    pub conversation_id: Option<Uuid>,
    pub status: RunStatus,
    pub answer: Option<String>,
    pub web_url: Option<String>,
}

impl ChatEnvelope {
    pub fn from_view(view: &RunView) -> Self {
        Self {
            run_id: view.run_id,
            conversation_id: view.conversation_id,
            status: view.status,
            answer: view.answer.clone(),
            web_url: view.web_url.clone(),
        }
    }
}

/// The single JSON output path (no per-command `--json` branches beyond this).
pub fn emit_json<T: Serialize>(value: &T) -> Result<(), String> {
    let text = serde_json::to_string_pretty(value).map_err(|e| e.to_string())?;
    let mut out = std::io::stdout().lock();
    writeln!(out, "{text}").map_err(|e| e.to_string())
}

/// Write the bare answer to stdout (`chat -p` human mode). Nothing else.
pub fn print_answer(answer: &str) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{answer}");
}

/// Human summary for `chat status` (goes to stdout — it is the command's output).
pub fn print_status_summary(view: &RunView) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "run:    {}", view.run_id);
    let _ = writeln!(out, "status: {}", status_label(view.status));
    if let Some(kind) = &view.failure_kind {
        let _ = writeln!(out, "reason: {kind}");
    }
    if let Some(url) = &view.web_url
        && view.status == RunStatus::RequiredApproval
    {
        let _ = writeln!(out, "approve: {url}");
    }
    if view.status == RunStatus::Succeeded
        && let Some(answer) = &view.answer
    {
        let _ = writeln!(out, "\n{answer}");
    }
}

/// The `review` `--json` envelope, using the API's field names verbatim.
#[derive(Debug, Serialize)]
pub struct ReviewEnvelope {
    pub mr_iid: i64,
    pub status: ReviewStatus,
    pub verdict: ReviewVerdict,
    pub findings_count: i64,
    pub title: String,
    pub url: Option<String>,
    pub repository_path: Option<String>,
    pub provider: String,
    pub severity_counts: ReviewSeverityCounts,
    pub findings: Vec<ReviewFinding>,
}

impl ReviewEnvelope {
    pub fn from_view(view: &ReviewView) -> Self {
        Self {
            mr_iid: view.mr_iid,
            status: view.status,
            verdict: view.verdict,
            findings_count: view.findings_count,
            title: view.title.clone(),
            url: view.url.clone(),
            repository_path: view.repository_path.clone(),
            provider: view.provider.clone(),
            severity_counts: view.severity_counts,
            findings: view.findings.clone(),
        }
    }
}

/// Human summary for `review status`/`review watch` (goes to stdout — it is
/// the command's output).
pub fn print_review_status_summary(view: &ReviewView) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "mr:       {} ({})", view.mr_iid, view.provider);
    let _ = writeln!(out, "title:    {}", view.title);
    let _ = writeln!(out, "status:   {}", review_status_label(view.status));
    let _ = writeln!(out, "verdict:  {}", review_verdict_label(view.verdict));
    let _ = writeln!(out, "findings: {}", view.findings_count);
    if let Some(url) = &view.url {
        let _ = writeln!(out, "url:      {url}");
    }
}

/// Human findings list for `review findings` (stdout), worst-severity first —
/// `view.findings` is already sorted that way (CA-RV-2).
pub fn print_review_findings(view: &ReviewView) {
    let mut out = std::io::stdout().lock();
    if view.findings.is_empty() {
        let _ = writeln!(out, "no findings.");
        return;
    }
    for finding in &view.findings {
        let location = match (&finding.file_path, finding.line_number) {
            (Some(path), Some(line)) => format!("{path}:{line}"),
            (Some(path), None) => path.clone(),
            _ => "-".to_string(),
        };
        let category = finding.category.as_deref().unwrap_or("uncategorized");
        let resolved = if finding.resolved { " [resolved]" } else { "" };
        let _ = writeln!(
            out,
            "[{}] {} ({}) — {}{}",
            finding.severity, location, category, finding.issue_title, resolved
        );
    }
}

fn review_status_label(status: ReviewStatus) -> &'static str {
    match status {
        ReviewStatus::InReview => "in review",
        ReviewStatus::ReviewComplete => "review complete",
        ReviewStatus::Filtered => "filtered",
        ReviewStatus::Failed => "failed",
    }
}

fn review_verdict_label(verdict: ReviewVerdict) -> &'static str {
    match verdict {
        ReviewVerdict::InReview => "in review",
        ReviewVerdict::Approved => "approved",
        ReviewVerdict::ReviewSuggested => "review suggested",
        ReviewVerdict::ChangesRequested => "changes requested",
        ReviewVerdict::Failed => "failed",
        ReviewVerdict::Filtered => "filtered",
    }
}

/// Progress note to stderr (skipped in `--json` mode by the caller).
pub fn progress(message: &str) {
    eprintln!("{message}");
}

/// A colored error line to stderr; color is suppressed off-TTY and under NO_COLOR.
pub fn eprintln_error(message: &str) {
    if stderr_supports_color() {
        eprintln!("{} {message}", "error:".red().bold());
    } else {
        eprintln!("error: {message}");
    }
}

/// A one-line warning to stderr.
pub fn warn(message: &str) {
    if stderr_supports_color() {
        eprintln!("{} {message}", "warning:".yellow().bold());
    } else {
        eprintln!("warning: {message}");
    }
}

fn status_label(status: RunStatus) -> &'static str {
    match status {
        RunStatus::Pending => "pending",
        RunStatus::Running => "running",
        RunStatus::Succeeded => "succeeded",
        RunStatus::Failed => "failed",
        RunStatus::RequiredApproval => "approval required",
    }
}

fn stderr_supports_color() -> bool {
    supports_color::on(supports_color::Stream::Stderr).is_some()
}
