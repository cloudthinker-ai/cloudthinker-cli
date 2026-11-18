use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;
use tokio::task::{JoinSet, LocalSet};
use uuid::Uuid;

use super::{ConnectionKind, ConnectionStatus, ConnectionTest, ConnectionTestPhase, CtClient};
use crate::auth::store::{
    CredentialProvenance, CredentialSource, EnvTokenStore, FileStore, StoredToken, TokenStore,
    WorkspaceSelector,
};
use crate::cli_config::URL_ENV_VAR;
use crate::doctor::{
    Area, Cause, Check, Fix, REQUEST_BUDGET, Report, Runner, SECTION_BUDGET, Status, overall,
};
use crate::error::{CtError, CtResult};
use crate::worker_api::{ProbeFailure, probe_origin};
use crate::{display_origin, login_command, origin_of, other_origin};

const LOCK_TIMEOUT: Duration = Duration::from_secs(1);
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);
const AUTH_RESERVE: Duration = Duration::from_secs(45);
const CONNECTION_LIMIT: usize = 20;
const CONNECTION_CONCURRENCY: usize = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OriginSource {
    Flag,
    Environment,
    Saved,
    BuiltIn,
}

impl OriginSource {
    pub fn label(self) -> &'static str {
        match self {
            Self::Flag => "from --url",
            Self::Environment => "from CLOUDTHINKER_URL",
            Self::Saved => "saved by login",
            Self::BuiltIn => "default",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ConnectionEvidence {
    pub id: Uuid,
    pub kind: ConnectionKind,
    pub status: Option<&'static str>,
    pub phase: Option<&'static str>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ServerEvidence {
    pub connection: Option<ConnectionEvidence>,
    pub http_status: Option<u16>,
}

pub type ServerCheck = Check<ServerEvidence>;

#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct DoctorCoverage {
    pub installed: Option<usize>,
    pub checked: usize,
    pub omitted: Option<usize>,
}

#[derive(Debug)]
pub struct Diagnosis {
    pub cli_version: String,
    pub origin: Option<String>,
    pub origin_source: OriginSource,
    pub user_id: Option<Uuid>,
    pub workspace_id: Option<Uuid>,
    pub connections: DoctorCoverage,
    pub report: Report<ServerEvidence>,
}

#[derive(Serialize)]
pub struct DoctorWireCheck {
    pub id: &'static str,
    pub area: Area,
    pub status: &'static str,
    pub summary: String,
    pub action: Option<String>,
    pub cause: Option<Cause>,
    pub connection_id: Option<Uuid>,
    pub connection_name: Option<String>,
    pub kind: Option<ConnectionKind>,
    pub connection_status: Option<&'static str>,
    pub test_phase: Option<&'static str>,
    pub http_status: Option<u16>,
}

#[derive(Serialize)]
pub struct DoctorWire {
    pub cli_version: String,
    pub origin: Option<String>,
    pub origin_source: OriginSource,
    pub overall: &'static str,
    pub user_id: Option<Uuid>,
    pub workspace_id: Option<Uuid>,
    pub checks: Vec<DoctorWireCheck>,
    pub connections: DoctorCoverage,
    pub exit_code: u8,
}

fn wire_status(status: Status) -> &'static str {
    match status {
        Status::Pass => "healthy",
        Status::Warn => "warn",
        Status::Fail => "error",
        Status::Skipped => "skipped",
        Status::Unknown => "unknown",
    }
}

impl Diagnosis {
    pub fn into_wire(self, exit_code: u8) -> DoctorWire {
        let overall = wire_status(self.report.overall());
        DoctorWire {
            cli_version: self.cli_version,
            origin: self.origin,
            origin_source: self.origin_source,
            overall,
            user_id: self.user_id,
            workspace_id: self.workspace_id,
            connections: self.connections,
            exit_code,
            checks: self
                .report
                .checks
                .into_iter()
                .map(|check| {
                    let action = check.fix_text();
                    let connection = check.evidence.connection;
                    DoctorWireCheck {
                        id: check.id,
                        area: check.area,
                        status: wire_status(check.status),
                        summary: check.summary,
                        action,
                        cause: check.cause,
                        connection_id: connection.map(|row| row.id),
                        connection_name: connection.and(check.subject),
                        kind: connection.map(|row| row.kind),
                        connection_status: connection.and_then(|row| row.status),
                        test_phase: connection.and_then(|row| row.phase),
                        http_status: check.evidence.http_status,
                    }
                })
                .collect(),
        }
    }
}

fn check(id: &'static str, status: Status, summary: impl Into<String>) -> ServerCheck {
    let (area, label) = match id {
        "cli" => (Area::Install, "CLI"),
        "configuration" => (Area::Account, "Server"),
        "credentials" => (Area::Account, "Login"),
        "api" => (Area::Server, "API"),
        "identity" => (Area::Account, "Account"),
        "workspace" => (Area::Account, "Workspace"),
        _ => (Area::Connections, "Connections"),
    };
    Check::new(area, id, status, summary).label(label)
}

fn step(text: impl Into<String>) -> Fix {
    Fix::Step(text.into())
}

fn with_http(mut check: ServerCheck, status: u16) -> ServerCheck {
    check.evidence.http_status = Some(status);
    check.detail(format!("HTTP {status}"))
}

fn with_connection(
    mut check: ServerCheck,
    id: Uuid,
    kind: ConnectionKind,
    name: &str,
) -> ServerCheck {
    let evidence = check.evidence.connection.get_or_insert(ConnectionEvidence {
        id,
        kind,
        status: None,
        phase: None,
    });
    evidence.id = id;
    evidence.kind = kind;
    check.subject(name)
}

fn skip(
    report: &mut Report<ServerEvidence>,
    blocker: &'static str,
    ids: &[&'static str],
    reason: &'static str,
) {
    for id in ids {
        report.push(check(id, Status::Skipped, reason).blocked_by(blocker));
    }
}

struct Prepared {
    client: CtClient,
    workspace_id: Option<Uuid>,
    source: CredentialSource,
}

struct SnapshotStore {
    token: StoredToken,
    source: CredentialSource,
}

impl TokenStore for SnapshotStore {
    fn source(&self) -> CredentialSource {
        self.source
    }

    fn load(&self) -> CtResult<Option<StoredToken>> {
        Ok(Some(self.token.clone()))
    }

    fn save(&self, _token: &StoredToken) -> CtResult<()> {
        Err(CtError::Store("diagnostic credential is read-only".into()))
    }

    fn clear(&self) -> CtResult<()> {
        Err(CtError::Store("diagnostic credential is read-only".into()))
    }

    fn refresh_enabled(&self) -> bool {
        false
    }
}

pub async fn diagnose(
    runner: &Runner,
    base_url: &str,
    origin_source: OriginSource,
    saved_url: Option<&str>,
    workspace: Option<&str>,
    cli_version: &str,
) -> Diagnosis {
    let mut report = Report::new("doctor", cli_version);
    report.push(check("cli", Status::Pass, format!("Version {cli_version}")));
    let mut diagnosis = Diagnosis {
        cli_version: cli_version.to_string(),
        origin: None,
        origin_source,
        user_id: None,
        workspace_id: None,
        connections: DoctorCoverage::default(),
        report,
    };
    collect(&mut diagnosis, runner, base_url, saved_url, workspace).await;
    diagnosis.report.server = diagnosis
        .origin
        .as_deref()
        .map(|origin| display_origin(origin).to_string());
    diagnosis
}

async fn collect(
    diagnosis: &mut Diagnosis,
    runner: &Runner,
    base_url: &str,
    saved_url: Option<&str>,
    workspace: Option<&str>,
) {
    let report = &mut diagnosis.report;
    let origin = match origin_of(base_url) {
        Ok(origin) => origin,
        Err(error) => {
            report.push(error_check("configuration", &error));
            skip(
                report,
                "configuration",
                &["credentials", "api", "identity", "workspace", "connections"],
                "Needs a valid server address",
            );
            return;
        }
    };
    diagnosis.origin = Some(origin.clone());
    let overridden = saved_url
        .filter(|_| diagnosis.origin_source == OriginSource::Environment)
        .and_then(|saved| other_origin(saved, &origin));
    report.push(match overridden {
        Some(saved) => check(
            "configuration",
            Status::Warn,
            format!(
                "{} from {URL_ENV_VAR}, not your saved default {}",
                display_origin(&origin),
                display_origin(&saved)
            ),
        )
        .fix(step(format!(
            "Unset {URL_ENV_VAR} to use the saved default, or pass --url to pick one."
        ))),
        None => check("configuration", Status::Pass, display_origin(&origin))
            .detail(diagnosis.origin_source.label()),
    });

    let prepared = prepare(base_url, &origin, workspace);
    if let Err(error) = &prepared {
        let row = if matches!(error, CtError::Auth(_)) && EnvTokenStore::from_env().is_none() {
            check("credentials", Status::Fail, "not signed in")
                .fix(Fix::Guide {
                    command: login_command(&origin),
                    text: legacy_recovery(&origin, workspace),
                })
                .cause(Cause::Auth)
        } else if matches!(error, CtError::ObsoleteCredentials { .. }) {
            check(
                "credentials",
                Status::Fail,
                "Saved login is unreadable or uses an older format",
            )
            .fix(step(legacy_recovery(&origin, workspace)))
            .cause(Cause::Auth)
        } else {
            error_check("credentials", error)
        };
        let usage = row.cause == Some(Cause::Usage);
        report.push(row);
        if usage {
            skip(
                report,
                "credentials",
                &["api", "identity", "workspace", "connections"],
                "Needs a valid login selection",
            );
            return;
        }
    } else if let Ok(prepared) = &prepared {
        let source = match prepared.source {
            CredentialSource::Stored => "Saved login found",
            CredentialSource::Environment => "Using CLOUDTHINKER_TOKEN; refresh is disabled",
        };
        report.push(check("credentials", Status::Pass, source));
    }

    let api_index = report.checks.len();
    report.push(public_probe(base_url).await);
    let prepared = match prepared {
        Ok(prepared) => prepared,
        Err(_) => {
            skip(
                report,
                "credentials",
                &["identity", "workspace", "connections"],
                "Needs a login",
            );
            return;
        }
    };
    if runner.remaining() < AUTH_RESERVE {
        report.push(
            check(
                "identity",
                Status::Unknown,
                "Insufficient time remains for a safe authentication refresh.",
            )
            .fix(step(
                "Run doctor again when local operations are responsive.",
            ))
            .cause(Cause::Deadline),
        );
        skip(
            report,
            "identity",
            &["workspace", "connections"],
            "Needs a working login",
        );
        return;
    }
    let identity = match prepared.client.whoami().await {
        Ok(identity) => identity,
        Err(error) => {
            let mut row = error_check("identity", &error);
            if row.cause == Some(Cause::Auth) && !matches!(error, CtError::Api { status: 403, .. })
            {
                let login = login_command(&origin);
                row.fix = Some(Fix::Guide {
                    text: format!("Run `{login}` to sign in again."),
                    command: login,
                });
            }
            report.push(row);
            skip(
                report,
                "identity",
                &["workspace", "connections"],
                "Needs a working login",
            );
            return;
        }
    };
    let api = &mut report.checks[api_index];
    if api.status != Status::Pass {
        api.status = Status::Warn;
        api.cause = None;
        api.summary = "Health probe inconclusive, but signed-in requests work".into();
        api.fix = None;
    }
    diagnosis.user_id = Some(identity.user_id);
    diagnosis.workspace_id = Some(identity.workspace_id);
    report.push(check(
        "identity",
        Status::Pass,
        format!("Signed in as {}", identity.user_email),
    ));
    if prepared
        .workspace_id
        .is_some_and(|expected| expected != identity.workspace_id)
    {
        report.push(
            check(
                "workspace",
                Status::Fail,
                "Live workspace does not match the selected file credential's workspace ID.",
            )
            .fix(step("Inspect the selected login with `cloudthinker auth status` and `cloudthinker whoami`."))
            .cause(Cause::Failure),
        );
        skip(
            report,
            "workspace",
            &["connections"],
            "Needs the matching workspace",
        );
        return;
    }
    report.push(check(
        "workspace",
        Status::Pass,
        identity.workspace_name.clone(),
    ));
    let snapshot = match snapshot_client(&prepared, identity.workspace_id) {
        Ok(snapshot) => snapshot,
        Err(error) => {
            report.push(error_check("connections", &error));
            return;
        }
    };
    let local = LocalSet::new();
    local
        .run_until(collect_connections(
            diagnosis,
            Arc::new(snapshot),
            runner.section(SECTION_BUDGET),
        ))
        .await;
}

fn prepare(base_url: &str, origin: &str, workspace: Option<&str>) -> CtResult<Prepared> {
    let store: Arc<dyn TokenStore> = if let Some(store) = EnvTokenStore::from_env() {
        if workspace.is_some() {
            return Err(CtError::Usage(
                "environment credential conflicts with workspace selection".into(),
            ));
        }
        Arc::new(store)
    } else {
        let selector = workspace.map_or(WorkspaceSelector::Active, |value| {
            WorkspaceSelector::IdOrName(value.to_string())
        });
        let store = FileStore::open_default_unmigrated(origin.to_string(), selector)?;
        let token = store
            .load()?
            .ok_or_else(|| CtError::Auth("no compatible selected file credential".into()))?;
        if let Some(id) = token.workspace_id {
            Arc::new(
                FileStore::open_default_unmigrated(
                    origin.to_string(),
                    WorkspaceSelector::IdOrName(id.to_string()),
                )?
                .with_lock_timeout(LOCK_TIMEOUT),
            )
        } else {
            Arc::new(store.with_lock_timeout(LOCK_TIMEOUT))
        }
    };
    let source = store.source();
    let workspace_id = store.load()?.and_then(|token| token.workspace_id);
    let client = CtClient::with_request_timeout(base_url.to_string(), store, REQUEST_BUDGET)?;
    if client.credential_provenance()? == CredentialProvenance::Missing {
        return Err(CtError::Auth("selected credential disappeared".into()));
    }
    Ok(Prepared {
        client,
        workspace_id,
        source,
    })
}

fn snapshot_client(prepared: &Prepared, workspace_id: Uuid) -> CtResult<CtClient> {
    let access = prepared
        .client
        .http_cache
        .lock()
        .map_err(|_| CtError::Store("diagnostic credential snapshot unavailable".into()))?
        .as_ref()
        .map(|(access, _, _)| access.clone())
        .ok_or_else(|| CtError::Store("authenticated credential snapshot unavailable".into()))?;
    let store = SnapshotStore {
        token: StoredToken {
            access_token: access,
            refresh_token: None,
            expires_at: None,
            workspace_id: Some(workspace_id),
            workspace_name: None,
        },
        source: prepared.source,
    };
    CtClient::with_request_timeout(
        prepared.client.base_url().to_string(),
        Arc::new(store),
        REQUEST_BUDGET,
    )
}

async fn public_probe(base_url: &str) -> ServerCheck {
    let failure = match tokio::time::timeout(PROBE_TIMEOUT, probe_origin(base_url)).await {
        Ok(Ok(probe)) => {
            let row = if (200..300).contains(&probe.status) {
                check("api", Status::Pass, "Reachable")
            } else {
                check(
                    "api",
                    Status::Unknown,
                    "Health endpoint did not return success",
                )
                .fix(step(
                    "Check the configured origin and deployment's public health route.",
                ))
                .cause(Cause::Failure)
            };
            return with_http(row, probe.status);
        }
        Ok(Err(failure)) => failure,
        Err(_) => ProbeFailure::Timeout,
    };
    let summary = match failure {
        ProbeFailure::Dns => "The public API hostname could not be resolved.",
        ProbeFailure::Connect => "The public API connection could not be established.",
        ProbeFailure::Timeout => "The public API probe exceeded its time budget.",
        ProbeFailure::Tls => "The public API TLS connection could not be verified.",
        ProbeFailure::Other => "The public API probe did not establish reachability.",
    };
    check("api", Status::Unknown, summary)
        .fix(step(
            "Check the configured origin, network, and trusted certificates; then run doctor again.",
        ))
        .cause(if failure == ProbeFailure::Timeout {
            Cause::Deadline
        } else {
            Cause::Failure
        })
}

fn classify_error(error: &CtError) -> Cause {
    match error {
        CtError::Usage(_) | CtError::Api { status: 422, .. } => Cause::Usage,
        CtError::Auth(_)
        | CtError::ObsoleteCredentials { .. }
        | CtError::Api {
            status: 401 | 403, ..
        } => Cause::Auth,
        CtError::Timeout(_) => Cause::Deadline,
        _ => Cause::Failure,
    }
}

fn error_check(id: &'static str, error: &CtError) -> ServerCheck {
    let (summary, action) = match error {
        CtError::Api { status: 403, .. } => (
            "The account lacks permission for this check; this does not establish expired credentials.",
            "Ask a workspace admin for access, or inspect the selected workspace.",
        ),
        CtError::Auth(_) | CtError::Api { status: 401, .. } => (
            "The selected credential was not accepted for this check.",
            "Check the intended account and workspace before signing in again.",
        ),
        CtError::Usage(_) | CtError::Api { status: 422, .. } => (
            "The URL, workspace selection, or request was rejected.",
            "Use an HTTPS origin (HTTP only for loopback), an unambiguous stored workspace ID, and no workspace selector with CLOUDTHINKER_TOKEN.",
        ),
        CtError::Timeout(_) => (
            "This check exceeded its time budget; no repair was attempted.",
            "Wait for other credential operations or network activity to finish, then run doctor again.",
        ),
        CtError::ObsoleteCredentials { .. } | CtError::Store(_) => (
            "Compatible credential-file evidence could not be read; private contents are hidden.",
            "Run `cloudthinker whoami` to inspect the existing login and follow its recovery guidance.",
        ),
        CtError::Transport(_) => (
            "An API request failed or timed out; no response details are exposed.",
            "Check the CloudThinker origin, network, and trusted certificates; then run doctor again.",
        ),
        CtError::Protocol(_) => (
            "The API returned an unreadable response.",
            "Check that the URL points to CloudThinker and that the CLI and server support this check.",
        ),
        CtError::Api { .. } => (
            "The API did not allow this check to complete.",
            "Check the selected workspace and deployment, then run doctor again.",
        ),
        _ => (
            "This check could not complete.",
            "Check the selected workspace and deployment, then run doctor again.",
        ),
    };
    let row = check(id, Status::Fail, summary)
        .fix(step(action))
        .cause(classify_error(error));
    match error {
        CtError::Api { status, .. } => with_http(row, *status),
        _ => row,
    }
}

fn legacy_recovery(origin: &str, workspace: Option<&str>) -> String {
    let login = login_command(origin);
    let whoami = login.replacen("cloudthinker login", "cloudthinker whoami", 1);
    let selection = if workspace.is_some() {
        " with your original --workspace selection"
    } else {
        ""
    };
    format!(
        "Run `{whoami}`{selection} to inspect or migrate an older login first. If no usable login exists, run `{login}`."
    )
}

async fn collect_connections(diagnosis: &mut Diagnosis, client: Arc<CtClient>, section: Runner) {
    let report = &mut diagnosis.report;
    let listed = match section.within(client.list_connections()).await {
        Ok(result) => result.map_err(|error| error_check("connections", &error)),
        Err(cause) => Err(stopped_check(cause)),
    };
    let mut installed = match listed {
        Ok(connections) => connections.installed,
        Err(mut row) => {
            row.id = "connections";
            report.push(row);
            return;
        }
    };
    let total = installed.len();
    diagnosis.connections.installed = Some(total);
    diagnosis.connections.omitted = Some(total);
    if total == 0 {
        report.push(
            check(
                "connections",
                Status::Warn,
                "None installed in this workspace",
            )
            .fix(step(
                "Add a Connection in the app when your task needs one.",
            )),
        );
        return;
    }
    installed.sort_by_key(|row| (row.id, row.kind.label()));
    installed.truncate(CONNECTION_LIMIT);
    let selected: Vec<_> = installed
        .into_iter()
        .map(|row| (row.id, row.kind, row.status, row.name))
        .collect();
    let mut remaining = selected.iter();
    let mut tasks = JoinSet::new();
    let connection_start = report.checks.len();
    for row in remaining.by_ref().take(CONNECTION_CONCURRENCY) {
        start_connection(&mut tasks, client.clone(), row);
    }
    let mut stopped = None;
    while !tasks.is_empty() {
        let joined = match section.within(tasks.join_next()).await {
            Ok(joined) => joined,
            Err(cause) => {
                stopped = Some(cause);
                break;
            }
        };
        match joined {
            Some(Ok((id, kind, name, status, result))) => {
                let row = match result {
                    Ok(test) => {
                        diagnosis.connections.checked += 1;
                        with_connection(classify_connection(&status, &test), id, kind, &name)
                    }
                    Err(error) => {
                        let mut row =
                            with_connection(error_check("connection", &error), id, kind, &name);
                        row.status = Status::Unknown;
                        row
                    }
                };
                let lost_auth = row.cause == Some(Cause::Auth);
                report.push(row);
                if lost_auth {
                    stopped = Some(Cause::Auth);
                    break;
                }
                if let Some(row) = remaining.next() {
                    start_connection(&mut tasks, client.clone(), row);
                }
            }
            Some(Err(_)) => {
                stopped = Some(Cause::Failure);
                break;
            }
            None => break,
        }
    }
    tasks.abort_all();
    while tasks.join_next().await.is_some() {}
    let read = |row: &ServerCheck, id: Uuid, kind: ConnectionKind| {
        row.evidence
            .connection
            .is_some_and(|seen| seen.id == id && seen.kind == kind)
    };
    for (id, kind, _, name) in &selected {
        if !report.checks.iter().any(|row| read(row, *id, *kind)) {
            report.push(with_connection(
                stopped_check(stopped.unwrap_or(Cause::Failure)),
                *id,
                *kind,
                name,
            ));
        }
    }
    report.checks[connection_start..].sort_by_key(|row| {
        row.evidence
            .connection
            .map(|seen| (seen.id, seen.kind.label()))
    });
    let omitted = total - diagnosis.connections.checked;
    diagnosis.connections.omitted = Some(omitted);
    let observed = overall(&report.checks[connection_start..]);
    let healthy = report.checks[connection_start..]
        .iter()
        .filter(|row| row.status == Status::Pass)
        .count();
    diagnosis.report.tally = Some(format!("{healthy} of {total} Connections"));
    let report = &mut diagnosis.report;
    let mut summary = check(
        "connections",
        if omitted == 0 || observed == Status::Fail {
            observed
        } else {
            Status::Unknown
        },
        if omitted == 0 {
            format!("{healthy} of {total} connected and passing")
        } else {
            format!("{healthy} of {total} connected and passing; {omitted} not read")
        },
    );
    if omitted > 0 {
        summary = summary.cause(stopped.unwrap_or(Cause::Failure)).fix(step(
            "Run doctor again, or open the unread Connections in the app.",
        ));
    }
    report.push(summary);
}

type ConnectionTask = (
    Uuid,
    ConnectionKind,
    String,
    ConnectionStatus,
    CtResult<ConnectionTest>,
);

fn start_connection(
    tasks: &mut JoinSet<ConnectionTask>,
    client: Arc<CtClient>,
    row: &(Uuid, ConnectionKind, ConnectionStatus, String),
) {
    let (id, kind, status, name) = row;
    let (id, kind, status, name) = (*id, *kind, status.clone(), name.clone());
    tasks.spawn_local(async move {
        (
            id,
            kind,
            name,
            status,
            client.connection_test(id, kind).await,
        )
    });
}

fn stopped_check(cause: Cause) -> ServerCheck {
    let summary = match cause {
        Cause::Deadline => "Not read: the time budget ran out",
        Cause::Interrupted => "Not read: interrupted",
        Cause::Auth => "Not read: access was lost",
        _ => "Not read: the check could not finish",
    };
    check("connection", Status::Unknown, summary)
        .fix(step("Run doctor again after resolving the reported cause."))
        .cause(cause)
}

fn classify_connection(status: &ConnectionStatus, test: &ConnectionTest) -> ServerCheck {
    let (state, summary, cause) = match (&test.phase, status) {
        (
            ConnectionTestPhase::Failed
            | ConnectionTestPhase::Timeout
            | ConnectionTestPhase::Cancelled,
            _,
        ) => (
            Status::Fail,
            "Last test failed, timed out, or was cancelled",
            Some(Cause::Failure),
        ),
        (_, ConnectionStatus::Other(_) | ConnectionStatus::Unknown)
        | (ConnectionTestPhase::Other(_), _) => (
            Status::Unknown,
            "Unknown status or test phase from the server",
            Some(Cause::Failure),
        ),
        (_, ConnectionStatus::Error) => (Status::Fail, "Reports an error", Some(Cause::Failure)),
        (_, ConnectionStatus::Disconnecting) => (Status::Warn, "Disconnecting", None),
        (
            ConnectionTestPhase::Queued
            | ConnectionTestPhase::StartingSandbox
            | ConnectionTestPhase::RunningScript,
            _,
        ) => (Status::Warn, "Last test still pending", None),
        (ConnectionTestPhase::Succeeded, ConnectionStatus::Connected) => {
            (Status::Pass, "Connected; last test passed", None)
        }
        _ => (
            Status::Unknown,
            "Status and last test disagree",
            Some(Cause::Failure),
        ),
    };
    let recorded_status = match status {
        ConnectionStatus::Connected => "connected",
        ConnectionStatus::Testing => "testing",
        ConnectionStatus::Disconnecting => "disconnecting",
        ConnectionStatus::Error => "error",
        ConnectionStatus::Unknown | ConnectionStatus::Other(_) => "unknown",
    };
    let phase = match &test.phase {
        ConnectionTestPhase::Queued => "queued",
        ConnectionTestPhase::StartingSandbox => "starting_sandbox",
        ConnectionTestPhase::RunningScript => "running_script",
        ConnectionTestPhase::Succeeded => "succeeded",
        ConnectionTestPhase::Failed => "failed",
        ConnectionTestPhase::Timeout => "timeout",
        ConnectionTestPhase::Cancelled => "cancelled",
        ConnectionTestPhase::Other(_) => "unknown",
    };
    let mut row = check("connection", state, summary).detail(format!(
        "status {recorded_status}, test {}",
        phase.replace('_', " ")
    ));
    row.cause = cause;
    row.evidence.connection = Some(ConnectionEvidence {
        id: Uuid::nil(),
        kind: ConnectionKind::Builtin,
        status: Some(recorded_status),
        phase: Some(phase),
    });
    if state != Status::Pass {
        row.fix = Some(step(
            "Open the Connection in the app and run its test again.",
        ));
    }
    row
}
