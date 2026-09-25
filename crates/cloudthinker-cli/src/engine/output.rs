//! Rendering: one JSON path, plus human helpers that respect stdout purity.
//!
//! Invariant (CA-CLI-10): in human mode `chat -p` writes ONLY the answer to
//! stdout so the result is pipeable. Progress, status, warnings, and errors all
//! go to stderr. `--json` writes exactly one envelope to stdout.

use std::io::{self, IsTerminal, Write};

use cloudthinker_client::{
    CliIdentity, ReviewFinding, ReviewSeverityCounts, ReviewStatus, ReviewVerdict, ReviewView,
    RunListItem, RunStatus, RunView, SubmittedRun, worker_types::ExecutorChoicePublic,
};
use owo_colors::{AnsiColors, OwoColorize};
use serde::Serialize;
use uuid::Uuid;

/// Write one line to `out`, treating a broken pipe (the reader hung up, e.g.
/// piping into `head`) as success per Unix convention rather than an error
/// for the caller to report. Any other write failure surfaces so the
/// automation contract holds: a script can trust that a non-zero exit means
/// the output didn't make it out.
fn write_line(out: &mut impl Write, line: &str) -> Result<(), String> {
    match writeln!(out, "{line}") {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::BrokenPipe => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}

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

/// The immediate `chat -p --no-wait --json` response.
#[derive(Debug, Serialize)]
pub struct ChatSubmittedEnvelope {
    pub run_id: Uuid,
    pub conversation_id: Uuid,
    pub status: RunStatus,
    pub web_url: String,
}

impl From<&SubmittedRun> for ChatSubmittedEnvelope {
    fn from(run: &SubmittedRun) -> Self {
        Self {
            run_id: run.run_id,
            conversation_id: run.conversation_id,
            status: run.status,
            web_url: run.web_url.clone(),
        }
    }
}

/// The single JSON output path (no per-command `--json` branches beyond this).
/// Shares `write_line`'s broken-pipe tolerance with human-mode output, so
/// `cloudthinker ... --json | head` exits 0 the same way human mode does.
pub fn emit_json<T: Serialize>(value: &T) -> Result<(), String> {
    let mut out = std::io::stdout().lock();
    emit_json_to(&mut out, value)
}

/// Writer-generic core of [`emit_json`], extracted for testing.
fn emit_json_to<T: Serialize>(out: &mut impl Write, value: &T) -> Result<(), String> {
    let text = serde_json::to_string_pretty(value).map_err(|e| e.to_string())?;
    write_line(out, &text)
}

/// Write the bare answer to stdout (`chat -p` human mode). Nothing else.
pub fn print_answer(answer: &str) -> Result<(), String> {
    let mut out = std::io::stdout().lock();
    write_line(&mut out, answer)
}

/// Write the raw access token to stdout, verbatim and alone. It is a secret a
/// caller pipes into a bearer header, so it is never labeled, colored, or
/// sanitized, and it never reaches stderr or a log.
pub fn print_access_token(token: &str) -> Result<(), String> {
    let mut out = std::io::stdout().lock();
    write_line(&mut out, token)
}

/// Write the identifiers needed to observe an asynchronously submitted run.
pub fn print_submitted(run: &SubmittedRun) -> Result<(), String> {
    let mut out = std::io::stdout().lock();
    write_line(
        &mut out,
        &format!(
            "run_id={} conversation_id={} status={} web_url={}",
            run.run_id,
            run.conversation_id,
            status_label(run.status),
            terminal_text(&run.web_url)
        ),
    )
}

/// Human table for `chat ls`. An empty result produces empty stdout.
pub fn print_run_list(runs: &[RunListItem]) -> Result<(), String> {
    if runs.is_empty() {
        return Ok(());
    }

    let mut out = std::io::stdout().lock();
    write_line(
        &mut out,
        "RUN ID\tCONVERSATION ID\tSTATUS\tPROMPT\tCREATED AT\tWEB URL",
    )?;
    for run in runs {
        let conversation_id = run
            .conversation_id
            .map_or_else(|| "-".to_string(), |id| id.to_string());
        let preview = run
            .prompt_preview
            .as_deref()
            .map_or_else(|| "-".to_string(), terminal_text);
        let web_url = run
            .web_url
            .as_deref()
            .map_or_else(|| "-".to_string(), terminal_text);
        write_line(
            &mut out,
            &format!(
                "{}\t{}\t{}\t{}\t{}\t{}",
                run.run_id,
                conversation_id,
                status_label(run.status),
                preview,
                run.created_at.to_rfc3339(),
                web_url
            ),
        )?;
    }
    Ok(())
}

/// Stable machine-readable continuation hint for every terminal run.
pub fn continuation_hint(conversation_id: Option<Uuid>, web_url: Option<&str>) {
    let conversation_id = conversation_id.map_or_else(|| "-".to_string(), |id| id.to_string());
    let web_url = web_url.map_or_else(|| "-".to_string(), terminal_text);
    eprintln!("continue_with={conversation_id} web_url={web_url}",);
}

pub fn print_whoami(identity: &CliIdentity) -> Result<(), String> {
    let mut out = std::io::stdout().lock();
    write_line(&mut out, &format_whoami(identity))
}

#[derive(Debug, Serialize)]
pub struct WhoamiEnvelope {
    pub host: String,
    pub user_id: Uuid,
    pub workspace_id: Uuid,
}

impl From<&CliIdentity> for WhoamiEnvelope {
    fn from(identity: &CliIdentity) -> Self {
        Self {
            host: identity.host.clone(),
            user_id: identity.user_id,
            workspace_id: identity.workspace_id,
        }
    }
}

pub fn emit_whoami(identity: &CliIdentity, json: bool) -> Result<(), String> {
    if json {
        emit_json(&WhoamiEnvelope::from(identity))
    } else {
        print_whoami(identity)
    }
}

fn format_whoami(identity: &CliIdentity) -> String {
    format!(
        "host={} email={} workspace={} ({})",
        terminal_text(&identity.host),
        terminal_text(&identity.user_email),
        terminal_text(&identity.workspace_name),
        identity.workspace_id
    )
}

/// Strip terminal control and Unicode bidi-control characters from server text.
pub fn terminal_text(value: &str) -> String {
    value
        .chars()
        .filter(|character| {
            !character.is_control()
                && !matches!(
                    character,
                    '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}'
                )
        })
        .collect()
}

/// Write one human-mode result line to stdout (`update`'s outcome message).
pub fn print_update_result(message: &str) -> Result<(), String> {
    let mut out = std::io::stdout().lock();
    write_line(&mut out, message)
}

/// Human summary for `chat status` (goes to stdout — it is the command's output).
pub fn print_status_summary(view: &RunView) -> Result<(), String> {
    let mut out = std::io::stdout().lock();
    write_line(&mut out, &format!("run:    {}", view.run_id))?;
    write_line(&mut out, &format!("status: {}", status_label(view.status)))?;
    if let Some(kind) = &view.failure_kind {
        write_line(&mut out, &format!("reason: {kind}"))?;
    }
    if let Some(url) = &view.web_url
        && view.status == RunStatus::RequiredApproval
    {
        write_line(&mut out, &format!("approve: {url}"))?;
    }
    if view.status == RunStatus::Succeeded
        && let Some(answer) = &view.answer
    {
        write_line(&mut out, &format!("\n{answer}"))?;
    }
    Ok(())
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
pub fn print_review_status_summary(view: &ReviewView) -> Result<(), String> {
    let mut out = std::io::stdout().lock();
    write_line(
        &mut out,
        &format!("mr:       {} ({})", view.mr_iid, view.provider),
    )?;
    write_line(&mut out, &format!("title:    {}", view.title))?;
    write_line(
        &mut out,
        &format!("status:   {}", review_status_label(view.status)),
    )?;
    write_line(
        &mut out,
        &format!("verdict:  {}", review_verdict_label(view.verdict)),
    )?;
    write_line(&mut out, &format!("findings: {}", view.findings_count))?;
    if let Some(url) = &view.url {
        write_line(&mut out, &format!("url:      {url}"))?;
    }
    Ok(())
}

/// Human findings list for `review findings` (stdout), worst-severity first —
/// `view.findings` is already sorted that way (CA-RV-2).
pub fn print_review_findings(view: &ReviewView) -> Result<(), String> {
    let mut out = std::io::stdout().lock();
    if view.findings.is_empty() {
        return write_line(&mut out, "no findings.");
    }
    for finding in &view.findings {
        let location = match (&finding.file_path, finding.line_number) {
            (Some(path), Some(line)) => format!("{path}:{line}"),
            (Some(path), None) => path.clone(),
            _ => "-".to_string(),
        };
        let category = finding.category.as_deref().unwrap_or("uncategorized");
        let resolved = if finding.resolved { " [resolved]" } else { "" };
        write_line(
            &mut out,
            &format!(
                "[{}] {} ({}) — {}{}",
                finding.severity, location, category, finding.issue_title, resolved
            ),
        )?;
    }
    Ok(())
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

const STEP_TICK: std::time::Duration = std::time::Duration::from_millis(80);
const STEP_FRAMES: &str = "⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏ ";

pub struct Step(indicatif::ProgressBar);

pub fn step(message: &str) -> Step {
    if !io::stderr().is_terminal() {
        progress(message);
        return Step(indicatif::ProgressBar::hidden());
    }
    let style = indicatif::ProgressStyle::with_template("{spinner:.cyan} {msg}")
        .map(|style| style.tick_chars(STEP_FRAMES))
        .unwrap_or_else(|_| indicatif::ProgressStyle::default_spinner());
    let spinner = indicatif::ProgressBar::new_spinner()
        .with_style(style)
        .with_message(message.to_string());
    spinner.enable_steady_tick(STEP_TICK);
    Step(spinner)
}

impl Drop for Step {
    fn drop(&mut self) {
        self.0.finish_and_clear();
    }
}

pub fn done(message: &str) {
    labeled_eprintln("✓", AnsiColors::Green, message);
}

/// One labeled stderr line; color is suppressed off-TTY and under NO_COLOR.
fn labeled_eprintln(label: &str, color: AnsiColors, message: &str) {
    if stderr_supports_color() {
        eprintln!("{} {message}", label.color(color).bold());
    } else {
        eprintln!("{label} {message}");
    }
}

/// A one-line error to stderr.
pub fn eprintln_error(message: &str) {
    labeled_eprintln("error:", AnsiColors::Red, message);
}

/// A one-line warning to stderr.
pub fn warn(message: &str) {
    labeled_eprintln("warning:", AnsiColors::Yellow, message);
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

pub fn print_document(document: &str) -> Result<(), String> {
    match std::io::stdout().lock().write_all(document.as_bytes()) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::BrokenPipe => Ok(()),
        Err(error) => Err(error.to_string()),
    }
}

pub fn emit_worker<T: Serialize>(value: &T, human: &str, json: bool) -> Result<(), String> {
    if json {
        emit_json(value)
    } else {
        print_answer(human)
    }
}

/// Human lines for `worker outpost` and `worker status`. Every outpost name is
/// workspace-supplied, so each one reaches the terminal through
/// [`terminal_text`].
pub fn outpost_created_line(name: &str, target_id: Uuid) -> String {
    format!(
        "Created outpost {}; start it with cloudthinker worker start --outpost {target_id} --workdir <directory>",
        terminal_text(name)
    )
}

pub fn outpost_archived_line(name: &str) -> String {
    format!("Archived outpost {}", terminal_text(name))
}

pub fn outpost_status_line(choice: &ExecutorChoicePublic) -> String {
    format!("{}: {}", terminal_text(&choice.name), choice.availability)
}

pub fn outpost_list_text(choices: &[ExecutorChoicePublic]) -> String {
    choices
        .iter()
        .map(|choice| format!("{}  {}", terminal_text(&choice.name), choice.availability))
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn worker_serving_line(name: &str, label: &str) -> String {
    format!(
        "Serving {} from {label}. File tools stay in this folder; shell commands run as your user without a sandbox.",
        terminal_text(name)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `Write` double that always fails with the configured error kind.
    struct FailingWriter(io::ErrorKind);

    impl Write for FailingWriter {
        fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
            Err(io::Error::from(self.0))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    // [ISSUE-4]: a broken pipe (the reader hung up, e.g. `| head`) is the
    // normal Unix shutdown path for a pipeline, not a failure the CLI should
    // report or exit non-zero for.
    #[test]
    fn write_line_treats_broken_pipe_as_success() {
        let mut out = FailingWriter(io::ErrorKind::BrokenPipe);
        assert_eq!(write_line(&mut out, "hello"), Ok(()));
    }

    // Any other write failure must surface so a script relying on the exit
    // code can tell the output didn't make it out.
    #[test]
    fn write_line_surfaces_other_errors() {
        let mut out = FailingWriter(io::ErrorKind::PermissionDenied);
        assert!(write_line(&mut out, "hello").is_err());
    }

    // `--json` and human output must agree on broken-pipe tolerance,
    // otherwise `chat -p --json | head` exits non-zero for a successful run.
    #[test]
    fn emit_json_treats_broken_pipe_as_success() {
        let mut out = FailingWriter(io::ErrorKind::BrokenPipe);
        assert_eq!(emit_json_to(&mut out, &"hello"), Ok(()));
    }

    #[test]
    fn emit_json_surfaces_other_errors() {
        let mut out = FailingWriter(io::ErrorKind::PermissionDenied);
        assert!(emit_json_to(&mut out, &"hello").is_err());
    }

    fn outpost(name: &str) -> ExecutorChoicePublic {
        use cloudthinker_client::worker_types::{
            ExecutorAvailability, ExecutorChoiceKind, ExecutorScope,
        };
        ExecutorChoicePublic {
            availability: ExecutorAvailability::Available,
            capabilities: Vec::new(),
            kind: ExecutorChoiceKind::Outpost,
            last_verified_at: None,
            name: name.to_string(),
            scope: ExecutorScope::Personal,
            target_id: Some(Uuid::from_u128(3)),
            verification_error: None,
        }
    }

    #[test]
    fn ca_wo_21_outpost_lines_strip_terminal_and_bidi_controls_from_the_name() {
        let hostile = "build\u{1b}]0;owned\u{7}\u{202e}txt";

        assert_eq!(
            outpost_list_text(&[outpost(hostile)]),
            "build]0;ownedtxt  available"
        );
        assert_eq!(
            outpost_status_line(&outpost(hostile)),
            "build]0;ownedtxt: available"
        );
        assert_eq!(
            outpost_archived_line(hostile),
            "Archived outpost build]0;ownedtxt"
        );
        assert_eq!(
            outpost_created_line(hostile, Uuid::from_u128(3)),
            "Created outpost build]0;ownedtxt; start it with cloudthinker worker start --outpost 00000000-0000-0000-0000-000000000003 --workdir <directory>"
        );
        assert_eq!(
            worker_serving_line(hostile, "outpost folder"),
            "Serving build]0;ownedtxt from outpost folder. File tools stay in this folder; shell commands run as your user without a sandbox."
        );
    }

    #[test]
    fn whoami_strips_terminal_and_bidi_controls_from_server_text() {
        let identity = CliIdentity {
            host: "api.example\nforged".into(),
            user_id: Uuid::from_u128(2),
            user_email: "user\u{1b}]0;owned\u{7}@example.com".into(),
            workspace_id: Uuid::from_u128(1),
            workspace_name: "Prod\u{202e}txt".into(),
        };

        assert_eq!(
            format_whoami(&identity),
            "host=api.exampleforged email=user]0;owned@example.com workspace=Prodtxt (00000000-0000-0000-0000-000000000001)"
        );
    }
}
