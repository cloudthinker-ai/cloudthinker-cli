//! Rendering: one JSON path, plus human helpers that respect stdout purity.
//!
//! Invariant (CA-CLI-10): in human mode `chat -p` writes ONLY the answer to
//! stdout so the result is pipeable. Progress, status, warnings, and errors all
//! go to stderr. `--json` writes exactly one envelope to stdout.

use std::fs::{OpenOptions, symlink_metadata};
use std::io::{self, IsTerminal, Write};
use std::path::Path;

use crate::engine::local_review::LocalReviewResult;
use cloudthinker_client::{
    CliIdentity, CoverageReport, CoverageStatus, CyberApp, CyberDomain, CyberExecutionHost,
    CyberExport, CyberFinding, CyberRun, CyberRunBrief, CyberRunResult, CyberSessionEntry,
    EvidenceReceipt, Partition, ReviewFinding, ReviewSeverityCounts, ReviewStatus, ReviewVerdict,
    ReviewView, RunListItem, RunStatus, RunView, SettleResult, StoredWorkspace, SubmittedRun,
    Surface, WorkPlan, worker_types::ExecutorChoicePublic,
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
pub struct ChatEnvelope<'a> {
    pub run_id: Uuid,
    pub conversation_id: Option<Uuid>,
    pub status: RunStatus,
    pub answer: Option<&'a str>,
    pub web_url: Option<&'a str>,
    pub message: Option<&'a str>,
    pub failure_kind: Option<&'a str>,
}

impl<'a> ChatEnvelope<'a> {
    pub fn from_view(view: &'a RunView) -> Self {
        Self {
            run_id: view.run_id,
            conversation_id: view.conversation_id,
            status: view.status,
            answer: view.answer.as_deref(),
            web_url: view.web_url.as_deref(),
            message: view.message.as_deref(),
            failure_kind: view.failure_kind.as_deref(),
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

pub fn write_json_file<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(value).map_err(|error| error.to_string())?;
    write_file(path, &bytes)
}

pub fn write_file(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let mut options = OpenOptions::new();
    options.write(true);
    match symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(format!(
                "refusing to write through symlink {}",
                path.display()
            ));
        }
        Ok(metadata) if metadata.is_file() => {
            options.truncate(true);
        }
        Ok(_) => return Err(format!("output path is not a file: {}", path.display())),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            options.create_new(true);
        }
        Err(error) => return Err(error.to_string()),
    }
    let mut file = options.open(path).map_err(|error| error.to_string())?;
    file.write_all(bytes).map_err(|error| error.to_string())?;
    file.sync_all().map_err(|error| error.to_string())
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
        progress("No headless runs yet. Start one with: cloudthinker chat -p '<prompt>'");
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
    pub user_email: String,
    pub workspace_id: Uuid,
    pub workspace_name: String,
}

impl From<&CliIdentity> for WhoamiEnvelope {
    fn from(identity: &CliIdentity) -> Self {
        Self {
            host: identity.host.clone(),
            user_id: identity.user_id,
            user_email: identity.user_email.clone(),
            workspace_id: identity.workspace_id,
            workspace_name: identity.workspace_name.clone(),
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

pub fn print_config_value(key: &str, value: &str) -> Result<(), String> {
    let mut out = std::io::stdout().lock();
    write_line(&mut out, &format!("{key} = {value}"))
}

pub fn print_lines(lines: &[String]) -> Result<(), String> {
    let mut out = std::io::stdout().lock();
    for line in lines {
        write_line(&mut out, line)?;
    }
    Ok(())
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
pub struct ReviewEnvelope<'a> {
    pub mr_iid: i64,
    pub status: ReviewStatus,
    pub verdict: ReviewVerdict,
    pub findings_count: i64,
    pub title: &'a str,
    pub url: Option<&'a str>,
    pub repository_path: Option<&'a str>,
    pub provider: &'a str,
    pub severity_counts: ReviewSeverityCounts,
    pub findings: &'a [ReviewFinding],
}

impl<'a> ReviewEnvelope<'a> {
    pub fn from_view(view: &'a ReviewView) -> Self {
        Self {
            mr_iid: view.mr_iid,
            status: view.status,
            verdict: view.verdict,
            findings_count: view.findings_count,
            title: &view.title,
            url: view.url.as_deref(),
            repository_path: view.repository_path.as_deref(),
            provider: &view.provider,
            severity_counts: view.severity_counts,
            findings: &view.findings,
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

/// Human one-liner for `cyber run launch` (stdout).
pub fn print_cyber_run(run: &CyberRun) -> Result<(), String> {
    let mut out = std::io::stdout().lock();
    write_line(
        &mut out,
        &format!(
            "run:      {} (host: {}, conversation: {})",
            run.run_id,
            cyber_host_label(run.execution_host),
            run.conversation_id
                .map_or_else(|| "unbound".to_string(), |id| id.to_string())
        ),
    )?;
    write_line(&mut out, &format!("app:      {}", run.app_id))?;
    write_line(
        &mut out,
        &format!(
            "state:    {} (new: {}, confirmed: {}, critical: {})",
            cyber_result_label(run.result),
            run.findings_discovered,
            run.findings_confirmed,
            run.findings_confirmed_critical
        ),
    )
}

pub fn print_cyber_apps(apps: &[CyberApp]) -> Result<(), String> {
    let mut out = std::io::stdout().lock();
    if apps.is_empty() {
        return write_line(&mut out, "no Apps found.");
    }
    write_line(
        &mut out,
        "APP ID\tNAME\tTARGET\tSETUP\tDOMAIN\tOPEN FINDINGS",
    )?;
    for app in apps {
        write_line(
            &mut out,
            &format!(
                "{}\t{}\t{}\t{}\t{}\t{}",
                app.app_id,
                terminal_text(&app.name),
                terminal_text(&app.target_ref),
                terminal_text(&app.setup_status),
                terminal_text(&app.domain_status),
                app.open_findings_count
            ),
        )?;
    }
    Ok(())
}

pub fn print_cyber_domains(domains: &[CyberDomain]) -> Result<(), String> {
    let mut out = std::io::stdout().lock();
    if domains.is_empty() {
        return write_line(&mut out, "no domains found.");
    }
    write_line(&mut out, "DOMAIN ID\tDOMAIN\tSTATUS")?;
    for domain in domains {
        write_line(
            &mut out,
            &format!(
                "{}\t{}\t{}",
                domain.domain_id,
                terminal_text(&domain.domain),
                terminal_text(&domain.status)
            ),
        )?;
    }
    Ok(())
}

pub fn print_cyber_findings(findings: &[CyberFinding]) -> Result<(), String> {
    let mut out = std::io::stdout().lock();
    if findings.is_empty() {
        return write_line(&mut out, "no findings.");
    }
    for finding in findings {
        write_line(
            &mut out,
            &terminal_text(&format!(
                "[{}] {} {} ({}) — {}",
                finding.severity,
                finding.display_id.as_deref().unwrap_or("-"),
                finding.status,
                finding.triage_state,
                terminal_text(&finding.title)
            )),
        )?;
    }
    Ok(())
}

pub fn print_cyber_finding(finding: &CyberFinding) -> Result<(), String> {
    let mut out = std::io::stdout().lock();
    write_line(
        &mut out,
        &terminal_text(&format!(
            "finding:  {}\nseverity:  {}\nstatus:    {}\ntriage:    {}\ntitle:     {}",
            finding.display_id.as_deref().unwrap_or("-"),
            finding.severity,
            finding.status,
            finding.triage_state,
            terminal_text(&finding.title)
        )),
    )?;
    write_line(&mut out, &terminal_text(&finding.description))
}

pub fn print_cyber_export(export: &CyberExport) -> Result<(), String> {
    let mut out = std::io::stdout().lock();
    write_line(
        &mut out,
        &format!("filename: {}", terminal_text(&export.filename)),
    )?;
    write_line(
        &mut out,
        &format!("download_url: {}", terminal_text(&export.download_url)),
    )
}

/// Human to-do list for `cyber probe plan` (stdout): the checks the backend wants
/// run on this machine.
pub fn print_work_plan(plan: &WorkPlan) -> Result<(), String> {
    let mut out = std::io::stdout().lock();
    for line in work_plan_lines(plan) {
        write_line(&mut out, &line)?;
    }
    Ok(())
}

fn work_plan_lines(plan: &WorkPlan) -> Vec<String> {
    let mut lines = vec![
        format!(
            "plan:     {} (run: {})",
            terminal_text(&plan.plan_id),
            plan.run_id
        ),
        format!("target:   {}", terminal_text(&plan.target_ref)),
        format!("checks:   {}", plan.rows.len()),
    ];
    lines.extend(plan.rows.iter().map(|row| {
        if row.executable {
            format!(
                "  {:<12} {:<9} {}",
                terminal_text(&row.row_id),
                terminal_text(&row.method),
                terminal_text(&row.url)
            )
        } else {
            format!(
                "  {:<12} {:<9} (skip: {})",
                terminal_text(&row.row_id),
                terminal_text(&row.locator),
                terminal_text(&row.note)
            )
        }
    }));
    lines
}

/// Human coverage summary for `cyber probe coverage` / `cyber probe exec` (stdout).
pub fn print_coverage_report(report: &CoverageReport) -> Result<(), String> {
    let mut out = std::io::stdout().lock();
    let coverage = &report.coverage;
    write_line(
        &mut out,
        &format!(
            "plan:     {}",
            coverage
                .plan_id
                .as_deref()
                .map_or_else(|| "(none)".to_string(), terminal_text)
        ),
    )?;
    write_line(
        &mut out,
        &format!(
            "rows:     {} terminal / {} total",
            coverage.terminal, coverage.total
        ),
    )?;
    write_line(
        &mut out,
        &format!(
            "gate:     {}",
            if coverage.valid {
                "complete"
            } else {
                "incomplete"
            }
        ),
    )?;
    if !coverage.blockers.is_empty() {
        write_line(
            &mut out,
            &format!("blockers: {}", terminal_text(&coverage.blockers.join(", "))),
        )?;
    }
    for row in &report.rows {
        write_line(
            &mut out,
            &format!(
                "  {:<26} {:<9} {:<28} {}",
                coverage_status_label(row.status),
                terminal_text(&row.method),
                terminal_text(&row.locator),
                terminal_text(&row.reason)
            ),
        )?;
    }
    Ok(())
}

/// Human attack-surface overview for `cyber probe surface` (stdout): the
/// distribution an agent authors themes against, as a readable JSON block.
pub fn print_surface(surface: &Surface) -> Result<(), String> {
    let mut out = std::io::stdout().lock();
    write_line(
        &mut out,
        &format!("plan:     {}", terminal_text(&surface.plan_id)),
    )?;
    let body = serde_json::to_string_pretty(&surface.surface).map_err(|e| e.to_string())?;
    write_line(&mut out, &terminal_text(&body))
}

/// Human lane assignment for `cyber probe partition` (stdout): one row per
/// theme lane a scout owns, plus any advisory over-cap notes.
pub fn print_partition(partition: &Partition) -> Result<(), String> {
    let mut out = std::io::stdout().lock();
    write_line(
        &mut out,
        &format!("plan:     {}", terminal_text(&partition.plan_id)),
    )?;
    write_line(&mut out, &format!("lanes:    {}", partition.shards.len()))?;
    write_line(
        &mut out,
        &format!(
            "unmatched:{} ({}%)",
            partition.unmatched, partition.unmatched_pct
        ),
    )?;
    for shard in &partition.shards {
        write_line(
            &mut out,
            &format!(
                "  {:<16} {:>3} rows  {}",
                terminal_text(&shard.shard_id),
                shard.row_ids.len(),
                terminal_text(&shard.focus)
            ),
        )?;
    }
    for note in &partition.notes {
        write_line(&mut out, &format!("  note: {}", terminal_text(note)))?;
    }
    Ok(())
}

fn coverage_status_label(status: CoverageStatus) -> &'static str {
    match status {
        CoverageStatus::Untested => "untested",
        CoverageStatus::Assigned => "assigned",
        CoverageStatus::Covered => "covered",
        CoverageStatus::Candidate => "candidate",
        CoverageStatus::CandidatePromoted => "candidate_promoted",
        CoverageStatus::CandidateDismissed => "candidate_dismissed",
        CoverageStatus::CandidateNeedsVerification => "candidate_needs_verification",
        CoverageStatus::Blocked => "blocked",
        CoverageStatus::SkippedWithReason => "skipped_with_reason",
    }
}

/// The `--json` envelope for `cyber session`: the run binding plus the entries.
#[derive(Debug, Serialize)]
pub struct CyberSessionEnvelope<'a> {
    pub run_id: Uuid,
    pub conversation_id: Option<Uuid>,
    pub entries: &'a [CyberSessionEntry],
}

/// Human transcript for `cyber run session` (stdout). Walks the active branch from
/// the newest entry back through `parent_id`, so a pi `/tree` jump does not
/// replay a dead branch. Only `message` entries render; everything else is
/// session bookkeeping the terminal does not need to re-read.
pub fn print_cyber_session(run: &CyberRun, entries: &[CyberSessionEntry]) -> Result<(), String> {
    let mut out = std::io::stdout().lock();
    write_line(&mut out, &format!("run:     {}", run.run_id))?;
    match run.conversation_id {
        Some(id) => write_line(&mut out, &format!("session: {id}"))?,
        None => {
            return write_line(
                &mut out,
                "session: not bound yet (run `cloudthinker cyber run bind` first)",
            );
        }
    }
    let by_id: std::collections::HashMap<&str, &CyberSessionEntry> = entries
        .iter()
        .map(|entry| (entry.entry_id.as_str(), entry))
        .collect();
    let mut branch: Vec<&CyberSessionEntry> = Vec::new();
    let mut cursor = entries.iter().max_by_key(|entry| entry.seq);
    while let Some(entry) = cursor {
        branch.push(entry);
        cursor = entry
            .parent_id
            .as_deref()
            .and_then(|id| by_id.get(id).copied());
    }
    branch.reverse();
    for entry in branch {
        for line in render_session_entry(entry) {
            write_line(&mut out, &line)?;
        }
    }
    Ok(())
}

fn render_session_entry(entry: &CyberSessionEntry) -> Vec<String> {
    if entry.entry_type != "message" {
        return Vec::new();
    }
    let message = entry.payload.get("message");
    let role = message
        .and_then(|m| m.get("role"))
        .and_then(|v| v.as_str())
        .unwrap_or("?");
    let content = message.and_then(|m| m.get("content"));
    let mut lines = Vec::new();
    if let Some(blocks) = content.and_then(|c| c.as_array()) {
        for block in blocks {
            match block.get("type").and_then(|v| v.as_str()) {
                Some("text") => {
                    if let Some(text) = block.get("text").and_then(|v| v.as_str()) {
                        for line in text.lines() {
                            lines.push(terminal_text(&format!("{role}: {line}")));
                        }
                    }
                }
                Some("toolCall") => {
                    let name = block.get("name").and_then(|v| v.as_str()).unwrap_or("tool");
                    let first = block
                        .get("arguments")
                        .and_then(|a| a.get("command").or_else(|| a.get("path")))
                        .and_then(|v| v.as_str())
                        .and_then(|target| target.lines().next())
                        .unwrap_or("");
                    lines.push(terminal_text(&format!(
                        "  -> {name}: {}",
                        truncate(first, 160)
                    )));
                }
                Some("toolResult") => lines.push("  <-".to_string()),
                _ => {}
            }
        }
    } else if let Some(text) = content.and_then(|c| c.as_str()) {
        for line in text.lines() {
            lines.push(terminal_text(&format!("{role}: {line}")));
        }
    }
    lines
}

fn truncate(value: &str, max: usize) -> String {
    if value.chars().count() <= max {
        return value.to_string();
    }
    let mut out: String = value.chars().take(max).collect();
    out.push('…');
    out
}

/// Human summary of the target and profile bound to this session.
pub fn print_run_brief(brief: &CyberRunBrief) -> Result<(), String> {
    print_lines(&run_brief_lines(brief))
}

fn run_brief_lines(brief: &CyberRunBrief) -> Vec<String> {
    let mut lines = vec![
        format!(
            "run:      {} ({})",
            brief.run_id,
            terminal_text(&brief.app_name)
        ),
        format!("target:   {}", terminal_text(&brief.target)),
        format!(
            "profile:  {} / {} / {} ({})",
            brief.mode,
            brief.intensity,
            brief.scan_mode,
            terminal_text(&brief.frameworks.join(", "))
        ),
        "evidence: upload local `surface/` and `findings/` files with `cyber run evidence`"
            .to_string(),
    ];
    if let Some(workspace) = &brief.local_workspace {
        lines.push(format!(
            "workspace: {}",
            terminal_text(&workspace.workspace_root)
        ));
    }
    lines.push("status:   ready; bound to this CloudThinker session".to_string());
    lines
}

/// Human receipt for `cyber run evidence` (stdout).
pub fn print_evidence_receipt(receipt: &EvidenceReceipt) -> Result<(), String> {
    let mut out = std::io::stdout().lock();
    write_line(
        &mut out,
        &format!("written:  {} file(s)", receipt.written.len()),
    )?;
    for path in &receipt.written {
        write_line(&mut out, &format!("  {}", terminal_text(path)))?;
    }
    if !receipt.skipped.is_empty() {
        write_line(
            &mut out,
            &format!("skipped:  {} file(s)", receipt.skipped.len()),
        )?;
        for skipped in &receipt.skipped {
            write_line(
                &mut out,
                &format!(
                    "  {} ({})",
                    terminal_text(&skipped.path),
                    terminal_text(&skipped.reason)
                ),
            )?;
        }
    }
    Ok(())
}

/// Human outcome for `cyber run settle` (stdout).
pub fn print_settle_result(result: &SettleResult) -> Result<(), String> {
    let mut out = std::io::stdout().lock();
    if result.settled {
        write_line(&mut out, "settled")
    } else {
        write_line(
            &mut out,
            "nothing to settle (another writer got there first, or the run already ended)",
        )
    }
}

fn cyber_result_label(result: CyberRunResult) -> &'static str {
    match result {
        CyberRunResult::Running => "running",
        CyberRunResult::Success => "success",
        CyberRunResult::Failed => "failed",
        CyberRunResult::Cancelled => "cancelled",
    }
}

fn cyber_host_label(host: CyberExecutionHost) -> &'static str {
    match host {
        CyberExecutionHost::Cloud => "cloud",
        CyberExecutionHost::Local => "local",
    }
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

pub fn print_local_review(result: &LocalReviewResult) -> Result<(), String> {
    let mut out = std::io::stdout().lock();
    if result.findings.is_empty() {
        write_line(&mut out, "No findings.")?;
    } else {
        for finding in &result.findings {
            write_line(
                &mut out,
                &format!(
                    "[{}] {}:{} — {}",
                    format!("{:?}", finding.severity).to_uppercase(),
                    terminal_text(&finding.file),
                    finding.line,
                    terminal_text(&finding.title)
                ),
            )?;
            write_line(
                &mut out,
                &format!("  {}", terminal_text(&finding.explanation)),
            )?;
            if !finding.suggested_fix.trim().is_empty() {
                write_line(
                    &mut out,
                    &format!("  Suggested fix: {}", terminal_text(&finding.suggested_fix)),
                )?;
            }
        }
    }
    write_line(
        &mut out,
        &format!(
            "Reviewed {} changed file(s) with {} (base {}, head {}).",
            result.changed_files,
            terminal_text(&result.inference),
            &result.base_sha[..result.base_sha.len().min(12)],
            &result.head_sha[..result.head_sha.len().min(12)]
        ),
    )
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

pub fn live_status(message: &str) -> Step {
    if !io::stderr().is_terminal() {
        return Step(indicatif::ProgressBar::hidden());
    }
    let style = indicatif::ProgressStyle::with_template("{spinner:.cyan} {msg} ({elapsed})")
        .map(|style| style.tick_chars(STEP_FRAMES))
        .unwrap_or_else(|_| indicatif::ProgressStyle::default_spinner());
    let spinner = indicatif::ProgressBar::new_spinner()
        .with_style(style)
        .with_message(message.to_string());
    spinner.enable_steady_tick(STEP_TICK);
    Step(spinner)
}

impl Step {
    pub fn set_message(&self, message: String) {
        self.0.set_message(message);
    }
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

pub const WORKER_LOG_ENV: &str = "CLOUDTHINKER_WORKER_LOG";

pub fn worker_event(message: &str) {
    eprintln!(
        "{} {message}",
        chrono::Local::now().format("%Y-%m-%dT%H:%M:%S%:z")
    );
}

pub fn worker_debug(message: &str) {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if *ENABLED.get_or_init(|| {
        std::env::var(WORKER_LOG_ENV).is_ok_and(|level| level.eq_ignore_ascii_case("debug"))
    }) {
        worker_event(message);
    }
}

/// A one-line warning to stderr.
pub fn warn(message: &str) {
    labeled_eprintln("warning:", AnsiColors::Yellow, message);
}

pub fn status_label(status: RunStatus) -> &'static str {
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

#[derive(Debug, Serialize)]
pub struct AuthStatus {
    pub host: String,
    pub environment_token: bool,
    pub workspaces: Vec<StoredWorkspace>,
}

pub fn workspace_label(workspace: &StoredWorkspace) -> String {
    match &workspace.workspace_name {
        Some(name) => format!("{} ({})", terminal_text(name), workspace.workspace_id),
        None => workspace.workspace_id.to_string(),
    }
}

fn auth_status_text(status: &AuthStatus, login: &str) -> String {
    let mut lines = vec![format!("host={}", terminal_text(&status.host))];
    if status.workspaces.is_empty() {
        lines.push(format!("No stored login for this host. Run `{login}`."));
    }
    for workspace in &status.workspaces {
        let marker = if workspace.active { "*" } else { " " };
        let expiry = workspace.expires_at.map_or_else(String::new, |at| {
            format!("  access token expires {}", at.format("%Y-%m-%d %H:%M UTC"))
        });
        lines.push(format!("{marker} {}{expiry}", workspace_label(workspace)));
    }
    if !status.workspaces.is_empty() && !status.workspaces.iter().any(|w| w.active) {
        lines.push("No active workspace. Run `cloudthinker auth switch <id|name>`.".into());
    }
    if status.environment_token {
        lines.push(
            "CLOUDTHINKER_TOKEN is set, so commands use it instead of these stored logins.".into(),
        );
    }
    lines.join("\n")
}

pub fn emit_auth_status(status: &AuthStatus, login: &str, json: bool) -> Result<(), String> {
    if json {
        emit_json(status)
    } else {
        print_answer(&auth_status_text(status, login))
    }
}

pub fn emit_cloud(result: &cloudthinker_client::CloudResult, json: bool) -> Result<(), String> {
    if json {
        return emit_json(result);
    }
    use cloudthinker_client::{CloudResult, cloud_types as api};
    let lines = match result {
        CloudResult::Session(value) => vec![
            format!("session: {}", value.conversation_id),
            format!("workspace: {}", value.workspace_id),
            value.web_url.clone(),
            format!("Auto Mode: {}", value.auto_mode.enabled),
        ],
        CloudResult::Connections(value) => value
            .connections
            .iter()
            .map(|entry| {
                format!(
                    "{} [{}] {} — {}",
                    entry.prefix,
                    entry.alias,
                    entry.execution_method,
                    entry
                        .skills
                        .iter()
                        .map(|skill| skill.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            })
            .collect(),
        CloudResult::Content(value) => vec![
            format!("{} ({})", value.connection_prefix, value.execution_method),
            format!("cloud root: {}", value.cloud_root),
            value.content.clone(),
        ],
        CloudResult::Read {
            conversation_id,
            execution,
        } => {
            let mut lines = vec![format!("session: {conversation_id}")];
            match execution {
                api::ResponseAgentCliExecuteAgentCliRead::Completed(value) => lines.extend([
                    format!("completed (exit {})", value.return_code),
                    value.stdout.clone(),
                    value.stderr.clone(),
                ]),
                api::ResponseAgentCliExecuteAgentCliRead::Started(value) => {
                    lines.push(format!("running: {}", value.task_id))
                }
            }
            lines
        }
        CloudResult::Task {
            conversation_id,
            task_id,
            output,
        } => vec![
            format!("session: {conversation_id}"),
            format!("task: {task_id} ({})", output.status),
            output.output.clone(),
            format!(
                "next cursor: {}{}",
                output.next_cursor,
                if output.truncated { " (truncated)" } else { "" }
            ),
        ],
        CloudResult::Write(value) => {
            let mut lines = cloud_write_lines(&value.write);
            match &value.execution {
                Some(api::AgentCliWriteOutcomeExecution::Completed(value)) => lines.extend([
                    format!("exit: {}", value.return_code),
                    value.stdout.clone(),
                    value.stderr.clone(),
                ]),
                Some(api::AgentCliWriteOutcomeExecution::Started(value)) => {
                    lines.push(format!("task: {}", value.task_id))
                }
                None => (),
            }
            lines
        }
        CloudResult::WriteStatus(value) => cloud_write_lines(value),
        CloudResult::Writes(value) => value.writes.iter().flat_map(cloud_write_lines).collect(),
    };
    let text = if lines.is_empty() {
        "No Connections or operations found.".into()
    } else {
        lines.join("\n")
    };
    let text = text
        .lines()
        .map(terminal_text)
        .collect::<Vec<_>>()
        .join("\n");
    print_answer(&text)
}

fn cloud_write_lines(value: &cloudthinker_client::cloud_types::AgentCliWritePublic) -> Vec<String> {
    let mut lines = vec![
        format!("write: {} ({})", value.id, value.status),
        value.verdict_reason.clone(),
        value.web_url.clone(),
    ];
    if value.status == cloudthinker_client::cloud_types::AgentCliWriteStatus::Approved {
        lines.push(format!(
            "Approved; execution has not started. Resume with cloud exec --session {} --write {}.",
            value.conversation_id, value.id
        ));
    }
    lines
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
    fn chat_and_review_envelopes_keep_their_json_bytes() {
        let run_id = Uuid::parse_str("a1111111-1111-4111-8111-111111111111").unwrap();
        let conversation_id = Uuid::parse_str("b2222222-2222-4222-8222-222222222222").unwrap();
        let run = RunView {
            run_id,
            conversation_id: Some(conversation_id),
            status: RunStatus::Succeeded,
            answer: Some("done \"quoted\" é".into()),
            message: Some("run message".into()),
            failure_kind: None,
            web_url: None,
        };
        assert_eq!(
            serde_json::to_string(&ChatEnvelope::from_view(&run)).unwrap(),
            concat!(
                "{\"run_id\":\"a1111111-1111-4111-8111-111111111111\",",
                "\"conversation_id\":\"b2222222-2222-4222-8222-222222222222\",",
                "\"status\":\"succeeded\",\"answer\":\"done \\\"quoted\\\" é\",",
                "\"web_url\":null,\"message\":\"run message\",\"failure_kind\":null}"
            )
        );
        let review = ReviewView {
            mr_iid: 7,
            status: ReviewStatus::ReviewComplete,
            verdict: ReviewVerdict::ChangesRequested,
            findings_count: 2,
            title: "Primary change".into(),
            url: Some("https://example.com/mr/7".into()),
            repository_path: None,
            provider: "gitlab".into(),
            severity_counts: ReviewSeverityCounts {
                critical: 0,
                high: 1,
                medium: 1,
                low: 0,
            },
            findings: vec![
                ReviewFinding {
                    severity: "high".into(),
                    file_path: Some("src/lib.rs".into()),
                    line_number: Some(3),
                    issue_title: "Unchecked input".into(),
                    category: None,
                    resolved: false,
                },
                ReviewFinding {
                    severity: "medium".into(),
                    file_path: None,
                    line_number: None,
                    issue_title: "Naming".into(),
                    category: Some("style".into()),
                    resolved: true,
                },
            ],
        };
        assert_eq!(
            serde_json::to_string(&ReviewEnvelope::from_view(&review)).unwrap(),
            concat!(
                "{\"mr_iid\":7,\"status\":\"review_complete\",",
                "\"verdict\":\"changes_requested\",\"findings_count\":2,",
                "\"title\":\"Primary change\",\"url\":\"https://example.com/mr/7\",",
                "\"repository_path\":null,\"provider\":\"gitlab\",",
                "\"severity_counts\":{\"critical\":0,\"high\":1,\"medium\":1,\"low\":0},",
                "\"findings\":[{\"severity\":\"high\",\"file_path\":\"src/lib.rs\",",
                "\"line_number\":3,\"issue_title\":\"Unchecked input\",\"category\":null,",
                "\"resolved\":false},{\"severity\":\"medium\",\"file_path\":null,",
                "\"line_number\":null,\"issue_title\":\"Naming\",\"category\":\"style\",",
                "\"resolved\":true}]}"
            )
        );
    }

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

    #[test]
    fn cyber_work_plan_strips_terminal_and_bidi_controls_from_server_text() {
        let plan = WorkPlan {
            plan_id: "plan-1".into(),
            run_id: Uuid::from_u128(4),
            target_ref: "api\u{1b}[31m\u{202e}.example".into(),
            schema_version: 1,
            rows: vec![cloudthinker_client::PlanCheck {
                row_id: "row\n1".into(),
                asset_type: "endpoint".into(),
                locator: "GET /\u{202e}secret".into(),
                method: "GET\u{1b}[2J".into(),
                url: "https://api.example/\u{1b}]0;owned\u{7}".into(),
                executable: true,
                note: String::new(),
            }],
        };
        let lines = work_plan_lines(&plan).join("\n");
        assert!(!lines.contains('\u{1b}'));
        assert!(!lines.contains('\u{202e}'));
        assert!(!lines.contains("\n1"));
    }

    #[test]
    fn cyber_run_brief_includes_the_local_workspace_path_without_terminal_controls() {
        let brief = CyberRunBrief {
            run_id: Uuid::from_u128(1),
            app_id: Uuid::from_u128(2),
            app_name: "local app".into(),
            conversation_id: Some(Uuid::from_u128(3)),
            result: CyberRunResult::Running,
            execution_host: CyberExecutionHost::Local,
            target: "http://localhost".into(),
            frameworks: vec!["owasp_api".into()],
            mode: "white".into(),
            intensity: "full".into(),
            scan_mode: "incremental".into(),
            report_preferences: cloudthinker_client::worker_types::AppSecReportPreferences::default(
            ),
            report_reference: None,
            started_at: chrono::DateTime::from_timestamp(0, 0).unwrap(),
            finished_at: None,
            targets: vec![],
            local_workspace: Some(cloudthinker_client::LocalCyberWorkspace {
                workspace_root: "/tmp/cyber\u{1b}]0;owned\u{7}\nworkspace".into(),
                evidence_root: "/tmp/cyber/workspace/evidence".into(),
                workflow_root: "/tmp/cyber/workspace/workflow".into(),
                manifest: "/tmp/cyber/workspace/manifest.json".into(),
            }),
        };

        let lines = run_brief_lines(&brief);

        assert!(
            lines
                .iter()
                .any(|line| { line == "workspace: /tmp/cyber]0;ownedworkspace" })
        );
        assert!(lines.iter().all(|line| !line.contains('\u{1b}')));
    }

    #[test]
    fn render_session_entry_renders_text_and_tool_calls() {
        let entry = CyberSessionEntry {
            seq: 1,
            entry_id: "e1".to_string(),
            parent_id: None,
            entry_type: "message".to_string(),
            payload: serde_json::json!({
                "type": "message",
                "message": {
                    "role": "assistant",
                    "content": [
                        {"type": "text", "text": "hi\nthere"},
                        {
                            "type": "toolCall",
                            "name": "bash",
                            "arguments": {"command": "curl -s http://x\necho ignored"}
                        }
                    ]
                }
            }),
        };

        assert_eq!(
            render_session_entry(&entry),
            vec![
                "assistant: hi".to_string(),
                "assistant: there".to_string(),
                "  -> bash: curl -s http://x".to_string(),
            ]
        );
    }

    #[test]
    fn render_session_entry_skips_bookkeeping() {
        let entry = CyberSessionEntry {
            seq: 2,
            entry_id: "e2".to_string(),
            parent_id: None,
            entry_type: "model_change".to_string(),
            payload: serde_json::json!({"type": "model_change"}),
        };

        assert!(render_session_entry(&entry).is_empty());
    }
}
