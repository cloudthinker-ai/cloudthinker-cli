use std::time::Duration;

use cloudthinker_client::{
    CtClient, CtError, LocalReviewContext, LocalReviewContextRequest, LocalReviewFindingIn,
    LocalReviewSeverity, LocalReviewUpload, LoupeReadiness, LoupeReadinessCheckName,
    LoupeReadinessState, LoupeSetupScope, ReviewSeverityCounts, ReviewStatus, ReviewView,
    parse_mr_url, valid_setup_web_url,
    worker_types::{ExecutorChoiceKind, ExecutorScope},
};
use serde::Serialize;

use crate::engine::exit::{self, ExitCode};
use crate::engine::local_review::{
    self, AgentAnswer, FindingSeverity, LocalFinding, LocalReviewResult, RepositoryIdentity,
    ReviewScope, ReviewSnapshot, ServerGuidance,
};
use crate::engine::output::{self, ReviewEnvelope};
use crate::engine::watch::{Poll, WatchConfig, watch};

use super::build_client;

pub async fn run_setup(base_url: &str, workspace: Option<&str>, worker: Option<&str>) -> ExitCode {
    let client = match setup_client(base_url, workspace).await {
        Ok(client) => client,
        Err(error) => return exit::report(&error),
    };
    let target = match setup_worker(&client, worker).await {
        Ok(target) => target,
        Err(error) => return exit::report(&error),
    };
    let conversation = match client.start_loupe_setup_conversation(target).await {
        Ok(conversation) => conversation,
        Err(error) => return exit::report(&error),
    };
    if !valid_setup_web_url(&conversation.web_url) {
        output::eprintln_error(
            "The frontend link is unavailable. Ask an administrator to configure FRONTEND_HOST, then retry.",
        );
        return ExitCode::JobFailed;
    }
    let conversation_url = output::terminal_text(&conversation.web_url);
    output::progress(&format!("Setup conversation: {conversation_url}"));
    match output::print_text(&format!(
        "Continue setup and answer any questions: {conversation_url}\n"
    )) {
        Ok(()) => ExitCode::Ok,
        Err(error) => exit::report(&CtError::Transport(error)),
    }
}

async fn setup_client(base_url: &str, workspace: Option<&str>) -> Result<CtClient, CtError> {
    if !super::env_token_is_set() || workspace.is_none() {
        return build_client(base_url, workspace);
    }
    let expected = workspace
        .and_then(|value| value.parse::<uuid::Uuid>().ok())
        .ok_or_else(|| {
            CtError::Usage("With CLOUDTHINKER_TOKEN, --workspace must be a workspace ID.".into())
        })?;
    let client = build_client(base_url, None)?;
    let identity = client.whoami().await?;
    if identity.workspace_id != expected {
        return Err(CtError::Usage(
            "CLOUDTHINKER_TOKEN belongs to a different workspace. Use the matching workspace or token."
                .into(),
        ));
    }
    Ok(client)
}

async fn setup_worker(client: &CtClient, selector: Option<&str>) -> Result<uuid::Uuid, CtError> {
    if let Some(id) = selector.and_then(|value| value.parse::<uuid::Uuid>().ok()) {
        return Ok(id);
    }
    let choices = client.list_outposts().await?;
    let shared: Vec<_> = choices
        .iter()
        .filter(|choice| {
            choice.kind == ExecutorChoiceKind::Outpost
                && choice.scope == ExecutorScope::Workspace
                && choice.target_id.is_some()
        })
        .filter(|choice| selector.is_none_or(|name| choice.name == name))
        .collect();
    match shared.as_slice() {
        [choice] => choice.target_id.ok_or_else(|| CtError::Usage("Worker has no ID".into())),
        [] => Err(CtError::Usage(
            "No matching shared environment. List environments with `cloudthinker worker outpost ls`.".into(),
        )),
        _ => Err(CtError::Usage(
            "Choose a shared environment with --worker <name-or-id>; list them with `cloudthinker worker outpost ls`.".into(),
        )),
    }
}

const DOCTOR_LIMIT: &str = "These are prerequisite checks only. App startup, sign-in, browser journeys, and base comparison were not verified.";

#[derive(Serialize)]
struct DoctorReport {
    prerequisites: LoupeReadiness,
    loupe_readiness: String,
    coverage: &'static str,
}

pub struct DoctorOptions<'a> {
    pub base_url: &'a str,
    pub workspace: Option<&'a str>,
    pub json: bool,
    pub worker: Option<uuid::Uuid>,
    pub connection: Option<uuid::Uuid>,
    pub provider: Option<&'a str>,
    pub repository: Option<&'a str>,
}

fn check_name(name: LoupeReadinessCheckName) -> &'static str {
    match name {
        LoupeReadinessCheckName::Checkout => "App checkout",
        LoupeReadinessCheckName::Node => "Node.js",
        LoupeReadinessCheckName::Bun => "Bun",
        LoupeReadinessCheckName::Playwright => "Playwright",
        LoupeReadinessCheckName::Chromium => "Chromium",
        LoupeReadinessCheckName::SignIn => "Saved sign-in file",
        LoupeReadinessCheckName::DevEnv => "Development environment file",
        LoupeReadinessCheckName::AppRuntime => "App runtime",
        LoupeReadinessCheckName::Sandbox => "Worker sandbox",
    }
}

fn state_label(state: LoupeReadinessState) -> &'static str {
    match state {
        LoupeReadinessState::Off => "No Worker is selected",
        LoupeReadinessState::Ready => "Required prerequisites passed",
        LoupeReadinessState::NeedsSetup => "Worker needs setup",
        LoupeReadinessState::Busy => "Worker is busy",
        LoupeReadinessState::Offline => "Worker is offline",
        LoupeReadinessState::NoAnswer => "Worker did not answer",
        LoupeReadinessState::Unavailable => "Worker is unavailable",
    }
}

fn setup_state_label(state: cloudthinker_client::LoupeSetupState) -> &'static str {
    match state {
        cloudthinker_client::LoupeSetupState::NotChecked => "not checked",
        cloudthinker_client::LoupeSetupState::Partial => "partial",
        cloudthinker_client::LoupeSetupState::Full => "full",
    }
}

pub async fn run_doctor(options: DoctorOptions<'_>) -> ExitCode {
    let client = match build_client(options.base_url, options.workspace) {
        Ok(client) => client,
        Err(error) => return exit::report(&error),
    };
    let scope = match (
        options.worker,
        options.connection,
        options.provider,
        options.repository,
    ) {
        (Some(worker), Some(connection), Some(provider), Some(repository)) => {
            Some(LoupeSetupScope {
                worker,
                connection,
                provider,
                repository,
            })
        }
        (_, None, None, None) => None,
        _ => {
            output::eprintln_error(
                "Provide --worker, --connection, --provider and --repository together.",
            );
            return ExitCode::Usage;
        }
    };
    let prerequisites = match client
        .check_loupe_readiness(options.worker, scope.as_ref())
        .await
    {
        Ok(prerequisites) => prerequisites,
        Err(error) => return exit::report(&error),
    };
    let report = DoctorReport {
        loupe_readiness: prerequisites
            .observed
            .as_ref()
            .map_or_else(|| "not_checked".to_string(), |view| view.state.to_string()),
        coverage: if prerequisites.observed.is_some() {
            "Stored setup observations; app journeys were not rechecked by this command."
        } else {
            DOCTOR_LIMIT
        },
        prerequisites,
    };
    let printed = if options.json {
        output::emit_json(&report)
    } else {
        let mut lines = vec![format!(
            "Loupe prerequisites: {}",
            state_label(report.prerequisites.state)
        )];
        if let Some(repository) = &report.prerequisites.repository {
            lines.push(format!("Repository: {repository}"));
        }
        if let Some(detail) = &report.prerequisites.detail {
            lines.push(detail.to_owned());
        }
        for check in &report.prerequisites.checks {
            let result = if check.passed {
                "passed"
            } else if check.required {
                "needs setup"
            } else {
                "optional, missing"
            };
            let detail = check
                .detail
                .as_ref()
                .map(|detail| format!(" ({detail})"))
                .unwrap_or_default();
            lines.push(format!("{}: {result}{detail}", check_name(check.name)));
        }
        if let Some(observed) = &report.prerequisites.observed {
            lines.push(format!("Setup: {}", setup_state_label(observed.state)));
            lines.push(format!(
                "Enabled: {}",
                if observed.enabled { "yes" } else { "no" }
            ));
            if let Some(checked_at) = &observed.checked_at {
                lines.push(format!(
                    "Observed at: {} UTC",
                    checked_at
                        .with_timezone(&chrono::Utc)
                        .format("%Y-%m-%d %H:%M")
                ));
            }
            if observed.check_pending {
                lines.push("A new setup check is in progress.".to_owned());
            }
            lines.push(observed.detail.to_owned());
        }
        lines.push(report.coverage.to_owned());
        output::print_lines(&lines)
    };
    if let Err(error) = printed {
        output::eprintln_error(&error);
        return ExitCode::JobFailed;
    }
    if matches!(report.prerequisites.state, LoupeReadinessState::Ready)
        && report.prerequisites.observed.as_ref().is_some_and(|view| {
            matches!(view.state, cloudthinker_client::LoupeSetupState::Full)
                && view.enabled
                && view.generation.is_some()
                && !view.check_pending
        })
    {
        ExitCode::Ok
    } else {
        ExitCode::JobFailed
    }
}

pub struct LocalReviewOptions<'a> {
    pub base_url: &'a str,
    pub workspace: Option<&'a str>,
    pub base_ref: Option<&'a str>,
    pub json: bool,
    pub timeout_secs: u64,
    pub fail_on: Option<FailOn>,
    pub upload: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, clap::ValueEnum)]
pub enum FailOn {
    Low,
    Medium,
    High,
    Critical,
}

impl FailOn {
    fn of(severity: &FindingSeverity) -> Self {
        match severity {
            FindingSeverity::Critical => FailOn::Critical,
            FindingSeverity::High => FailOn::High,
            FindingSeverity::Medium => FailOn::Medium,
            FindingSeverity::Low => FailOn::Low,
        }
    }

    fn local_hits(self, findings: &[LocalFinding]) -> i64 {
        findings
            .iter()
            .filter(|finding| FailOn::of(&finding.severity) >= self)
            .count()
            .try_into()
            .unwrap_or(i64::MAX)
    }

    fn remote_hits(self, counts: &ReviewSeverityCounts) -> i64 {
        [
            (FailOn::Critical, counts.critical),
            (FailOn::High, counts.high),
            (FailOn::Medium, counts.medium),
            (FailOn::Low, counts.low),
        ]
        .into_iter()
        .filter(|(severity, _)| *severity >= self)
        .map(|(_, count)| count.max(0))
        .fold(0, i64::saturating_add)
    }

    fn name(self) -> &'static str {
        match self {
            FailOn::Low => "low",
            FailOn::Medium => "medium",
            FailOn::High => "high",
            FailOn::Critical => "critical",
        }
    }
}

fn threshold_code(fail_on: Option<FailOn>, hits: impl FnOnce(FailOn) -> i64) -> ExitCode {
    let Some(threshold) = fail_on else {
        return ExitCode::Ok;
    };
    match hits(threshold) {
        0 => ExitCode::Ok,
        count => {
            output::eprintln_error(&format!(
                "{count} finding(s) at {} severity or worse (--fail-on {})",
                threshold.name(),
                threshold.name()
            ));
            ExitCode::ReviewFindings
        }
    }
}

pub async fn run_local(options: LocalReviewOptions<'_>) -> ExitCode {
    let started = std::time::Instant::now();
    let cwd = match std::env::current_dir() {
        Ok(cwd) => cwd,
        Err(error) => {
            output::eprintln_error(&format!(
                "could not determine the current directory: {error}"
            ));
            return ExitCode::JobFailed;
        }
    };
    let review_scope = ReviewScope {
        base_ref: options.base_ref.map(str::to_string),
    };
    let snapshot = match local_review::collect_snapshot(&cwd, &review_scope) {
        Ok(snapshot) => snapshot,
        Err(error) => {
            output::eprintln_error(&error.to_string());
            return if error.is_usage() {
                ExitCode::Usage
            } else {
                ExitCode::JobFailed
            };
        }
    };
    let agent = match super::agent::prepare_local_review(options.base_url, options.workspace).await
    {
        Ok(agent) => agent,
        Err(code) => return code,
    };
    let identity = local_review::repository_identity(&snapshot.repository);
    let client = build_client(options.base_url, options.workspace);
    let guidance = match (&client, &identity) {
        (Ok(client), Some(identity)) => fetch_guidance(client, identity, &snapshot).await,
        (Err(error), Some(_)) => {
            warn_guidance(&error.to_string());
            Guidance::Unavailable
        }
        (_, None) => Guidance::Unavailable,
    };
    let chunks = local_review::diff_chunks(&snapshot.diff);
    let scope = options.base_ref.map_or_else(
        || "the worktree delta against HEAD".to_string(),
        |base| format!("the merge-base with {base}"),
    );
    let parts = if chunks.len() > 1 {
        format!(" in {} parts", chunks.len())
    } else {
        String::new()
    };
    output::progress(&format!(
        "Reviewing {} changed file(s) from {scope}{parts} with a local read-only agent; CloudThinker provides inference.",
        snapshot.changed_files.len()
    ));
    let deadline = started + Duration::from_secs(options.timeout_secs);
    let mut answer = AgentAnswer::default();
    for (index, chunk) in chunks.iter().enumerate() {
        let part = (index + 1, chunks.len());
        let timeout = deadline.saturating_duration_since(std::time::Instant::now());
        if timeout.is_zero() {
            output::eprintln_error(&format!(
                "the review reached its {} s limit before part {} of {}; rerun with a longer --timeout",
                options.timeout_secs, part.0, part.1
            ));
            return ExitCode::JobFailed;
        }
        if chunks.len() > 1 {
            output::progress(&format!("Reviewing part {} of {}…", part.0, part.1));
        }
        let server = match &guidance {
            Guidance::Server(context) => Some(ServerGuidance {
                instructions: &context.instructions,
                context: &context.context,
            }),
            Guidance::Unknown | Guidance::Unavailable => None,
        };
        let prompt = local_review::build_review_prompt(&snapshot, chunk, part, server);
        match review_chunk(&agent, options.base_url, &snapshot, &prompt, deadline).await {
            Ok(parsed) => answer.merge(parsed),
            Err(code) => return code,
        }
    }
    let current = match local_review::collect_snapshot(&cwd, &review_scope) {
        Ok(snapshot) => snapshot,
        Err(error) => {
            output::eprintln_error(&error.to_string());
            return ExitCode::JobFailed;
        }
    };
    if current.base_sha != snapshot.base_sha
        || current.head_sha != snapshot.head_sha
        || current.diff != snapshot.diff
    {
        output::eprintln_error(
            "the review scope changed while the agent was working; rerun the review",
        );
        return ExitCode::JobFailed;
    }
    let mut result = local_review::validate_result(&snapshot, answer);
    if result.dropped_findings > 0 {
        output::progress(&format!(
            "Dropped {} finding(s) that did not point at a changed line or were incomplete.",
            result.dropped_findings
        ));
    }
    let known = !matches!(guidance, Guidance::Unknown);
    if let (true, true, Ok(client), Some(identity)) = (options.upload, known, &client, &identity) {
        result.uploaded = upload(client, identity, &snapshot, &result, started).await;
    }
    let rendered = if options.json {
        output::emit_json(&result)
    } else {
        output::print_local_review(&result)
    };
    if let Err(error) = rendered {
        output::eprintln_error(&error);
        return ExitCode::JobFailed;
    }
    threshold_code(options.fail_on, |threshold| {
        threshold.local_hits(&result.findings)
    })
}

async fn review_chunk(
    agent: &super::agent::LocalReviewAgent,
    base_url: &str,
    snapshot: &ReviewSnapshot,
    prompt: &str,
    deadline: std::time::Instant,
) -> Result<AgentAnswer, ExitCode> {
    let timeout = deadline.saturating_duration_since(std::time::Instant::now());
    let answer =
        super::agent::run_local_review(agent, base_url, &snapshot.repository, prompt, timeout)
            .await?;
    if let Ok(parsed) = local_review::parse_agent_answer(&answer) {
        return Ok(parsed);
    }
    output::warn("The review answer was not valid JSON; asking the agent once more.");
    let retry = local_review::retry_prompt(prompt);
    let timeout = deadline.saturating_duration_since(std::time::Instant::now());
    let answer =
        super::agent::run_local_review(agent, base_url, &snapshot.repository, &retry, timeout)
            .await?;
    local_review::parse_agent_answer(&answer).map_err(|error| {
        output::eprintln_error(&error);
        ExitCode::JobFailed
    })
}

enum Guidance {
    Server(LocalReviewContext),
    Unknown,
    Unavailable,
}

async fn fetch_guidance(
    client: &CtClient,
    identity: &RepositoryIdentity,
    snapshot: &ReviewSnapshot,
) -> Guidance {
    let request = LocalReviewContextRequest {
        remote_url: &identity.remote_url,
        branch: identity.branch.as_deref(),
        base_sha: &snapshot.base_sha,
        head_sha: &snapshot.head_sha,
        changed_paths: &snapshot.changed_files,
    };
    match client.local_review_context(&request).await {
        Ok(context) if context.repository_known => Guidance::Server(context),
        Ok(_) => {
            output::warn(
                "CloudThinker does not know this repository yet, so the review uses the built-in instructions and the findings stay on this machine.",
            );
            Guidance::Unknown
        }
        Err(error) => {
            warn_guidance(&error.to_string());
            Guidance::Unavailable
        }
    }
}

fn warn_guidance(reason: &str) {
    output::warn(&format!(
        "Could not load this repository's review guidance from CloudThinker ({reason}); the review uses the built-in instructions."
    ));
}

fn severity(severity: FindingSeverity) -> LocalReviewSeverity {
    match severity {
        FindingSeverity::Critical => LocalReviewSeverity::Critical,
        FindingSeverity::High => LocalReviewSeverity::High,
        FindingSeverity::Medium => LocalReviewSeverity::Medium,
        FindingSeverity::Low => LocalReviewSeverity::Low,
    }
}

async fn upload(
    client: &CtClient,
    identity: &RepositoryIdentity,
    snapshot: &ReviewSnapshot,
    result: &LocalReviewResult,
    started: std::time::Instant,
) -> bool {
    let line_texts = result
        .findings
        .iter()
        .map(|finding| local_review::line_text(&snapshot.repository, &finding.file, finding.line))
        .collect::<Vec<_>>();
    let findings = result
        .findings
        .iter()
        .zip(&line_texts)
        .map(|(finding, line_text)| LocalReviewFindingIn {
            severity: severity(finding.severity),
            file: &finding.file,
            line: finding.line,
            title: &finding.title,
            explanation: &finding.explanation,
            suggested_fix: &finding.suggested_fix,
            line_text,
        })
        .collect::<Vec<_>>();
    let upload = LocalReviewUpload {
        remote_url: &identity.remote_url,
        branch: identity.branch.as_deref(),
        base_sha: &snapshot.base_sha,
        head_sha: &snapshot.head_sha,
        duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        findings: &findings,
    };
    match client.upload_local_review(&upload).await {
        Ok(stored) if stored.repository_known && stored.id.is_some() => {
            output::progress("Sent to CloudThinker: the PR review will credit these findings.");
            true
        }
        Ok(_) => {
            output::warn(
                "CloudThinker does not know this repository yet, so these findings were not saved.",
            );
            false
        }
        Err(error) => {
            output::warn(&format!(
                "Could not send the findings to CloudThinker ({error}); they are printed here only."
            ));
            false
        }
    }
}

/// Show a review's current status. A read: exits 0 on any successful fetch —
/// the review's own status/verdict is advisory, not failure (CA-RV-SP5,
/// mirrors `chat status` CA-CLI-16).
pub async fn run_status(
    base_url: &str,
    workspace: Option<&str>,
    url: &str,
    json: bool,
) -> ExitCode {
    fetch_and_render(
        base_url,
        workspace,
        url,
        json,
        output::print_review_status_summary,
    )
    .await
}

/// List a review's findings, worst-severity first (CA-RV-2). Also a read
/// (CA-RV-SP5).
pub async fn run_findings(
    base_url: &str,
    workspace: Option<&str>,
    url: &str,
    json: bool,
) -> ExitCode {
    fetch_and_render(
        base_url,
        workspace,
        url,
        json,
        output::print_review_findings,
    )
    .await
}

/// Poll a review to a terminal state, printing the final verdict.
///
/// Exit 0 on any terminal outcome except `ReviewStatus::Failed` (exit 1,
/// CA-RV-3). A client-side deadline prints a resume hint and exits 4
/// (CA-RV-SP6); the review keeps running server-side.
pub async fn run_watch(
    base_url: &str,
    workspace: Option<&str>,
    url: &str,
    json: bool,
    timeout_secs: u64,
    fail_on: Option<FailOn>,
) -> ExitCode {
    let coords = match parse_mr_url(url) {
        Ok(coords) => coords,
        Err(err) => return exit::report(&err),
    };
    let client = match build_client(base_url, workspace) {
        Ok(client) => client,
        Err(err) => return exit::report(&err),
    };

    if !json {
        output::progress(&format!("Watching review for {url}…"));
    }

    let cfg = WatchConfig::for_run(Duration::from_secs(timeout_secs));
    let outcome = watch(
        async || {
            let view = client.lookup_review(&coords).await?;
            if view.status.is_terminal() {
                Ok(Poll::Terminal(view))
            } else {
                Ok(Poll::Pending)
            }
        },
        &cfg,
    )
    .await;

    match outcome {
        Ok(view) => finish_watch(&view, json, fail_on),
        Err(CtError::Timeout(_)) => {
            output::progress(&format!(
                "Timed out. The review continues server-side — resume with: cloudthinker review status {url}"
            ));
            ExitCode::Timeout
        }
        Err(err) => report_lookup_error(&err, url),
    }
}

/// Shared body for `status`/`findings`: parse the URL, fetch once, render.
async fn fetch_and_render(
    base_url: &str,
    workspace: Option<&str>,
    url: &str,
    json: bool,
    human: fn(&ReviewView) -> Result<(), String>,
) -> ExitCode {
    let coords = match parse_mr_url(url) {
        Ok(coords) => coords,
        Err(err) => return exit::report(&err),
    };
    let client = match build_client(base_url, workspace) {
        Ok(client) => client,
        Err(err) => return exit::report(&err),
    };

    match client.lookup_review(&coords).await {
        Ok(view) => render(&view, json, human),
        Err(err) => report_lookup_error(&err, url),
    }
}

fn render(view: &ReviewView, json: bool, human: fn(&ReviewView) -> Result<(), String>) -> ExitCode {
    let result = if json {
        output::emit_json(&ReviewEnvelope::from_view(view))
    } else {
        human(view)
    };
    if let Err(err) = result {
        output::eprintln_error(&err);
        return ExitCode::JobFailed;
    }
    ExitCode::Ok
}

fn finish_watch(view: &ReviewView, json: bool, fail_on: Option<FailOn>) -> ExitCode {
    let result = if json {
        output::emit_json(&ReviewEnvelope::from_view(view))
    } else {
        output::print_review_status_summary(view)
    };
    if let Err(err) = result {
        output::eprintln_error(&err);
        return ExitCode::JobFailed;
    }
    match view.status {
        ReviewStatus::Failed => ExitCode::JobFailed,
        _ => threshold_code(fail_on, |threshold| {
            threshold.remote_hits(&view.severity_counts)
        }),
    }
}

/// A 404 (unknown coordinates) gets a review-specific message; everything
/// else goes through the standard error → exit-code mapping (CA-RV-SP4,
/// CA-RV-SP2, CA-RV-SP3).
fn report_lookup_error(err: &CtError, url: &str) -> ExitCode {
    match err {
        CtError::Api { status: 404, .. } => {
            output::eprintln_error(&format!("no code review found for {url}"));
            ExitCode::JobFailed
        }
        other => exit::report(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_hits_saturates_instead_of_overflowing() {
        let counts = ReviewSeverityCounts {
            critical: i64::MAX,
            high: i64::MAX,
            medium: 1,
            low: -5,
        };
        assert_eq!(FailOn::Low.remote_hits(&counts), i64::MAX);
        assert_eq!(FailOn::Critical.remote_hits(&counts), i64::MAX);
    }
}
