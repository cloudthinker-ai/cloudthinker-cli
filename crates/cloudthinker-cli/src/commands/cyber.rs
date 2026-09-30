//! `cloudthinker cyber` — drive a local Cyber pentest run for one App: launch a
//! run that executes on this machine, bind this session to it, feed evidence,
//! and settle it. CloudThinker stays canonical for the App, the run, findings,
//! and the report; the cloud worker never touches a local run.

use std::path::Path;
use std::time::{Duration, Instant};

use base64::Engine;
use cloudthinker_client::{
    CtError, CtResult, CyberApp, CyberIntensity, CyberRun, EvidenceFile, FindingFilter,
    while_run_running,
};
use uuid::Uuid;

use crate::engine::exit::{self, ExitCode};
use crate::engine::output;

use super::build_client;

pub struct RunOpenOptions<'a> {
    pub app_ref: Option<&'a str>,
    pub app_name: Option<&'a str>,
    pub target: Option<&'a str>,
    pub api_coverage: bool,
    pub intensity: Option<CyberIntensity>,
    pub conversation_id: Option<Uuid>,
    pub scope: RunScopeOptions,
    pub json: bool,
}

pub struct IngestOptions<'a> {
    pub plan_id: &'a str,
    pub rows: &'a [String],
    pub status: &'a str,
    pub reason: &'a str,
    pub evidence: &'a str,
    pub worker: &'a str,
    pub json: bool,
}

#[derive(Default)]
pub struct RunScopeOptions {
    pub include: Vec<String>,
    pub exclude: Vec<String>,
}

impl RunScopeOptions {
    fn into_api_scope(
        self,
    ) -> Result<Option<cloudthinker_client::worker_types::ScopeSpec>, String> {
        if self.include.is_empty() && self.exclude.is_empty() {
            return Ok(None);
        }
        let include = self
            .include
            .into_iter()
            .map(|value| {
                value
                    .parse()
                    .map_err(|error| format!("invalid --include scope pattern `{value}`: {error}"))
            })
            .collect::<Result<Vec<cloudthinker_client::worker_types::IncludeItem>, _>>()?;
        let exclude = self
            .exclude
            .into_iter()
            .map(|value| {
                value
                    .parse()
                    .map_err(|error| format!("invalid --exclude scope pattern `{value}`: {error}"))
            })
            .collect::<Result<Vec<cloudthinker_client::worker_types::ExcludeItem>, _>>()?;
        Ok(Some(cloudthinker_client::worker_types::ScopeSpec {
            include,
            exclude,
        }))
    }
}

pub async fn list_apps(base_url: &str, workspace: Option<&str>, json: bool) -> ExitCode {
    let client = match build_client(base_url, workspace) {
        Ok(client) => client,
        Err(err) => return exit::report(&err),
    };
    match client.cyber_list_apps().await {
        Ok(apps) => {
            let result = if json {
                output::emit_json(&apps)
            } else {
                output::print_cyber_apps(&apps)
            };
            finish(result)
        }
        Err(err) => exit::report(&err),
    }
}

pub async fn list_domains(base_url: &str, workspace: Option<&str>, json: bool) -> ExitCode {
    let client = match build_client(base_url, workspace) {
        Ok(client) => client,
        Err(err) => return exit::report(&err),
    };
    match client.cyber_list_domains().await {
        Ok(domains) => {
            let result = if json {
                output::emit_json(&domains)
            } else {
                output::print_cyber_domains(&domains)
            };
            finish(result)
        }
        Err(err) => exit::report(&err),
    }
}

pub async fn create_domain(
    base_url: &str,
    workspace: Option<&str>,
    domain: &str,
    json: bool,
) -> ExitCode {
    let client = match build_client(base_url, workspace) {
        Ok(client) => client,
        Err(err) => return exit::report(&err),
    };
    match client.cyber_create_domain(domain).await {
        Ok(value) => {
            let result = if json {
                output::emit_json(&value)
            } else {
                output::print_lines(&[format!(
                    "{}  {}  {}",
                    value.domain_id, value.status, value.domain
                )])
            };
            finish(result)
        }
        Err(err) => exit::report(&err),
    }
}

pub async fn check_domain(
    base_url: &str,
    workspace: Option<&str>,
    domain_id: Uuid,
    json: bool,
) -> ExitCode {
    let client = match build_client(base_url, workspace) {
        Ok(client) => client,
        Err(err) => return exit::report(&err),
    };
    match client.cyber_check_domain(domain_id).await {
        Ok(value) => {
            let result = if json {
                output::emit_json(&value)
            } else {
                output::print_lines(&[format!(
                    "{}  {}  {}",
                    value.domain_id, value.status, value.domain
                )])
            };
            finish(result)
        }
        Err(err) => exit::report(&err),
    }
}

pub async fn open_run(
    base_url: &str,
    workspace: Option<&str>,
    options: RunOpenOptions<'_>,
) -> ExitCode {
    let run_scope = match options.scope.into_api_scope() {
        Ok(scope) => scope,
        Err(error) => {
            output::eprintln_error(&error);
            return ExitCode::Usage;
        }
    };
    let client = match build_client(base_url, workspace) {
        Ok(client) => client,
        Err(err) => return exit::report(&err),
    };
    let apps = match client.cyber_list_apps().await {
        Ok(apps) => apps,
        Err(err) => return exit::report(&err),
    };
    // `--app` wins over the saved default so a one-off run never edits
    // the repo's config; the config `app` makes the next run one sentence again.
    let config = super::cyber_config::resolved_values();
    let app_ref = options
        .app_ref
        .map(str::to_string)
        .or_else(|| config.get("app").cloned());
    let selected = app_ref
        .as_deref()
        .and_then(|reference| resolve_app(&apps, reference));
    if let Some(reference) = missing_app_reference(app_ref.as_deref(), selected.is_some()) {
        output::eprintln_error(&format!(
            "App `{}` was not found. Check `cyber app ls` or select a valid App.",
            output::terminal_text(reference)
        ));
        return ExitCode::Usage;
    }
    if let (Some(app), Some(target)) = (&selected, options.target)
        && !app_target_matches(app, target)
    {
        output::eprintln_error(&format!(
            "App `{}` targets `{}`; the requested target is `{}`. Select the matching App or create an App for the requested target.",
            output::terminal_text(&app.name),
            output::terminal_text(&app.target_ref),
            output::terminal_text(target)
        ));
        return ExitCode::Usage;
    }
    // `created` gates the auto-complete below. The CLI completes only an App it
    // just created with an explicit `--target`, because that intent already
    // carries the target and coverage the setup review would ask for. A
    // pre-existing draft the user selected may be mid-setup in the UI on
    // purpose, so it keeps the "complete it in Cyber" message.
    let (app, created) = match selected {
        Some(app) => (app, false),
        None if options.app_name.is_some() && options.target.is_some() => {
            let target = options.target.unwrap_or_default();
            match client
                .cyber_create_app(
                    options.app_name.unwrap_or_default(),
                    target,
                    options.api_coverage,
                )
                .await
            {
                Ok(app) => (app, true),
                Err(err) => return exit::report(&err),
            }
        }
        None => {
            output::eprintln_error(
                "App setup is incomplete. Use `cyber app ls`, set `cyber config set app <id>`, or provide `cyber run open --name NAME --target TARGET`.",
            );
            return ExitCode::Usage;
        }
    };
    let app = if app.setup_status != "complete" {
        if !created {
            output::eprintln_error(&format!(
                "App `{}` is still in setup ({}). Complete the App setup in Cyber, then retry.",
                app.name, app.setup_status
            ));
            return ExitCode::Usage;
        }
        output::progress(&format!("Completing setup for App `{}`", app.name));
        match client.cyber_complete_app_setup(app.app_id).await {
            Ok(updated) => updated,
            Err(err) => return exit::report(&err),
        }
    } else {
        app
    };
    let verification = match client.cyber_domain_verification(app.app_id).await {
        Ok(value) => value,
        Err(err) => return exit::report(&err),
    };
    // Launch only when the target needs no proof (a loopback or private IP in a
    // local run) or the proof is verified. `unclaimed`, `pending`, and
    // `failed` are each unresolved and must block, so the CLI never lets through
    // a launch the backend will refuse. The strings are the API enum's Display
    // values, not a guessed `unverified` that matches nothing.
    let status = verification.status.to_string();
    if status != "not_required" && status != "verified" {
        output::eprintln_error(&format!(
            "Domain proof is `{status}` for `{}`. Claim and verify the domain, or target a loopback IP for a local run, before launching.",
            app.target_ref
        ));
        return ExitCode::Usage;
    }
    let mut missing = missing_credentials();
    if options.conversation_id.is_none() {
        missing.push(
            "conversation_id (run inside `cloudthinker agent` or pass --conversation-id)"
                .to_string(),
        );
    }
    if !missing.is_empty() {
        output::eprintln_error(&format!(
            "Missing local Cyber facts: {}. Set them once in `cyber config` or provide the session value, then retry.",
            missing.join(", ")
        ));
        return ExitCode::Usage;
    }
    let Some(conversation_id) = options.conversation_id else {
        return ExitCode::Usage;
    };
    // Save the launch-ready App so the next run in this repo needs no `--app`.
    // A config-write failure must not fail a valid launch; warn and proceed.
    let app_id = app.app_id.to_string();
    if config.get("app") != Some(&app_id)
        && let Err(err) = super::cyber_config::set_project_key("app", &app_id)
    {
        output::progress(&format!(
            "warning: could not save the App to .cloudthinker/config.toml: {err}"
        ));
    }
    let (mode, _reason) = super::cyber_config::derived_mode();
    let run = match client
        .cyber_launch_local_run(app.app_id, Some(mode), options.intensity, run_scope)
        .await
    {
        Ok(run) => run,
        Err(err) => return exit::report(&err),
    };
    match client.cyber_bind_run(run.run_id, conversation_id).await {
        Ok(mut brief) => {
            if let Err(error) = attach_local_workspace(&mut brief) {
                output::eprintln_error(&format!(
                    "run {} is bound; rerun `cyber run bind` to restore its local workspace: {error}",
                    brief.run_id
                ));
                return ExitCode::JobFailed;
            }
            let result = if options.json {
                output::emit_json(&brief)
            } else {
                output::print_run_brief(&brief)
            };
            finish(result)
        }
        Err(err) => {
            if let Err(cancel_err) = client.cyber_cancel(run.run_id).await {
                output::progress(&format!(
                    "warning: could not cancel unbound run {}: {cancel_err}",
                    run.run_id
                ));
            }
            exit::report(&err)
        }
    }
}

fn credential_preflight() -> Option<ExitCode> {
    let missing = missing_credentials();
    if missing.is_empty() {
        return None;
    }
    output::eprintln_error(&format!(
        "Configured local Cyber credentials are missing: {}. Set all listed environment variables or clear their config entries; local Cyber refuses to degrade to black-box mode.",
        missing.join(", ")
    ));
    Some(ExitCode::Usage)
}

fn missing_credentials() -> Vec<String> {
    super::cyber_config::missing_secret_env_vars()
}

fn resolve_app(apps: &[CyberApp], reference: &str) -> Option<CyberApp> {
    if let Ok(id) = reference.parse::<Uuid>() {
        return apps.iter().find(|app| app.app_id == id).cloned();
    }
    apps.iter().find(|app| app.name == reference).cloned()
}

fn app_target_matches(app: &CyberApp, target: &str) -> bool {
    comparable_target_ref(&app.target_ref) == comparable_target_ref(target)
}

fn missing_app_reference(reference: Option<&str>, selected: bool) -> Option<&str> {
    if selected { None } else { reference }
}

fn comparable_target_ref(target: &str) -> String {
    let target = target.trim();
    let Some((scheme, rest)) = target.split_once("://") else {
        return target.to_string();
    };
    let boundary = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, suffix) = rest.split_at(boundary);
    let suffix_boundary = suffix.find(['?', '#']).unwrap_or(suffix.len());
    let (path, query_or_fragment) = suffix.split_at(suffix_boundary);
    let path = path.trim_end_matches('/');
    format!("{scheme}://{authority}{path}{query_or_fragment}")
}

pub async fn watch(
    base_url: &str,
    workspace: Option<&str>,
    run_id: Uuid,
    timeout_secs: u64,
    json: bool,
) -> ExitCode {
    let client = match build_client(base_url, workspace) {
        Ok(client) => client,
        Err(err) => return exit::report(&err),
    };
    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
    loop {
        let run = match client.cyber_get_run(run_id).await {
            Ok(run) => run,
            Err(err) => return exit::report(&err),
        };
        if run.is_terminal() {
            return render_run(&run, json);
        }
        if Instant::now() >= deadline {
            output::eprintln_error(&format!(
                "Timed out. The run continues server-side; resume with `cloudthinker cyber run status {run_id}`"
            ));
            return ExitCode::Timeout;
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

pub async fn cancel(base_url: &str, workspace: Option<&str>, run_id: Uuid, json: bool) -> ExitCode {
    let client = match build_client(base_url, workspace) {
        Ok(client) => client,
        Err(err) => return exit::report(&err),
    };
    match client.cyber_cancel(run_id).await {
        Ok(run) => render_run(&run, json),
        Err(err) => exit::report(&err),
    }
}

pub async fn findings_list(
    base_url: &str,
    workspace: Option<&str>,
    app_id: Uuid,
    page: u64,
    take: u64,
    filter: FindingFilter,
    json: bool,
) -> ExitCode {
    let client = match build_client(base_url, workspace) {
        Ok(client) => client,
        Err(err) => return exit::report(&err),
    };
    match client.cyber_list_findings(app_id, page, take, filter).await {
        Ok(findings) => {
            let result = if json {
                output::emit_json(&findings)
            } else {
                output::print_cyber_findings(&findings.data)
            };
            finish(result)
        }
        Err(err) => exit::report(&err),
    }
}

pub async fn finding_get(
    base_url: &str,
    workspace: Option<&str>,
    finding_id: Uuid,
    json: bool,
) -> ExitCode {
    let client = match build_client(base_url, workspace) {
        Ok(client) => client,
        Err(err) => return exit::report(&err),
    };
    match client.cyber_get_finding(finding_id).await {
        Ok(finding) => {
            let result = if json {
                output::emit_json(&finding)
            } else {
                output::print_cyber_finding(&finding)
            };
            finish(result)
        }
        Err(err) => exit::report(&err),
    }
}

pub async fn findings_export(
    base_url: &str,
    workspace: Option<&str>,
    app_id: Uuid,
    finding_id: Option<Uuid>,
    json: bool,
) -> ExitCode {
    let client = match build_client(base_url, workspace) {
        Ok(client) => client,
        Err(err) => return exit::report(&err),
    };
    let result = match finding_id {
        Some(id) => client.cyber_export_finding(id).await,
        None => client.cyber_export_findings(app_id).await,
    };
    match result {
        Ok(export) => {
            let rendered = if json {
                output::emit_json(&export)
            } else {
                output::print_cyber_export(&export)
            };
            finish(rendered)
        }
        Err(err) => exit::report(&err),
    }
}

/// Launch a local-execution-host run for the App. Exits 0 once the backend
/// holds a RUNNING row; the caller then `open`s, feeds evidence, and settles.
pub async fn run_launch(
    base_url: &str,
    workspace: Option<&str>,
    app_id: Uuid,
    intensity: Option<CyberIntensity>,
    scope: RunScopeOptions,
    json: bool,
) -> ExitCode {
    let run_scope = match scope.into_api_scope() {
        Ok(scope) => scope,
        Err(error) => {
            output::eprintln_error(&error);
            return ExitCode::Usage;
        }
    };
    if let Some(code) = credential_preflight() {
        return code;
    }
    let client = match build_client(base_url, workspace) {
        Ok(client) => client,
        Err(err) => return exit::report(&err),
    };
    let (mode, _reason) = super::cyber_config::derived_mode();
    match client
        .cyber_launch_local_run(app_id, Some(mode), intensity, run_scope)
        .await
    {
        Ok(run) => render_run(&run, json),
        Err(err) => exit::report(&err),
    }
}

/// Show a run's lifecycle, host, binding, and finding counts. A read: exits 0
/// on any successful fetch — the run's own result is advisory, not failure
/// (mirrors `review status` CA-RV-SP5).
pub async fn run_status(
    base_url: &str,
    workspace: Option<&str>,
    run_id: Uuid,
    json: bool,
) -> ExitCode {
    let client = match build_client(base_url, workspace) {
        Ok(client) => client,
        Err(err) => return exit::report(&err),
    };
    match client.cyber_get_run(run_id).await {
        Ok(run) => render_run(&run, json),
        Err(err) => exit::report(&err),
    }
}

/// Replay a run's mirrored local-agent transcript. A read: exits 0 on any
/// successful fetch; a run that never bound a conversation says so and fails,
/// because there is nothing to replay.
pub async fn run_session(
    base_url: &str,
    workspace: Option<&str>,
    run_id: Uuid,
    json: bool,
) -> ExitCode {
    let client = match build_client(base_url, workspace) {
        Ok(client) => client,
        Err(err) => return exit::report(&err),
    };
    let run = match client.cyber_get_run(run_id).await {
        Ok(run) => run,
        Err(err) => return exit::report(&err),
    };
    let Some(conversation_id) = run.conversation_id else {
        output::eprintln_error(
            "run has no bound conversation yet; run `cloudthinker cyber run bind` first",
        );
        return ExitCode::JobFailed;
    };
    let entries = match client.cyber_session_entries(conversation_id).await {
        Ok(entries) => entries,
        Err(err) => return exit::report(&err),
    };
    let result = if json {
        output::emit_json(&output::CyberSessionEnvelope {
            run_id: run.run_id,
            conversation_id: run.conversation_id,
            entries: &entries,
        })
    } else {
        output::print_cyber_session(&run, &entries)
    };
    finish(result)
}

pub async fn bind_run(
    base_url: &str,
    workspace: Option<&str>,
    run_id: Uuid,
    conversation_id: Uuid,
    json: bool,
) -> ExitCode {
    let client = match build_client(base_url, workspace) {
        Ok(client) => client,
        Err(err) => return exit::report(&err),
    };
    match client.cyber_bind_run(run_id, conversation_id).await {
        Ok(mut brief) => {
            if let Err(error) = attach_local_workspace(&mut brief) {
                output::eprintln_error(&format!(
                    "run {run_id} is bound; rerun `cyber run bind` to restore its local workspace: {error}"
                ));
                return ExitCode::JobFailed;
            }
            let result = if json {
                output::emit_json(&brief)
            } else {
                output::print_run_brief(&brief)
            };
            finish(result)
        }
        Err(err) => exit::report(&err),
    }
}

fn attach_local_workspace(brief: &mut cloudthinker_client::CyberRunBrief) -> Result<(), String> {
    let conversation_id = brief
        .conversation_id
        .ok_or_else(|| "the bound run brief did not include a conversation id".to_string())?;
    brief.local_workspace = Some(super::cyber_discovery::prepare_local_workspace(
        brief.run_id,
        brief.app_id,
        conversation_id,
    )?);
    Ok(())
}

/// Read the given files as text and upload them into the run's durable
/// evidence tree. The receipt names what was written and what was skipped.
pub async fn submit_evidence(
    base_url: &str,
    workspace: Option<&str>,
    run_id: Uuid,
    paths: &[String],
    json: bool,
) -> ExitCode {
    let mut files = Vec::with_capacity(paths.len());
    for path in paths {
        match read_evidence_file(Path::new(path)) {
            Ok(file) => files.push(file),
            Err(err) => {
                output::eprintln_error(&err);
                return ExitCode::Usage;
            }
        }
    }

    let client = match build_client(base_url, workspace) {
        Ok(client) => client,
        Err(err) => return exit::report(&err),
    };
    match client.cyber_submit_evidence(run_id, files).await {
        Ok(receipt) => {
            let result = if json {
                output::emit_json(&receipt)
            } else {
                output::print_evidence_receipt(&receipt)
            };
            finish(result)
        }
        Err(err) => exit::report(&err),
    }
}

/// Settle the run. Exit 0 when the finalize CAS accepted this session's
/// claim; `settled: false` (another writer got there first, or the run already
/// ended) is a clean no-op, not a failure.
pub async fn settle(
    base_url: &str,
    workspace: Option<&str>,
    run_id: Uuid,
    failed: bool,
    message: Option<String>,
    json: bool,
) -> ExitCode {
    let client = match build_client(base_url, workspace) {
        Ok(client) => client,
        Err(err) => return exit::report(&err),
    };
    match client.cyber_settle(run_id, !failed, message).await {
        Ok(result) => {
            let rendered = if json {
                output::emit_json(&result)
            } else {
                output::print_settle_result(&result)
            };
            finish(rendered)
        }
        Err(err) => exit::report(&err),
    }
}

/// Print the backend-issued plan: the bounded to-do list for this run. The
/// backend owns the rules; this shows what it wants probed, never why.
pub async fn plan(base_url: &str, workspace: Option<&str>, run_id: Uuid, json: bool) -> ExitCode {
    let client = match build_client(base_url, workspace) {
        Ok(client) => client,
        Err(err) => return exit::report(&err),
    };
    match client.cyber_issue_work_plan(run_id).await {
        Ok(plan) => {
            let rendered = if json {
                output::emit_json(&plan)
            } else {
                output::print_work_plan(&plan)
            };
            finish(rendered)
        }
        Err(err) => exit::report(&err),
    }
}

/// Execute the backend's plan on this machine and report the observations back.
/// A row the plan marked non-executable is recorded as skipped with its reason;
/// a transport failure is recorded as blocked, never as a silent pass.
pub async fn scan(
    base_url: &str,
    workspace: Option<&str>,
    run_id: Uuid,
    rows: &[String],
    identity: Option<&str>,
    json: bool,
) -> ExitCode {
    use cloudthinker_client::{CoverageStatus, Observation};

    let client = match build_client(base_url, workspace) {
        Ok(client) => client,
        Err(err) => return exit::report(&err),
    };
    let plan = match client.cyber_issue_work_plan(run_id).await {
        Ok(plan) => plan,
        Err(err) => return exit::report(&err),
    };
    if !plan_has_rows(&plan.rows) {
        output::eprintln_error("The probe plan has no rows; there is nothing to scan.");
        return ExitCode::JobFailed;
    }
    let lane = match select_plan_rows(rows, &plan.rows) {
        Ok(lane) => lane,
        Err(unknown_rows) => {
            output::eprintln_error(&format!(
                "Unknown probe plan row(s): {}. Use `cyber probe plan` to list valid rows.",
                unknown_rows.join(", ")
            ));
            return ExitCode::Usage;
        }
    };
    // A scout owns one lane, so it probes only its assigned rows. An empty set
    // means the whole plan, which keeps the single-worker path unchanged.
    let worker = "cloudthinker-cli".to_string();
    // Resolve the configured identity once; every probe sends it so an
    // authenticated run reaches authenticated endpoints instead of collecting
    // 401s. No identity means an honest anonymous (black-box) probe.
    let auth_header = match super::cyber_config::probe_auth_header(identity) {
        Ok(header) => header,
        Err(error) => {
            output::eprintln_error(&error);
            return ExitCode::Usage;
        }
    };
    let observations = match while_run_running(&client, run_id, async {
        let mut observations: Vec<Observation> = Vec::with_capacity(plan.rows.len());
        for row in &plan.rows {
            if !lane.is_empty() && !lane.contains(row.row_id.as_str()) {
                continue;
            }
            if !row.executable {
                observations.push(Observation {
                    row_id: row.row_id.clone(),
                    status: CoverageStatus::SkippedWithReason,
                    reason: if row.note.is_empty() {
                        "not executable".to_string()
                    } else {
                        row.note.clone()
                    },
                    evidence_ref: String::new(),
                    worker: worker.clone(),
                });
                continue;
            }
            let header = auth_header
                .as_ref()
                .map(|(name, value)| (name.as_str(), value.as_str()));
            let probe = client.cyber_probe(&row.method, &row.url, header).await?;
            observations.push(probe.to_observation(&row.row_id, &worker, &row.url));
        }
        Ok::<_, CtError>(observations)
    })
    .await
    {
        Ok(observations) => observations,
        Err(err) => return exit::report(&err),
    };
    match client
        .cyber_ingest_observations(run_id, &plan.plan_id, observations)
        .await
    {
        Ok(report) => {
            let rendered = if json {
                output::emit_json(&report)
            } else {
                output::print_coverage_report(&report)
            };
            finish(rendered)
        }
        Err(err) => exit::report(&err),
    }
}

fn plan_has_rows(rows: &[cloudthinker_client::PlanCheck]) -> bool {
    !rows.is_empty()
}

fn select_plan_rows<'a>(
    requested: &'a [String],
    plan_rows: &'a [cloudthinker_client::PlanCheck],
) -> Result<std::collections::HashSet<&'a str>, Vec<&'a str>> {
    let available: std::collections::HashSet<&str> =
        plan_rows.iter().map(|row| row.row_id.as_str()).collect();
    let unknown: Vec<&str> = requested
        .iter()
        .map(String::as_str)
        .filter(|row| !available.contains(row))
        .collect();
    if !unknown.is_empty() {
        return Err(unknown);
    }
    Ok(requested.iter().map(String::as_str).collect())
}

/// Read this run's attack-surface overview. An agent reads it to author the
/// themes it partitions the plan by before it fans scouts out.
pub async fn surface(
    base_url: &str,
    workspace: Option<&str>,
    run_id: Uuid,
    json: bool,
) -> ExitCode {
    let client = match build_client(base_url, workspace) {
        Ok(client) => client,
        Err(err) => return exit::report(&err),
    };
    match client.cyber_surface(run_id).await {
        Ok(surface) => {
            let rendered = if json {
                output::emit_json(&surface)
            } else {
                output::print_surface(&surface)
            };
            finish(rendered)
        }
        Err(err) => exit::report(&err),
    }
}

/// Carve the plan into theme lanes for fan-out. `--themes` is a JSON object or
/// `@path` to one; omit it to read the surface alone. The backend owns the
/// partition rule, so a lane's row set is authoritative for its scout.
pub async fn partition(
    base_url: &str,
    workspace: Option<&str>,
    run_id: Uuid,
    themes: Option<&str>,
    max_rows_per_shard: Option<i64>,
    only_status: Option<String>,
    json: bool,
) -> ExitCode {
    let client = match build_client(base_url, workspace) {
        Ok(client) => client,
        Err(err) => return exit::report(&err),
    };
    let themes_json = match themes {
        Some(raw) => match read_themes_arg(raw) {
            Ok(text) => Some(text),
            Err(err) => return exit::report(&err),
        },
        None => None,
    };
    match client
        .cyber_partition(
            run_id,
            themes_json.as_deref(),
            max_rows_per_shard,
            only_status,
        )
        .await
    {
        Ok(partition) => {
            let rendered = if json {
                output::emit_json(&partition)
            } else {
                output::print_partition(&partition)
            };
            finish(rendered)
        }
        Err(err) => exit::report(&err),
    }
}

/// Report a scout's reasoned observation for one or more plan rows. Each row a
/// scout examines ends by its own hand: covered, a candidate finding, blocked,
/// or skipped with a reason. Disjoint lanes let scouts ingest concurrently.
pub async fn ingest(
    base_url: &str,
    workspace: Option<&str>,
    run_id: Uuid,
    options: IngestOptions<'_>,
) -> ExitCode {
    use cloudthinker_client::Observation;

    let IngestOptions {
        plan_id,
        rows,
        status,
        reason,
        evidence,
        worker,
        json,
    } = options;
    let status = match validate_ingest(status, rows, reason) {
        Ok(status) => status,
        Err(error) => return exit::report(&CtError::Usage(error)),
    };
    let client = match build_client(base_url, workspace) {
        Ok(client) => client,
        Err(err) => return exit::report(&err),
    };
    let observations: Vec<Observation> = rows
        .iter()
        .map(|row_id| Observation {
            row_id: row_id.clone(),
            status,
            reason: reason.to_string(),
            evidence_ref: evidence.to_string(),
            worker: worker.to_string(),
        })
        .collect();
    match client
        .cyber_ingest_observations(run_id, plan_id, observations)
        .await
    {
        Ok(report) => {
            let rendered = if json {
                output::emit_json(&report)
            } else {
                output::print_coverage_report(&report)
            };
            finish(rendered)
        }
        Err(err) => exit::report(&err),
    }
}

fn validate_ingest(
    status: &str,
    rows: &[String],
    reason: &str,
) -> Result<cloudthinker_client::CoverageStatus, String> {
    let status_label = status;
    let Some(status) = cloudthinker_client::CoverageStatus::from_wire(status_label) else {
        return Err(format!("--status {status_label} is not a coverage status"));
    };
    if rows.is_empty() {
        return Err("at least one --row is required".to_string());
    }
    if status.requires_reason() && reason.trim().is_empty() {
        return Err(format!("--reason is required when --status {status_label}"));
    }
    Ok(status)
}

/// Resolve `--themes`: a literal JSON object, or `@path` to a JSON file.
fn read_themes_arg(raw: &str) -> CtResult<String> {
    match raw.strip_prefix('@') {
        Some(path) => std::fs::read_to_string(path)
            .map_err(|e| CtError::Usage(format!("--themes @{path}: {e}"))),
        None => Ok(raw.to_string()),
    }
}

/// Read the run's coverage ledger and completion gate.
pub async fn coverage(
    base_url: &str,
    workspace: Option<&str>,
    run_id: Uuid,
    json: bool,
) -> ExitCode {
    let client = match build_client(base_url, workspace) {
        Ok(client) => client,
        Err(err) => return exit::report(&err),
    };
    match client.cyber_get_coverage(run_id).await {
        Ok(report) => {
            let rendered = if json {
                output::emit_json(&report)
            } else {
                output::print_coverage_report(&report)
            };
            finish(rendered)
        }
        Err(err) => exit::report(&err),
    }
}

fn render_run(run: &CyberRun, json: bool) -> ExitCode {
    let result = if json {
        output::emit_json(run)
    } else {
        output::print_cyber_run(run)
    };
    finish(result)
}

fn finish(result: Result<(), String>) -> ExitCode {
    if let Err(err) = result {
        output::eprintln_error(&err);
        return ExitCode::JobFailed;
    }
    ExitCode::Ok
}

/// Read one bounded text or binary artifact; a directory or unreadable path is
/// a usage error naming the path.
fn read_evidence_file(path: &Path) -> Result<EvidenceFile, String> {
    if path.is_dir() {
        return Err(format!(
            "{} is a directory; evidence files must be regular files",
            path.display()
        ));
    }
    let bytes = std::fs::read(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    if bytes.len() > 16_000_000 {
        return Err(format!(
            "{} exceeds the 16 MiB evidence limit",
            path.display()
        ));
    }
    let relative = path
        .strip_prefix(std::env::current_dir().unwrap_or_else(|_| Path::new(".").to_path_buf()))
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/");
    let relative = relative.strip_prefix("./").unwrap_or(&relative).to_string();
    if let Ok(content) = String::from_utf8(bytes.clone()) {
        return Ok(EvidenceFile {
            path: relative,
            content: Some(content),
            content_base64: None,
            mime_type: Some("text/plain".to_string()),
            size_bytes: bytes.len() as u64,
        });
    }
    Ok(EvidenceFile {
        path: relative,
        content: None,
        content_base64: Some(base64::engine::general_purpose::STANDARD.encode(bytes.clone())),
        mime_type: mime_type(path),
        size_bytes: bytes.len() as u64,
    })
}

fn mime_type(path: &Path) -> Option<String> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    let mime = match extension.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "pcap" => "application/vnd.tcpdump.pcap",
        "zip" => "application/zip",
        "gz" => "application/gzip",
        "pdf" => "application/pdf",
        _ => "application/octet-stream",
    };
    Some(mime.to_string())
}

#[cfg(test)]
mod tests {
    use super::{
        RunScopeOptions, app_target_matches, missing_app_reference, plan_has_rows,
        select_plan_rows, validate_ingest,
    };
    use cloudthinker_client::{CyberApp, PlanCheck};
    use uuid::Uuid;

    #[test]
    fn empty_run_scope_is_omitted_and_patterns_map_to_the_wire_scope() {
        assert!(
            RunScopeOptions::default()
                .into_api_scope()
                .unwrap()
                .is_none()
        );
        let scope = RunScopeOptions {
            include: vec!["/health".into()],
            exclude: vec!["/admin".into()],
        }
        .into_api_scope()
        .unwrap()
        .unwrap();
        assert_eq!(&*scope.include[0], "/health");
        assert_eq!(&*scope.exclude[0], "/admin");
    }

    #[test]
    fn an_explicit_target_must_match_the_selected_app() {
        let app = CyberApp {
            app_id: Uuid::from_u128(1),
            name: "CT32".into(),
            target_ref: "http://ct32.localhost:8088".into(),
            setup_status: "complete".into(),
            domain_status: "not_required".into(),
            domain_id: None,
            open_findings_count: 0,
        };
        assert!(app_target_matches(&app, " http://ct32.localhost:8088/ "));
        assert!(!app_target_matches(&app, "http://127.0.0.1:8088"));
        let path_app = CyberApp {
            target_ref: "http://ct32.localhost:8088/Security".into(),
            ..app
        };
        let normalized_path_app = CyberApp {
            target_ref: "http://ct32.localhost:8088/api/v1/utils/health-check".into(),
            ..path_app.clone()
        };
        assert!(app_target_matches(
            &normalized_path_app,
            "http://ct32.localhost:8088/api/v1/utils/health-check/"
        ));
        assert!(!app_target_matches(
            &normalized_path_app,
            "http://ct32.localhost:8089/api/v1/utils/health-check/"
        ));
        assert!(!app_target_matches(
            &normalized_path_app,
            "http://ct32.localhost:8088/api/v1/utils/Health-check/"
        ));
        let query_app = CyberApp {
            target_ref: "http://ct32.localhost:8088/api/v1/utils/health-check?next=/".into(),
            ..normalized_path_app.clone()
        };
        assert!(app_target_matches(
            &query_app,
            "http://ct32.localhost:8088/api/v1/utils/health-check/?next=/"
        ));
        assert!(!app_target_matches(
            &query_app,
            "http://ct32.localhost:8088/api/v1/utils/health-check?next="
        ));
        let fragment_app = CyberApp {
            target_ref: "http://ct32.localhost:8088/api/v1/utils/health-check#section".into(),
            ..normalized_path_app.clone()
        };
        assert!(app_target_matches(
            &fragment_app,
            "http://ct32.localhost:8088/api/v1/utils/health-check/#section"
        ));
        assert!(!app_target_matches(
            &path_app,
            "http://ct32.localhost:8088/security"
        ));
    }

    #[test]
    fn unresolved_app_references_do_not_fall_through_to_app_creation() {
        assert_eq!(
            missing_app_reference(Some("missing-app"), false),
            Some("missing-app")
        );
        assert_eq!(missing_app_reference(Some("existing-app"), true), None);
        assert_eq!(missing_app_reference(None, false), None);
    }

    #[test]
    fn an_empty_work_plan_cannot_start_a_scan() {
        assert!(!plan_has_rows(&[]));
        assert!(plan_has_rows(&[PlanCheck {
            row_id: "row-a".into(),
            asset_type: "endpoint".into(),
            method: "GET".into(),
            url: "https://example.test/".into(),
            locator: "GET /".into(),
            executable: true,
            note: String::new(),
        }]));
    }

    #[test]
    fn probe_row_selection_rejects_unknown_rows_before_scanning() {
        let plan = vec![PlanCheck {
            row_id: "row-a".into(),
            asset_type: "endpoint".into(),
            method: "GET".into(),
            url: "https://example.test/".into(),
            locator: "GET /".into(),
            executable: true,
            note: String::new(),
        }];
        assert_eq!(
            select_plan_rows(&["missing".into()], &plan).unwrap_err(),
            vec!["missing"]
        );
        assert_eq!(select_plan_rows(&["row-a".into()], &plan).unwrap().len(), 1);
    }

    #[test]
    fn probe_ingest_validates_required_reason_before_client_creation() {
        assert_eq!(
            validate_ingest("candidate", &["row-a".into()], "  ").unwrap_err(),
            "--reason is required when --status candidate"
        );
        assert!(validate_ingest("covered", &["row-a".into()], "").is_ok());
    }
}
