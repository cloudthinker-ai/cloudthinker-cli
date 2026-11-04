use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;
use tokio::task::{JoinSet, LocalSet};
use tokio::time::Instant;
use uuid::Uuid;

use super::{ConnectionKind, ConnectionStatus, ConnectionTest, ConnectionTestPhase, CtClient};
use crate::auth::store::{
    CredentialProvenance, CredentialSource, EnvTokenStore, FileStore, StoredToken, TokenStore,
    WorkspaceSelector,
};
use crate::cli_config::URL_ENV_VAR;
use crate::error::{CtError, CtResult};
use crate::worker_api::{ProbeFailure, probe_origin};
use crate::{display_origin, login_command, origin_of, other_origin};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(3);
const LOCK_TIMEOUT: Duration = Duration::from_secs(1);
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);
const AUTH_RESERVE: Duration = Duration::from_secs(45);
const CONNECTION_BUDGET: Duration = Duration::from_secs(15);
const COMMAND_BUDGET: Duration = Duration::from_secs(70);
const CONNECTION_LIMIT: usize = 20;
const CONNECTION_CONCURRENCY: usize = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DoctorStatus {
    Healthy,
    Warn,
    Error,
    Skipped,
    Unknown,
}

impl DoctorStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Healthy => "healthy",
            Self::Warn => "warn",
            Self::Error => "error",
            Self::Skipped => "skipped",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OriginSource {
    Flag,
    Environment,
    Saved,
    BuiltIn,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DoctorCause {
    Usage,
    Auth,
    Deadline,
    Failure,
    Interrupted,
}

#[derive(Debug, Serialize)]
pub struct DoctorCheck {
    pub id: &'static str,
    pub status: DoctorStatus,
    pub summary: String,
    pub action: Option<String>,
    pub cause: Option<DoctorCause>,
    pub connection_id: Option<Uuid>,
    pub connection_name: Option<String>,
    pub kind: Option<ConnectionKind>,
    pub connection_status: Option<&'static str>,
    pub test_phase: Option<&'static str>,
    pub http_status: Option<u16>,
}

impl DoctorCheck {
    fn new(id: &'static str, status: DoctorStatus, summary: impl Into<String>) -> Self {
        Self {
            id,
            status,
            summary: summary.into(),
            action: None,
            cause: None,
            connection_id: None,
            connection_name: None,
            kind: None,
            connection_status: None,
            test_phase: None,
            http_status: None,
        }
    }

    fn action(mut self, action: impl Into<String>) -> Self {
        self.action = Some(action.into());
        self
    }

    fn cause(mut self, cause: DoctorCause) -> Self {
        self.cause = Some(cause);
        self
    }

    fn connection(mut self, id: Uuid, kind: ConnectionKind, name: &str) -> Self {
        self.connection_id = Some(id);
        self.connection_name = Some(name.to_string());
        self.kind = Some(kind);
        self
    }
}

#[derive(Debug, Default, Serialize)]
pub struct DoctorCoverage {
    pub installed: Option<usize>,
    pub checked: usize,
    pub omitted: Option<usize>,
}

#[derive(Debug, Serialize)]
pub struct DoctorReport {
    pub cli_version: String,
    pub origin: Option<String>,
    pub origin_source: OriginSource,
    pub overall: DoctorStatus,
    pub user_id: Option<Uuid>,
    pub workspace_id: Option<Uuid>,
    pub checks: Vec<DoctorCheck>,
    pub connections: DoctorCoverage,
}

impl DoctorReport {
    fn skip(&mut self, ids: &[&'static str], reason: &'static str) {
        self.checks.extend(
            ids.iter()
                .map(|id| DoctorCheck::new(id, DoctorStatus::Skipped, reason)),
        );
    }

    fn finish(mut self) -> Self {
        self.checks.push(DoctorCheck::new(
            "release",
            DoctorStatus::Skipped,
            "Not checked",
        ));
        self.overall = observed_status(&self.checks);
        self
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
    base_url: &str,
    origin_source: OriginSource,
    saved_url: Option<&str>,
    workspace: Option<&str>,
    cli_version: &str,
) -> DoctorReport {
    let deadline = Instant::now() + COMMAND_BUDGET;
    let mut report = DoctorReport {
        cli_version: cli_version.to_string(),
        origin: None,
        origin_source,
        overall: DoctorStatus::Unknown,
        user_id: None,
        workspace_id: None,
        checks: vec![DoctorCheck::new(
            "cli",
            DoctorStatus::Healthy,
            format!("Version {cli_version}"),
        )],
        connections: DoctorCoverage::default(),
    };
    collect(&mut report, base_url, saved_url, workspace, deadline).await;
    report.finish()
}

async fn collect(
    report: &mut DoctorReport,
    base_url: &str,
    saved_url: Option<&str>,
    workspace: Option<&str>,
    deadline: Instant,
) {
    let origin = match origin_of(base_url) {
        Ok(origin) => origin,
        Err(error) => {
            report.checks.push(error_check("configuration", &error));
            report.skip(
                &["credentials", "api", "identity", "workspace", "connections"],
                "Needs a valid server address",
            );
            return;
        }
    };
    report.origin = Some(origin.clone());
    let overridden = saved_url
        .filter(|_| report.origin_source == OriginSource::Environment)
        .and_then(|saved| other_origin(saved, &origin));
    report.checks.push(match overridden {
        Some(saved) => DoctorCheck::new(
            "configuration",
            DoctorStatus::Warn,
            format!(
                "{} from {URL_ENV_VAR}, not your saved default {}",
                display_origin(&origin),
                display_origin(&saved)
            ),
        )
        .action(format!(
            "Unset {URL_ENV_VAR} to use the saved default, or pass --url to pick one."
        )),
        None => DoctorCheck::new(
            "configuration",
            DoctorStatus::Healthy,
            display_origin(&origin),
        ),
    });

    let prepared = prepare(base_url, &origin, workspace);
    if let Err(error) = &prepared {
        let check = if matches!(error, CtError::Auth(_)) && EnvTokenStore::from_env().is_none() {
            DoctorCheck::new(
                "credentials",
                DoctorStatus::Unknown,
                "No saved login for this server",
            )
            .action(legacy_recovery(&origin, workspace))
            .cause(DoctorCause::Auth)
        } else if matches!(error, CtError::ObsoleteCredentials { .. }) {
            DoctorCheck::new(
                "credentials",
                DoctorStatus::Error,
                "Saved login is unreadable or uses an older format",
            )
            .action(legacy_recovery(&origin, workspace))
            .cause(DoctorCause::Auth)
        } else {
            error_check("credentials", error)
        };
        let usage = check.cause == Some(DoctorCause::Usage);
        report.checks.push(check);
        if usage {
            report.skip(
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
        report.checks.push(DoctorCheck::new(
            "credentials",
            DoctorStatus::Healthy,
            source,
        ));
    }

    let api_index = report.checks.len();
    report.checks.push(public_probe(base_url).await);
    let prepared = match prepared {
        Ok(prepared) => prepared,
        Err(_) => {
            report.skip(&["identity", "workspace", "connections"], "Needs a login");
            return;
        }
    };
    if deadline.saturating_duration_since(Instant::now()) < AUTH_RESERVE {
        report.checks.push(
            DoctorCheck::new(
                "identity",
                DoctorStatus::Unknown,
                "Insufficient time remains for a safe authentication refresh.",
            )
            .action("Run doctor again when local operations are responsive.")
            .cause(DoctorCause::Deadline),
        );
        report.skip(&["workspace", "connections"], "Needs a working login");
        return;
    }
    let identity = match prepared.client.whoami().await {
        Ok(identity) => identity,
        Err(error) => {
            let mut check = error_check("identity", &error);
            if check.cause == Some(DoctorCause::Auth)
                && !matches!(error, CtError::Api { status: 403, .. })
            {
                check.action = Some(format!(
                    "Run `{}` to sign in again.",
                    login_command(&origin)
                ));
            }
            report.checks.push(check);
            report.skip(&["workspace", "connections"], "Needs a working login");
            return;
        }
    };
    if report.checks[api_index].status != DoctorStatus::Healthy {
        report.checks[api_index].status = DoctorStatus::Warn;
        report.checks[api_index].cause = None;
        report.checks[api_index].summary =
            "Health probe inconclusive, but signed-in requests work".into();
        report.checks[api_index].action = None;
    }
    report.user_id = Some(identity.user_id);
    report.workspace_id = Some(identity.workspace_id);
    report.checks.push(DoctorCheck::new(
        "identity",
        DoctorStatus::Healthy,
        format!("Signed in as {}", identity.user_email),
    ));
    if prepared
        .workspace_id
        .is_some_and(|expected| expected != identity.workspace_id)
    {
        report.checks.push(
            DoctorCheck::new(
                "workspace",
                DoctorStatus::Error,
                "Live workspace does not match the selected file credential's workspace ID.",
            )
            .action("Inspect the selected login with `cloudthinker auth status` and `cloudthinker whoami`.")
            .cause(DoctorCause::Failure),
        );
        report.skip(&["connections"], "Needs the matching workspace");
        return;
    }
    report.checks.push(DoctorCheck::new(
        "workspace",
        DoctorStatus::Healthy,
        identity.workspace_name.clone(),
    ));
    let snapshot = match snapshot_client(&prepared, identity.workspace_id) {
        Ok(snapshot) => snapshot,
        Err(error) => {
            report.checks.push(error_check("connections", &error));
            return;
        }
    };
    collect_connections(report, Arc::new(snapshot), deadline).await;
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
    let client = CtClient::with_request_timeout(base_url.to_string(), store, REQUEST_TIMEOUT)?;
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
        REQUEST_TIMEOUT,
    )
}

async fn public_probe(base_url: &str) -> DoctorCheck {
    let failure = match tokio::time::timeout(PROBE_TIMEOUT, probe_origin(base_url)).await {
        Ok(Ok(probe)) => {
            let mut check = if (200..300).contains(&probe.status) {
                DoctorCheck::new("api", DoctorStatus::Healthy, "Reachable")
            } else {
                DoctorCheck::new(
                    "api",
                    DoctorStatus::Unknown,
                    "Health endpoint did not return success",
                )
                .action("Check the configured origin and deployment's public health route.")
                .cause(DoctorCause::Failure)
            };
            check.http_status = Some(probe.status);
            return check;
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
    DoctorCheck::new("api", DoctorStatus::Unknown, summary)
        .action("Check the configured origin, network, and trusted certificates; then run doctor again.")
        .cause(if failure == ProbeFailure::Timeout {
            DoctorCause::Deadline
        } else {
            DoctorCause::Failure
        })
}

fn classify_error(error: &CtError) -> DoctorCause {
    match error {
        CtError::Usage(_) | CtError::Api { status: 422, .. } => DoctorCause::Usage,
        CtError::Auth(_)
        | CtError::ObsoleteCredentials { .. }
        | CtError::Api {
            status: 401 | 403, ..
        } => DoctorCause::Auth,
        CtError::Timeout(_) => DoctorCause::Deadline,
        _ => DoctorCause::Failure,
    }
}

fn error_check(id: &'static str, error: &CtError) -> DoctorCheck {
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
    let mut check = DoctorCheck::new(id, DoctorStatus::Error, summary)
        .action(action)
        .cause(classify_error(error));
    if let CtError::Api { status, .. } = error {
        check.http_status = Some(*status);
    }
    check
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

async fn collect_connections(
    report: &mut DoctorReport,
    client: Arc<CtClient>,
    command_deadline: Instant,
) {
    let local = LocalSet::new();
    local
        .run_until(collect_connections_local(report, client, command_deadline))
        .await;
}

async fn collect_connections_local(
    report: &mut DoctorReport,
    client: Arc<CtClient>,
    command_deadline: Instant,
) {
    let deadline = command_deadline.min(Instant::now() + CONNECTION_BUDGET);
    let listed = tokio::select! {
        result = tokio::time::timeout_at(deadline, client.list_connections()) => match result {
            Ok(result) => result.map_err(|error| error_check("connections", &error)),
            Err(_) => Err(stopped_check(DoctorCause::Deadline)),
        },
        signal = tokio::signal::ctrl_c() => Err(stopped_check(match signal {
            Ok(()) => DoctorCause::Interrupted,
            Err(_) => DoctorCause::Failure,
        })),
    };
    let mut installed = match listed {
        Ok(connections) => connections.installed,
        Err(mut check) => {
            check.id = "connections";
            report.checks.push(check);
            return;
        }
    };
    let total = installed.len();
    report.connections.installed = Some(total);
    report.connections.omitted = Some(total);
    if total == 0 {
        report.checks.push(
            DoctorCheck::new(
                "connections",
                DoctorStatus::Warn,
                "None installed in this workspace",
            )
            .action("Add a Connection in the app when your task needs one."),
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
        let joined = tokio::select! {
            result = tokio::time::timeout_at(deadline, tasks.join_next()) => match result {
                Ok(result) => result,
                Err(_) => {
                    stopped = Some(DoctorCause::Deadline);
                    break;
                }
            },
            signal = tokio::signal::ctrl_c() => {
                stopped = Some(match signal {
                    Ok(()) => DoctorCause::Interrupted,
                    Err(_) => DoctorCause::Failure,
                });
                break;
            },
        };
        match joined {
            Some(Ok((id, kind, name, status, result))) => {
                let check = match result {
                    Ok(test) => {
                        report.connections.checked += 1;
                        classify_connection(&status, &test).connection(id, kind, &name)
                    }
                    Err(error) => {
                        let mut check =
                            error_check("connection", &error).connection(id, kind, &name);
                        check.status = DoctorStatus::Unknown;
                        check
                    }
                };
                let lost_auth = check.cause == Some(DoctorCause::Auth);
                report.checks.push(check);
                if lost_auth {
                    stopped = Some(DoctorCause::Auth);
                    break;
                }
                if let Some(row) = remaining.next() {
                    start_connection(&mut tasks, client.clone(), row);
                }
            }
            Some(Err(_)) => {
                stopped = Some(DoctorCause::Failure);
                break;
            }
            None => break,
        }
    }
    tasks.abort_all();
    while tasks.join_next().await.is_some() {}
    for (id, kind, _, name) in &selected {
        if !report
            .checks
            .iter()
            .any(|check| check.connection_id == Some(*id) && check.kind == Some(*kind))
        {
            report.checks.push(
                stopped_check(stopped.unwrap_or(DoctorCause::Failure)).connection(*id, *kind, name),
            );
        }
    }
    report.checks[connection_start..]
        .sort_by_key(|check| (check.connection_id, check.kind.map(ConnectionKind::label)));
    let omitted = total - report.connections.checked;
    report.connections.omitted = Some(omitted);
    let observed = observed_status(&report.checks[connection_start..]);
    let healthy = report.checks[connection_start..]
        .iter()
        .filter(|check| check.status == DoctorStatus::Healthy)
        .count();
    let mut summary = DoctorCheck::new(
        "connections",
        if omitted == 0 || observed == DoctorStatus::Error {
            observed
        } else {
            DoctorStatus::Unknown
        },
        if omitted == 0 {
            format!("{healthy} of {total} connected and passing")
        } else {
            format!("{healthy} of {total} connected and passing; {omitted} not read")
        },
    );
    if omitted > 0 {
        summary = summary
            .cause(stopped.unwrap_or(DoctorCause::Failure))
            .action("Run doctor again, or open the unread Connections in the app.");
    }
    report.checks.push(summary);
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

fn stopped_check(cause: DoctorCause) -> DoctorCheck {
    let summary = match cause {
        DoctorCause::Deadline => "Not read: the time budget ran out",
        DoctorCause::Interrupted => "Not read: interrupted",
        DoctorCause::Auth => "Not read: access was lost",
        _ => "Not read: the check could not finish",
    };
    DoctorCheck::new("connection", DoctorStatus::Unknown, summary)
        .action("Run doctor again after resolving the reported cause.")
        .cause(cause)
}

fn classify_connection(status: &ConnectionStatus, test: &ConnectionTest) -> DoctorCheck {
    let (state, summary, cause) = match (&test.phase, status) {
        (
            ConnectionTestPhase::Failed
            | ConnectionTestPhase::Timeout
            | ConnectionTestPhase::Cancelled,
            _,
        ) => (
            DoctorStatus::Error,
            "Last test failed, timed out, or was cancelled",
            Some(DoctorCause::Failure),
        ),
        (_, ConnectionStatus::Other(_) | ConnectionStatus::Unknown)
        | (ConnectionTestPhase::Other(_), _) => (
            DoctorStatus::Unknown,
            "Unknown status or test phase from the server",
            Some(DoctorCause::Failure),
        ),
        (_, ConnectionStatus::Error) => (
            DoctorStatus::Error,
            "Reports an error",
            Some(DoctorCause::Failure),
        ),
        (_, ConnectionStatus::Disconnecting) => (DoctorStatus::Warn, "Disconnecting", None),
        (
            ConnectionTestPhase::Queued
            | ConnectionTestPhase::StartingSandbox
            | ConnectionTestPhase::RunningScript,
            _,
        ) => (DoctorStatus::Warn, "Last test still pending", None),
        (ConnectionTestPhase::Succeeded, ConnectionStatus::Connected) => {
            (DoctorStatus::Healthy, "Connected; last test passed", None)
        }
        _ => (
            DoctorStatus::Unknown,
            "Status and last test disagree",
            Some(DoctorCause::Failure),
        ),
    };
    let mut check = DoctorCheck::new("connection", state, summary);
    check.cause = cause;
    check.connection_status = Some(match status {
        ConnectionStatus::Connected => "connected",
        ConnectionStatus::Testing => "testing",
        ConnectionStatus::Disconnecting => "disconnecting",
        ConnectionStatus::Error => "error",
        ConnectionStatus::Unknown | ConnectionStatus::Other(_) => "unknown",
    });
    check.test_phase = Some(match &test.phase {
        ConnectionTestPhase::Queued => "queued",
        ConnectionTestPhase::StartingSandbox => "starting_sandbox",
        ConnectionTestPhase::RunningScript => "running_script",
        ConnectionTestPhase::Succeeded => "succeeded",
        ConnectionTestPhase::Failed => "failed",
        ConnectionTestPhase::Timeout => "timeout",
        ConnectionTestPhase::Cancelled => "cancelled",
        ConnectionTestPhase::Other(_) => "unknown",
    });
    if state != DoctorStatus::Healthy {
        check.action = Some("Open the Connection in the app and run its test again.".into());
    }
    check
}

fn observed_status(checks: &[DoctorCheck]) -> DoctorStatus {
    [
        DoctorStatus::Error,
        DoctorStatus::Unknown,
        DoctorStatus::Warn,
    ]
    .into_iter()
    .find(|status| checks.iter().any(|check| check.status == *status))
    .unwrap_or(DoctorStatus::Healthy)
}
