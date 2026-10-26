//! `CtClient` — the typed wire surface every command calls.
//!
//! Wraps the generated `cloudthinker_api::Client`, injects the bearer token,
//! and owns the 401-retry / proactive-refresh dance so commands never touch
//! reqwest, serde, or refresh logic directly.

use std::num::NonZeroU64;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::Serialize;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::auth::refresh::RefreshCoordinator;
use crate::auth::store::{
    CredentialProvenance, EnvTokenStore, FileStore, StoredToken, TOKEN_ENV_VAR, TokenStore,
    WorkspaceSelector, acquire_credential_lock,
};
use crate::error::{CtError, CtResult, to_ct_error};
use crate::retry::classify;

mod connections;
mod cyber;
mod incidents;
mod memory;
mod recommendations;
mod reviews;
#[cfg(test)]
use crate::review_url::{MrCoordinates, MrProvider};
pub use connections::{
    ConnectionDetail, ConnectionKind, ConnectionLocation, ConnectionMatch, ConnectionStatus,
    ConnectionTest, ConnectionTestPhase, ConnectionView, Connections, validate_connect_prefix,
};
pub use cyber::{
    Coverage, CoverageReport, CoverageRow, CoverageStatus, CyberApp, CyberDiscoveryTarget,
    CyberDomain, CyberExecutionHost, CyberExport, CyberFinding, CyberFindingPage,
    CyberFindingStatus, CyberIntensity, CyberMode, CyberRun, CyberRunBrief, CyberRunResult,
    CyberSessionEntry, CyberSeverity, CyberTriageState, EvidenceFile, EvidenceReceipt,
    EvidenceSkipped, FindingFilter, LocalCyberWorkspace, Observation, Partition, PlanCheck,
    ProbeOutcome, SettleResult, Shard, Surface, WorkPlan,
};
#[cfg(test)]
use cyber::{observation_batches, validate_observation_ids};
pub use incidents::{IncidentStatus, IncidentView};
pub use memory::{CyberMemoryContextSource, CyberMemoryFile, CyberMemorySnapshot};
pub use recommendations::{RecommendationStatus, RecommendationView};
pub use reviews::{ReviewFinding, ReviewSeverityCounts, ReviewStatus, ReviewVerdict, ReviewView};

pub const DEFAULT_BASE_URL: &str = "https://app.cloudthinker.io";
const REQUEST_TIMEOUT_SECS: u64 = 30;

/// A short-lived device authorization shown in another browser.
#[derive(Debug, Clone)]
pub struct DeviceAuthorization {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub expires_in: Duration,
    pub interval: Duration,
}

/// One response from the RFC 8628-style device token poll.
#[derive(Debug, Clone)]
pub enum DeviceTokenPoll {
    Pending(Duration),
    SlowDown(Duration),
    Unavailable(Duration),
    Token(StoredToken),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LogoutOutcome {
    Cleared,
    NothingStored,
}

#[derive(Debug, Clone, Serialize)]
pub struct CliIdentity {
    pub host: String,
    pub user_id: Uuid,
    pub user_email: String,
    pub workspace_id: Uuid,
    pub workspace_name: String,
}

/// Terminal + non-terminal run states, serialized with the API's wire values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Pending,
    Running,
    Succeeded,
    Failed,
    RequiredApproval,
    Cancelled,
}

impl RunStatus {
    /// True once the run has reached a state that will not change on its own.
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Succeeded | Self::Failed | Self::RequiredApproval | Self::Cancelled
        )
    }
}

impl From<cloudthinker_api::types::AgentRunStatus> for RunStatus {
    fn from(status: cloudthinker_api::types::AgentRunStatus) -> Self {
        use cloudthinker_api::types::AgentRunStatus as A;
        match status {
            A::Pending => Self::Pending,
            A::Running => Self::Running,
            A::Succeeded => Self::Succeeded,
            A::Failed => Self::Failed,
            A::RequiredApproval => Self::RequiredApproval,
            A::Cancelled => Self::Cancelled,
        }
    }
}

/// Result of `POST /cli/runs`.
#[derive(Debug, Clone)]
pub struct SubmittedRun {
    pub run_id: Uuid,
    pub conversation_id: Uuid,
    pub status: RunStatus,
    pub web_url: String,
}

impl SubmittedRun {
    fn from_api(api: cloudthinker_api::types::HeadlessRunSubmitted) -> Self {
        Self {
            run_id: api.run_id,
            conversation_id: api.conversation_id,
            status: api.status.into(),
            web_url: api.web_url,
        }
    }
}

/// One recent headless run returned by `GET /cli/runs`.
#[derive(Debug, Clone, Serialize)]
pub struct RunListItem {
    pub run_id: Uuid,
    pub conversation_id: Option<Uuid>,
    pub status: RunStatus,
    pub prompt_preview: Option<String>,
    pub created_at: DateTime<Utc>,
    pub web_url: Option<String>,
}

impl From<cloudthinker_api::types::HeadlessRunListItem> for RunListItem {
    fn from(api: cloudthinker_api::types::HeadlessRunListItem) -> Self {
        Self {
            run_id: api.run_id,
            conversation_id: api.conversation_id,
            status: api.status.into(),
            prompt_preview: api.prompt_preview,
            created_at: api.created_at,
            web_url: api.web_url,
        }
    }
}

/// A polled view of a run (`GET /cli/runs/{id}`).
#[derive(Debug, Clone)]
pub struct RunView {
    pub run_id: Uuid,
    pub conversation_id: Option<Uuid>,
    pub status: RunStatus,
    pub answer: Option<String>,
    pub message: Option<String>,
    pub failure_kind: Option<String>,
    pub web_url: Option<String>,
}

impl RunView {
    fn from_api(api: cloudthinker_api::types::HeadlessRunStatus) -> Self {
        Self {
            run_id: api.run_id,
            conversation_id: api.conversation_id,
            status: api.status.into(),
            answer: api.answer,
            message: api.message,
            failure_kind: api.failure_kind,
            web_url: api.web_url,
        }
    }
}

/// The authenticated client. Cheap to clone-share via `Arc`.
pub struct CtClient {
    base_url: String,
    store: Arc<dyn TokenStore>,
    refresh: RefreshCoordinator,
    // Reqwest pools connections; rebuild only when the bearer token rotates so
    // polling reuses one TLS connection across the run.
    http_cache: Mutex<Option<(String, Duration, reqwest::Client)>>,
    // Anonymous client for endpoints that carry no bearer (exchange, refresh,
    // best-effort logout).
    anon_http: reqwest::Client,
    probe_http: reqwest::Client,
    rejected_credential: Mutex<Option<[u8; 32]>>,
}

impl CtClient {
    pub fn new(base_url: impl Into<String>, store: Arc<dyn TokenStore>) -> CtResult<Self> {
        let base_url = normalize_base_url(&base_url.into())?;
        let timeout = Duration::from_secs(REQUEST_TIMEOUT_SECS);
        let anon_http = crate::tls::http_client_builder()
            .timeout(timeout)
            .redirect(same_origin_redirects())
            .build()
            .map_err(|e| CtError::Transport(format!("http client build: {e}")))?;
        let probe_http = crate::tls::http_client_builder()
            .timeout(timeout)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| CtError::Transport(format!("probe client build: {e}")))?;
        let refresh = RefreshCoordinator::new(base_url.clone(), store.clone(), anon_http.clone());
        Ok(Self {
            base_url,
            store,
            refresh,
            http_cache: Mutex::new(None),
            anon_http,
            probe_http,
            rejected_credential: Mutex::new(None),
        })
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub fn credential_provenance(&self) -> CtResult<CredentialProvenance> {
        let Some(token) = self.store.load()? else {
            return Ok(CredentialProvenance::Missing);
        };
        let rejected = self
            .rejected_credential
            .lock()
            .map_err(|_| CtError::Store("credential provenance lock poisoned".into()))?;
        if *rejected == Some(self.credential_fingerprint(&token.access_token)) {
            Ok(CredentialProvenance::Stale(self.store.source()))
        } else {
            Ok(CredentialProvenance::Present(self.store.source()))
        }
    }

    fn credential_fingerprint(&self, access: &str) -> [u8; 32] {
        Sha256::new()
            .chain_update(self.base_url.as_bytes())
            .chain_update([0])
            .chain_update(access.as_bytes())
            .finalize()
            .into()
    }

    fn record_auth_failure(&self, access: &str, error: CtError) -> CtError {
        if matches!(
            &error,
            CtError::Auth(_)
                | CtError::ObsoleteCredentials { .. }
                | CtError::Api { status: 401, .. }
        ) && let Ok(mut rejected) = self.rejected_credential.lock()
        {
            *rejected = Some(self.credential_fingerprint(access));
        }
        error
    }

    fn accept_credential(&self, access: &str) {
        if let Ok(mut rejected) = self.rejected_credential.lock()
            && *rejected == Some(self.credential_fingerprint(access))
        {
            *rejected = None;
        }
    }

    // -- authenticated calls -------------------------------------------------

    pub async fn submit_run(
        &self,
        prompt: &str,
        conversation_id: Option<Uuid>,
        selected_agent_reference: Option<&str>,
    ) -> CtResult<SubmittedRun> {
        let prompt_field = prompt
            .parse::<cloudthinker_api::types::SubmitHeadlessRunRequestPrompt>()
            .map_err(|_| CtError::Usage("prompt must be 1–50000 characters".into()))?;
        let selected_agent_reference = selected_agent_reference.map(str::trim);
        if selected_agent_reference.is_some_and(str::is_empty) {
            return Err(CtError::Usage("--agent must not be empty".into()));
        }
        let body = cloudthinker_api::types::SubmitHeadlessRunRequest {
            conversation_id,
            idempotency_key: None,
            prompt: prompt_field,
            selection: cloudthinker_api::types::SavedSelection {
                option_id: "mode:pro".parse().map_err(|error| {
                    CtError::Protocol(format!("invalid default selection: {error}"))
                })?,
                thinking_effort: None,
            },
            source_conversation_id: None,
            selected_agent_reference: selected_agent_reference.map(str::to_owned),
        };
        let submitted = self
            .authed(async |c: cloudthinker_api::Client| {
                c.cli_submit_headless_run(None, &body).await
            })
            .await?;
        Ok(SubmittedRun::from_api(submitted))
    }

    /// Resolve a `--continue` UUID. A known run maps to its conversation; a
    /// 404 means the caller supplied a conversation UUID directly.
    pub async fn resolve_conversation_id(&self, id: Uuid) -> CtResult<Uuid> {
        match self.get_run(id).await {
            Ok(run) => run
                .conversation_id
                .ok_or_else(|| CtError::Protocol(format!("run {id} has no conversation id"))),
            Err(CtError::Api { status: 404, .. }) => Ok(id),
            Err(err) => Err(err),
        }
    }

    /// List recent headless runs for the selected workspace.
    pub async fn list_runs(
        &self,
        limit: u64,
        conversation_id: Option<Uuid>,
    ) -> CtResult<Vec<RunListItem>> {
        let limit = NonZeroU64::new(limit)
            .ok_or_else(|| CtError::Usage("--limit must be between 1 and 50".into()))?;
        let runs = self
            .authed(async |c: cloudthinker_api::Client| {
                c.cli_list_headless_runs(conversation_id.as_ref(), Some(limit), None)
                    .await
            })
            .await?;
        Ok(runs.into_iter().map(RunListItem::from).collect())
    }

    /// Poll one run's current status.
    pub async fn get_run(&self, run_id: Uuid) -> CtResult<RunView> {
        let view = self
            .authed(async |c: cloudthinker_api::Client| c.cli_get_headless_run(&run_id, None).await)
            .await?;
        Ok(RunView::from_api(view))
    }

    /// Resolve the live account and workspace carried by this credential.
    pub async fn whoami(&self) -> CtResult<CliIdentity> {
        let identity = self
            .authed(async |c: cloudthinker_api::Client| c.login_cli_whoami().await)
            .await?;
        Ok(self.identity_from_api(identity))
    }

    // -- unauthenticated calls -----------------------------------------------

    /// Exchange a one-time code + verifier for a token. The backend's one
    /// generic 400 collapses to a single generic auth error (it does not
    /// distinguish burned vs expired codes, CA-CLI-4); any other status
    /// surfaces as-is.
    pub async fn exchange_code(&self, code: &str, verifier: &str) -> CtResult<StoredToken> {
        let body = cloudthinker_api::types::CliTokenRequest {
            code: code
                .parse()
                .map_err(|e| CtError::Login(format!("malformed code: {e}")))?,
            code_verifier: verifier
                .parse()
                .map_err(|e| CtError::Login(format!("malformed verifier: {e}")))?,
        };
        let api = cloudthinker_api::Client::new_with_client(&self.base_url, self.anon_http.clone());
        match api.login_exchange_cli_token(&body).await {
            Ok(rv) => {
                let mut token = StoredToken::from_token(&rv.into_inner());
                if let Ok(identity) = self.whoami_with_access(&token.access_token).await {
                    token.workspace_name = Some(identity.workspace_name);
                }
                Ok(token)
            }
            Err(e) => Err(match to_ct_error(e).await {
                CtError::Api { status: 400, .. } => CtError::Auth(format!(
                    "could not complete login; run `{}` again",
                    login_command(&self.base_url)
                )),
                other => other,
            }),
        }
    }

    async fn whoami_with_access(&self, access: &str) -> CtResult<CliIdentity> {
        let api = self.api_client(access, Duration::from_secs(REQUEST_TIMEOUT_SECS))?;
        let response = match api.login_cli_whoami().await {
            Ok(response) => response,
            Err(error) => return Err(to_ct_error(error).await),
        };
        Ok(self.identity_from_api(response.into_inner()))
    }

    fn identity_from_api(&self, api: cloudthinker_api::types::CliWhoAmIResponse) -> CliIdentity {
        CliIdentity {
            host: origin_of(&self.base_url).unwrap_or_else(|_| self.base_url.clone()),
            user_id: api.user_id,
            user_email: api.user_email,
            workspace_id: api.workspace_id,
            workspace_name: api.workspace_name,
        }
    }

    /// Start an outbound-only login for machines where loopback callbacks fail.
    pub async fn start_device_authorization(&self) -> CtResult<DeviceAuthorization> {
        let api = cloudthinker_api::Client::new_with_client(&self.base_url, self.anon_http.clone());
        let response = match api.login_start_cli_device_authorization().await {
            Ok(response) => response.into_inner(),
            Err(error) => return Err(to_ct_error(error).await),
        };
        let expires_in = positive_duration(response.expires_in, "expires_in")?;
        let interval = positive_duration(response.interval, "interval")?;
        validate_verification_uri(&self.base_url, &response.verification_uri)?;
        Ok(DeviceAuthorization {
            device_code: response.device_code,
            user_code: response.user_code,
            verification_uri: response.verification_uri,
            expires_in,
            interval,
        })
    }

    /// Poll once; pacing and deadline ownership stay in the auth module.
    pub async fn poll_device_token(&self, device_code: &str) -> CtResult<DeviceTokenPoll> {
        use cloudthinker_api::Error as ApiError;
        use cloudthinker_api::types::CliDeviceTokenErrorCode as Code;

        let body = cloudthinker_api::types::CliDeviceTokenRequest {
            device_code: device_code
                .parse()
                .map_err(|e| CtError::Login(format!("malformed device code: {e}")))?,
        };
        let api = cloudthinker_api::Client::new_with_client(&self.base_url, self.anon_http.clone());
        match api.login_poll_cli_device_token(&body).await {
            Ok(response) => {
                let mut token = StoredToken::from_token(&response.into_inner());
                if let Ok(identity) = self.whoami_with_access(&token.access_token).await {
                    token.workspace_name = Some(identity.workspace_name);
                }
                Ok(DeviceTokenPoll::Token(token))
            }
            Err(ApiError::ErrorResponse(response)) => {
                let error = response.into_inner();
                let interval = optional_poll_duration(error.interval)?;
                match error.error {
                    Code::AuthorizationPending => Ok(DeviceTokenPoll::Pending(interval)),
                    Code::SlowDown => Ok(DeviceTokenPoll::SlowDown(interval)),
                    Code::AccessDenied => Err(CtError::LoginDenied),
                    Code::ExpiredToken => Err(device_code_expired(&self.base_url)),
                }
            }
            Err(ApiError::UnexpectedResponse(response))
                if crate::error::is_retryable_status(response.status().as_u16()) =>
            {
                let (_, retry_after) = classify(ApiError::UnexpectedResponse(response)).await;
                Ok(DeviceTokenPoll::Unavailable(
                    retry_after.unwrap_or_default(),
                ))
            }
            Err(ApiError::UnexpectedResponse(response)) => {
                let status = response.status().as_u16();
                Err(CtError::Api {
                    status,
                    detail: response
                        .text()
                        .await
                        .ok()
                        .as_deref()
                        .and_then(crate::error::parse_error_message),
                })
            }
            Err(ApiError::InvalidResponsePayload(_, error)) => Err(CtError::Protocol(format!(
                "unreadable device response: {error}"
            ))),
            Err(ApiError::CommunicationError(error)) => Err(CtError::Transport(error.to_string())),
            Err(ApiError::ResponseBodyError(error)) => Err(CtError::Transport(error.to_string())),
            Err(ApiError::InvalidUpgrade(error)) => Err(CtError::Transport(error.to_string())),
            Err(ApiError::InvalidRequest(message)) | Err(ApiError::Custom(message)) => {
                Err(CtError::Transport(message))
            }
        }
    }

    /// Best-effort server revoke followed by a required local clear.
    pub async fn logout(&self) -> CtResult<LogoutOutcome> {
        let lock = acquire_credential_lock(self.store.clone()).await?;
        let current = match self.store.load_locked(&lock) {
            Ok(current) => current,
            Err(CtError::ObsoleteCredentials { .. }) => None,
            Err(error) => return Err(error),
        };
        let Some(current) = current else {
            return Ok(LogoutOutcome::NothingStored);
        };
        let revoked = self.revoke(current.refresh_token).await;
        self.store.clear_locked(&lock)?;
        if let Err(cause) = revoked {
            return Err(CtError::Logout(format!(
                "server session could not be revoked ({cause}); local credential was cleared"
            )));
        }
        Ok(LogoutOutcome::Cleared)
    }

    /// Revoke every stored workspace session for this host, then clear them.
    pub async fn logout_all(&self) -> CtResult<LogoutOutcome> {
        let lock = acquire_credential_lock(self.store.clone()).await?;
        let tokens = match self.store.load_all_locked(&lock) {
            Ok(tokens) => tokens,
            Err(CtError::ObsoleteCredentials { .. }) => Vec::new(),
            Err(error) => return Err(error),
        };
        if tokens.is_empty() {
            self.store.clear_all_locked(&lock)?;
            return Ok(LogoutOutcome::NothingStored);
        }
        let mut attempted = 0_usize;
        let mut causes = Vec::new();
        for token in tokens {
            if token.refresh_token.is_some() {
                attempted += 1;
                if let Err(cause) = self.revoke(token.refresh_token).await {
                    causes.push(cause);
                }
            }
        }
        self.store.clear_all_locked(&lock)?;
        if !causes.is_empty() {
            return Err(CtError::Logout(format!(
                "server revocation failed for {} of {attempted} sessions ({}); local credentials were cleared",
                causes.len(),
                causes.join("; ")
            )));
        }
        Ok(LogoutOutcome::Cleared)
    }

    /// The current access token, refreshed proactively when it is within the
    /// skew window of expiry. A secret: callers hand it to a bearer header or
    /// to `cloudthinker auth token`'s stdout, never to a log.
    pub async fn access_token(&self) -> CtResult<String> {
        let current = self.store.load()?.ok_or_else(|| {
            CtError::Auth(format!(
                "not logged in; run `{}`",
                login_command(&self.base_url)
            ))
        })?;
        if self.store.refresh_enabled()
            && current.expires_within(RefreshCoordinator::proactive_skew())
        {
            let rotated = self
                .refresh
                .refresh(&current.access_token)
                .await
                .map_err(|error| self.record_auth_failure(&current.access_token, error))?;
            Ok(rotated.access_token)
        } else {
            Ok(current.access_token)
        }
    }

    async fn revoke(&self, refresh_token: Option<String>) -> Result<(), String> {
        let Some(refresh_token) = refresh_token else {
            return Ok(());
        };
        let body = cloudthinker_api::types::RevokeTokenRequest {
            refresh_token: Some(refresh_token),
        };
        match cloudthinker_api::Client::new_with_client(&self.base_url, self.anon_http.clone())
            .login_logout(&body)
            .await
        {
            Ok(_) => Ok(()),
            Err(error) => Err(to_ct_error(error).await.to_string()),
        }
    }

    // -- internals -----------------------------------------------------------

    /// Run `call` with a fresh bearer, retrying exactly once through a refresh
    /// on a 401. Proactive refresh happens in `access_token` before the first
    /// attempt.
    pub(crate) async fn authed<T, F>(&self, call: F) -> CtResult<T>
    where
        F: AsyncFn(
            cloudthinker_api::Client,
        )
            -> Result<cloudthinker_api::ResponseValue<T>, cloudthinker_api::Error<()>>,
    {
        self.authed_with_timeout(Duration::from_secs(REQUEST_TIMEOUT_SECS), call)
            .await
    }

    pub(crate) async fn authed_with_timeout<T, F>(&self, timeout: Duration, call: F) -> CtResult<T>
    where
        F: AsyncFn(
            cloudthinker_api::Client,
        )
            -> Result<cloudthinker_api::ResponseValue<T>, cloudthinker_api::Error<()>>,
    {
        let (access, client) = self.authorized_client(timeout).await?;
        let error = match call(client).await {
            Ok(rv) => {
                self.accept_credential(&access);
                return Ok(rv.into_inner());
            }
            Err(error) => error,
        };
        let (rotated, client) = self.retry_client(&access, error, timeout).await?;
        match call(client).await {
            Ok(rv) => {
                self.accept_credential(&rotated);
                Ok(rv.into_inner())
            }
            Err(error) => Err(self.authed_failure(&rotated, error).await),
        }
    }

    async fn authorized_client(
        &self,
        timeout: Duration,
    ) -> CtResult<(String, cloudthinker_api::Client)> {
        let access = self.access_token().await?;
        let client = self.api_client(&access, timeout)?;
        Ok((access, client))
    }

    async fn retry_client(
        &self,
        access: &str,
        error: cloudthinker_api::Error<()>,
        timeout: Duration,
    ) -> CtResult<(String, cloudthinker_api::Client)> {
        let is_unauthorized = error.status().map(|s| s.as_u16()) == Some(401);
        if !is_unauthorized || !self.store.refresh_enabled() {
            return Err(self.authed_failure(access, error).await);
        }
        let rotated = self
            .refresh
            .refresh(access)
            .await
            .map_err(|error| self.record_auth_failure(access, error))?;
        let client = self.api_client(&rotated.access_token, timeout)?;
        Ok((rotated.access_token, client))
    }

    async fn authed_failure(&self, access: &str, error: cloudthinker_api::Error<()>) -> CtError {
        self.record_auth_failure(access, reclassify(to_ct_error(error).await, &self.base_url))
    }

    /// Build (or reuse) an API client whose bearer header is `access`.
    fn api_client(&self, access: &str, timeout: Duration) -> CtResult<cloudthinker_api::Client> {
        let mut guard = self
            .http_cache
            .lock()
            .map_err(|_| CtError::Transport("http cache poisoned".into()))?;
        let cached = match guard.as_ref() {
            Some((tok, cached_timeout, client)) if tok == access && *cached_timeout == timeout => {
                Some(client.clone())
            }
            _ => None,
        };
        let http = match cached {
            Some(client) => client,
            None => {
                let client = build_authed_http(access, timeout)?;
                *guard = Some((access.to_string(), timeout, client.clone()));
                client
            }
        };
        drop(guard);
        Ok(cloudthinker_api::Client::new_with_client(
            &self.base_url,
            http,
        ))
    }
}

const MAX_REDIRECTS: usize = 10;

fn same_origin_redirects() -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(|attempt| {
        let next = attempt.url();
        let same_origin = attempt.previous().first().is_some_and(|first| {
            first.scheme() == next.scheme()
                && first.host_str() == next.host_str()
                && first.port_or_known_default() == next.port_or_known_default()
        });
        if same_origin && attempt.previous().len() <= MAX_REDIRECTS {
            attempt.follow()
        } else {
            attempt.stop()
        }
    })
}

fn build_authed_http(access: &str, timeout: Duration) -> CtResult<reqwest::Client> {
    let mut headers = reqwest::header::HeaderMap::new();
    let mut value = reqwest::header::HeaderValue::from_str(&format!("Bearer {access}"))
        .map_err(|e| CtError::Auth(format!("invalid token header: {e}")))?;
    value.set_sensitive(true);
    headers.insert(reqwest::header::AUTHORIZATION, value);
    crate::tls::http_client_builder()
        .timeout(timeout)
        .default_headers(headers)
        .build()
        .map_err(|e| CtError::Transport(format!("http client build: {e}")))
}

fn positive_duration(seconds: i64, field: &str) -> CtResult<Duration> {
    let seconds = u64::try_from(seconds)
        .ok()
        .filter(|seconds| *seconds > 0)
        .ok_or_else(|| CtError::Protocol(format!("device response has invalid {field}")))?;
    Ok(Duration::from_secs(seconds))
}

fn optional_poll_duration(seconds: Option<i64>) -> CtResult<Duration> {
    match seconds {
        Some(seconds) => positive_duration(seconds, "interval"),
        None => Ok(Duration::ZERO),
    }
}

fn validate_verification_uri(base_url: &str, verification_uri: &str) -> CtResult<()> {
    if origin_of(base_url)? != origin_of(verification_uri)? {
        return Err(CtError::Protocol(
            "device verification URL does not match the configured CloudThinker origin".into(),
        ));
    }
    Ok(())
}

/// A 401 surfacing here (rather than being cured by refresh) means the user
/// must log in again.
fn reclassify(err: CtError, base_url: &str) -> CtError {
    match err {
        CtError::Api { status: 401, .. } => {
            CtError::Auth(format!("run `{}`", login_command(base_url)))
        }
        CtError::Api {
            status: 403,
            detail,
        } => CtError::Api {
            status: 403,
            detail: Some(format!(
                "{}; ask a workspace admin for access, or run `{}` to authorize another workspace",
                detail.unwrap_or_else(|| "permission denied".into()),
                login_command(base_url)
            )),
        },
        other => other,
    }
}

pub(crate) fn device_code_expired(base_url: &str) -> CtError {
    CtError::Timeout(format!(
        "device code expired; run `{} --device-auth` again",
        login_command(base_url)
    ))
}

pub fn login_command(url: &str) -> String {
    login_command_for(url, &crate::cli_config::effective_default_url())
}

fn login_command_for(url: &str, effective_default: &str) -> String {
    let default_origin = origin_of(effective_default);
    match (origin_of(url), url::Url::parse(url)) {
        (Ok(origin), _) if default_origin.as_ref().is_ok_and(|d| *d == origin) => {
            "cloudthinker login".into()
        }
        (Ok(_), Ok(parsed)) if matches!(parsed.path(), "" | "/") => format!(
            "cloudthinker login --url {}",
            parsed.origin().ascii_serialization()
        ),
        (Ok(_), Ok(_)) => format!("cloudthinker login --url {}", url.trim_end_matches('/')),
        _ => "cloudthinker login".into(),
    }
}

/// Derive the canonical origin (`scheme://host:port`) used as the token-store
/// key, and enforce the transport contract while we're parsing anyway: only
/// `https://` is accepted, except loopback (127.0.0.1 / localhost / ::1) for
/// local dev. Keying by the full origin — not the bare host — means an HTTPS
/// token can never be replayed against a plain-HTTP listener or a different
/// port on the same host.
pub fn origin_of(base_url: &str) -> CtResult<String> {
    let url =
        url::Url::parse(base_url).map_err(|e| CtError::Usage(format!("invalid --url: {e}")))?;
    let scheme = url.scheme();
    if scheme != "https" && !(scheme == "http" && is_loopback_host(&url)) {
        return Err(CtError::Usage(
            "--url must be https:// (plain http is only allowed for loopback: 127.0.0.1, localhost, ::1)".into(),
        ));
    }
    let host = url
        .host_str()
        .ok_or_else(|| CtError::Usage("--url has no host".into()))?;
    let port = url
        .port_or_known_default()
        .ok_or_else(|| CtError::Usage("--url has no resolvable port".into()))?;
    Ok(format!("{scheme}://{host}:{port}"))
}

fn normalize_base_url(raw: &str) -> CtResult<String> {
    origin_of(raw)?;
    Ok(raw.trim_end_matches('/').to_string())
}

fn is_loopback_host(url: &url::Url) -> bool {
    match url.host() {
        Some(url::Host::Domain(d)) => d.eq_ignore_ascii_case("localhost"),
        Some(url::Host::Ipv4(addr)) => addr.is_loopback(),
        Some(url::Host::Ipv6(addr)) => addr.is_loopback(),
        None => false,
    }
}

/// Resolve the read/refresh store: env override if `CLOUDTHINKER_TOKEN` is set,
/// otherwise the credentials file. Opening the file the first time after the
/// 0.5.3 upgrade migrates a keyring login into it (`FileStore::open_default`).
pub fn resolve_store(base_url: &str, workspace: Option<&str>) -> CtResult<Arc<dyn TokenStore>> {
    if let Some(env) = EnvTokenStore::from_env() {
        if workspace.is_some() {
            return Err(CtError::Usage(format!(
                "--workspace cannot be used with {TOKEN_ENV_VAR}"
            )));
        }
        return Ok(Arc::new(env));
    }
    file_store(base_url, workspace)
}

/// The persistent store used by `login`/`logout` (never the env override — those
/// commands must write real credentials).
pub fn persistent_store(base_url: &str, workspace: Option<&str>) -> CtResult<Arc<dyn TokenStore>> {
    file_store(base_url, workspace)
}

fn file_store(base_url: &str, workspace: Option<&str>) -> CtResult<Arc<dyn TokenStore>> {
    let selector = workspace.map_or(WorkspaceSelector::Active, |value| {
        WorkspaceSelector::IdOrName(value.to_string())
    });
    Ok(Arc::new(FileStore::open_default(
        origin_of(base_url)?,
        selector,
    )?))
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::*;
    use crate::auth::refresh::PROACTIVE_REFRESH_SKEW_SECS;
    use crate::test_support::{MockTokenStore, stored, token_json};
    use wiremock::matchers::{body_json, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn cancelled_run_status_is_terminal_and_serializes_as_cancelled() {
        let status = RunStatus::from(cloudthinker_api::types::AgentRunStatus::Cancelled);

        assert_eq!(status, RunStatus::Cancelled);
        assert!(status.is_terminal());
        assert_eq!(serde_json::to_value(status).unwrap(), "cancelled");
    }

    #[test]
    fn local_run_brief_json_does_not_expose_server_memory_paths() {
        let brief = CyberRunBrief {
            run_id: Uuid::from_u128(1),
            app_id: Uuid::from_u128(2),
            app_name: "ct32-local".into(),
            conversation_id: Some(Uuid::from_u128(3)),
            result: CyberRunResult::Running,
            execution_host: CyberExecutionHost::Local,
            target: "http://ct32.localhost:8088".into(),
            frameworks: vec!["owasp_api".into()],
            mode: "white".into(),
            intensity: "full".into(),
            scan_mode: "full".into(),
            report_preferences: cloudthinker_api::types::CyberReportPreferences::default(),
            report_reference: Some(cloudthinker_api::types::LocalRunReportReferencePublic {
                source_id: Uuid::from_u128(4),
                name: "house-style.pdf".into(),
                sha256: "a".repeat(64),
                content_type: "application/pdf".into(),
                download_url: "https://objects.example.test/signed".into(),
            }),
            started_at: DateTime::from_timestamp(0, 0).unwrap(),
            finished_at: None,
            targets: vec![],
            local_workspace: Some(LocalCyberWorkspace {
                workspace_root: "/private/runs/1/workspace".into(),
                evidence_root: "/private/runs/1/workspace/evidence".into(),
                workflow_root: "/private/runs/1/workspace/workflow".into(),
                manifest: "/private/runs/1/workspace/manifest.json".into(),
            }),
        };

        let json = serde_json::to_value(brief).unwrap();

        assert_eq!(json["execution_host"], "local");
        assert_eq!(
            json["local_workspace"]["workspace_root"],
            "/private/runs/1/workspace"
        );
        assert_eq!(
            json["local_workspace"]["evidence_root"],
            "/private/runs/1/workspace/evidence"
        );
        assert_eq!(
            json["local_workspace"]["workflow_root"],
            "/private/runs/1/workspace/workflow"
        );
        assert!(json["local_workspace"].get("memory_root").is_none());
        assert!(json.get("memory_root").is_none());
        assert!(json.get("memory_mount").is_none());
        assert_eq!(json["report_reference"]["sha256"], "a".repeat(64));
        assert_eq!(
            json["report_reference"]["download_url"],
            "https://objects.example.test/signed"
        );
        assert!(json["report_reference"].get("storage_key").is_none());
    }

    #[test]
    fn only_candidate_blocked_and_skipped_coverage_require_a_reason() {
        assert!(CoverageStatus::Candidate.requires_reason());
        assert!(CoverageStatus::Blocked.requires_reason());
        assert!(CoverageStatus::SkippedWithReason.requires_reason());
        assert!(!CoverageStatus::Covered.requires_reason());
    }

    fn run_status_json(status: &str, answer: Option<&str>) -> serde_json::Value {
        serde_json::json!({
            "run_id": "11111111-1111-4111-8111-111111111111",
            "conversation_id": "22222222-2222-4222-8222-222222222222",
            "status": status,
            "answer": answer,
            "message": null,
            "failure_kind": null,
            "web_url": "https://app.example.com/c/22222222-2222-4222-8222-222222222222",
            "created_at": "2026-07-20T00:00:00Z",
            "start_time": null,
            "end_time": null,
        })
    }

    fn api_error_json(code: &str, message: &str) -> serde_json::Value {
        serde_json::json!({
            "error": {
                "code": code,
                "message": message,
                "retryable": false,
                "field_errors": null,
            },
            "request_id": "request-test",
            "detail": message,
        })
    }

    fn valid_verifier() -> String {
        "a".repeat(43)
    }

    struct MultiLogoutStore {
        tokens: Mutex<Vec<StoredToken>>,
        cleared: AtomicBool,
    }

    impl MultiLogoutStore {
        fn new(tokens: Vec<StoredToken>) -> Self {
            Self {
                tokens: Mutex::new(tokens),
                cleared: AtomicBool::new(false),
            }
        }
    }

    impl TokenStore for MultiLogoutStore {
        fn load(&self) -> CtResult<Option<StoredToken>> {
            Ok(self.tokens.lock().unwrap().last().cloned())
        }

        fn load_all(&self) -> CtResult<Vec<StoredToken>> {
            Ok(self.tokens.lock().unwrap().clone())
        }

        fn save(&self, _token: &StoredToken) -> CtResult<()> {
            Ok(())
        }

        fn clear(&self) -> CtResult<()> {
            self.tokens.lock().unwrap().pop();
            Ok(())
        }

        fn clear_all(&self) -> CtResult<()> {
            self.tokens.lock().unwrap().clear();
            self.cleared.store(true, Ordering::SeqCst);
            Ok(())
        }

        fn refresh_enabled(&self) -> bool {
            true
        }
    }

    // [ISSUE-1a/1b]: the store key is the full origin, so switching scheme or
    // port never reuses another origin's token.
    #[test]
    fn origin_of_scheme_changes_the_key() {
        // Both sides are individually valid (http is allowed here only because
        // the host is loopback) yet must key to different origins.
        let https = origin_of("https://127.0.0.1:9443").unwrap();
        let http = origin_of("http://127.0.0.1:9443").unwrap();
        assert_ne!(https, http);
    }

    #[test]
    fn origin_of_port_changes_the_key() {
        let default_port = origin_of("https://app.example.com").unwrap();
        let explicit_port = origin_of("https://app.example.com:8443").unwrap();
        assert_ne!(default_port, explicit_port);
    }

    // [ISSUE-1c]: a non-loopback plain-http URL is rejected outright — an
    // HTTPS token must never be requested over an unencrypted connection to a
    // real host.
    #[test]
    fn origin_of_rejects_non_loopback_http() {
        let err = origin_of("http://app.example.com").unwrap_err();
        assert!(matches!(err, CtError::Usage(_)), "got {err:?}");
    }

    // [ISSUE-1d]: loopback stays the local-dev escape hatch for plain http.
    #[test]
    fn origin_of_allows_loopback_http() {
        assert!(origin_of("http://127.0.0.1:8000").is_ok());
        assert!(origin_of("http://localhost:8000").is_ok());
        assert!(origin_of("http://[::1]:8000").is_ok());
    }

    // CA-CLI-1 (login happy, server half): a valid code + verifier exchange
    // yields a stored token carrying the approved workspace. The loopback half
    // is covered by `auth::pkce::tests::happy_callback_yields_code`.
    #[tokio::test]
    async fn ca_cli_1_exchange_success_returns_stored_token() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/login/cli/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "fresh-access",
                "refresh_token": "fresh-refresh",
                "token_type": "bearer",
                "workspace_id": "55555555-5555-4555-8555-555555555555",
            })))
            .mount(&server)
            .await;

        let client = CtClient::new(server.uri(), Arc::new(MockTokenStore::new(None))).unwrap();
        let token = client
            .exchange_code("one-time-code", &valid_verifier())
            .await
            .unwrap();
        assert_eq!(token.access_token, "fresh-access");
        assert_eq!(token.refresh_token.as_deref(), Some("fresh-refresh"));
        assert_eq!(
            token.workspace_id,
            Some(Uuid::parse_str("55555555-5555-4555-8555-555555555555").unwrap())
        );
    }

    #[tokio::test]
    async fn token_exchange_does_not_follow_a_cross_origin_redirect() {
        let server = MockServer::start().await;
        let elsewhere = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/login/cli/token"))
            .respond_with(ResponseTemplate::new(307).insert_header(
                "location",
                format!("{}/api/v1/login/cli/token", elsewhere.uri()),
            ))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&elsewhere)
            .await;

        let client = CtClient::new(server.uri(), Arc::new(MockTokenStore::new(None))).unwrap();
        let result = client
            .exchange_code("one-time-code", &valid_verifier())
            .await;

        assert!(result.is_err());
        assert!(elsewhere.received_requests().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn base_url_trailing_slashes_normalize_to_the_bare_origin() {
        let server = MockServer::start().await;
        let store = || Arc::new(MockTokenStore::new(None));
        let bare = CtClient::new(server.uri(), store()).unwrap();
        let one = CtClient::new(format!("{}/", server.uri()), store()).unwrap();
        let many = CtClient::new(format!("{}///", server.uri()), store()).unwrap();
        assert_eq!(bare.base_url(), server.uri());
        assert_eq!(one.base_url(), server.uri());
        assert_eq!(many.base_url(), server.uri());
    }

    #[test]
    fn base_url_must_be_a_usable_origin() {
        let store = || Arc::new(MockTokenStore::new(None));
        for raw in ["", "ftp://app.example.com"] {
            let err = CtClient::new(raw, store()).err().expect(raw);
            assert!(matches!(err, CtError::Usage(_)), "{raw}: got {err:?}");
        }
    }

    #[tokio::test]
    async fn trailing_slash_base_url_requests_a_single_slash_path() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/cli/whoami"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "user_id": "22222222-2222-4222-8222-222222222222",
                "user_email": "duc@example.com",
                "workspace_id": "11111111-1111-4111-8111-111111111111",
                "workspace_name": "Production",
                "organization_id": null,
            })))
            .expect(1)
            .mount(&server)
            .await;

        let client = CtClient::new(
            format!("{}/", server.uri()),
            Arc::new(MockTokenStore::new(Some(stored("access", "r")))),
        )
        .unwrap();
        let identity = client.whoami().await.unwrap();
        assert_eq!(identity.workspace_name, "Production");
    }

    #[tokio::test]
    async fn exchange_code_surfaces_a_non_400_status() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/login/cli/token"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;

        let client = CtClient::new(server.uri(), Arc::new(MockTokenStore::new(None))).unwrap();
        let err = client
            .exchange_code("one-time-code", &valid_verifier())
            .await
            .unwrap_err();
        assert!(
            matches!(err, CtError::Api { status: 404, .. }),
            "got {err:?}"
        );
    }

    // CA-CLI-4: the backend's one generic 400 collapses to a generic auth error.
    #[tokio::test]
    async fn ca_cli_4_exchange_generic_400_maps_to_auth() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/login/cli/token"))
            .respond_with(
                ResponseTemplate::new(400)
                    .set_body_json(api_error_json("validation_error", "invalid")),
            )
            .mount(&server)
            .await;

        let client = CtClient::new(server.uri(), Arc::new(MockTokenStore::new(None))).unwrap();
        let err = client
            .exchange_code("one-time-code", &valid_verifier())
            .await
            .unwrap_err();
        assert!(matches!(err, CtError::Auth(_)), "got {err:?}");
    }

    #[tokio::test]
    async fn logout_all_revokes_every_workspace_before_clearing_local_credentials() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/login/logout"))
            .and(wiremock::matchers::header_exists("api-version"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "message": "Logged out successfully"
            })))
            .expect(2)
            .mount(&server)
            .await;
        let mut first = stored("one", "refresh-one");
        first.workspace_id = Some(Uuid::from_u128(1));
        let mut second = stored("two", "refresh-two");
        second.workspace_id = Some(Uuid::from_u128(2));
        let store = Arc::new(MultiLogoutStore::new(vec![first, second]));
        let client = CtClient::new(server.uri(), store.clone()).unwrap();

        client.logout_all().await.unwrap();

        assert!(store.cleared.load(Ordering::SeqCst));
        assert!(store.load_all().unwrap().is_empty());
    }

    #[tokio::test]
    async fn logout_all_clears_local_credentials_and_reports_failed_revocations() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/login/logout"))
            .respond_with(ResponseTemplate::new(503))
            .expect(2)
            .mount(&server)
            .await;
        let mut first = stored("one", "refresh-one");
        first.workspace_id = Some(Uuid::from_u128(1));
        let mut second = stored("two", "refresh-two");
        second.workspace_id = Some(Uuid::from_u128(2));
        let store = Arc::new(MultiLogoutStore::new(vec![first, second]));
        let client = CtClient::new(server.uri(), store.clone()).unwrap();

        let error = client.logout_all().await.unwrap_err();

        assert!(matches!(error, CtError::Logout(message) if message.contains("2 of 2")));
        assert!(store.cleared.load(Ordering::SeqCst));
        assert!(store.load_all().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_403_keeps_the_server_reason_and_leaves_the_login_usable() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/cli/whoami"))
            .respond_with(ResponseTemplate::new(403).set_body_json(serde_json::json!({
                "detail": "Unauthorized workspace access"
            })))
            .mount(&server)
            .await;
        let store = Arc::new(MockTokenStore::new(Some(stored("live", "refresh"))));
        let client = CtClient::new(server.uri(), store).unwrap();

        let error = client.whoami().await.unwrap_err();

        assert!(
            matches!(&error, CtError::Api { status: 403, detail: Some(detail) }
                if detail.starts_with("Unauthorized workspace access; ask a workspace admin")),
            "{error:?}"
        );
        assert_eq!(
            client.credential_provenance().unwrap(),
            CredentialProvenance::Present(crate::CredentialSource::Stored)
        );
    }

    #[tokio::test]
    async fn a_device_poll_server_error_waits_instead_of_ending_the_login() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/login/cli/device/token"))
            .respond_with(ResponseTemplate::new(503).insert_header("retry-after", "7"))
            .mount(&server)
            .await;
        let client = CtClient::new(server.uri(), Arc::new(MockTokenStore::new(None))).unwrap();

        let polled = client.poll_device_token(&"d".repeat(43)).await.unwrap();

        assert!(matches!(
            polled,
            DeviceTokenPoll::Unavailable(delay) if delay == Duration::from_secs(7)
        ));
    }

    #[test]
    fn login_command_names_every_host_but_the_default() {
        let login = |url| login_command_for(url, DEFAULT_BASE_URL);
        assert_eq!(login(DEFAULT_BASE_URL), "cloudthinker login");
        assert_eq!(
            login("https://app.cloudthinker.io:443/"),
            "cloudthinker login"
        );
        assert_eq!(
            login("https://dev.cloudthinker.io"),
            "cloudthinker login --url https://dev.cloudthinker.io"
        );
        assert_eq!(
            login("https://dev.cloudthinker.io:443"),
            "cloudthinker login --url https://dev.cloudthinker.io"
        );
        assert_eq!(
            login("http://127.0.0.1:8091"),
            "cloudthinker login --url http://127.0.0.1:8091"
        );
        assert_eq!(login("origin"), "cloudthinker login");
    }

    #[test]
    fn login_command_measures_against_the_remembered_origin() {
        let remembered = "https://dev.cloudthinker.io";
        assert_eq!(
            login_command_for("https://dev.cloudthinker.io:443", remembered),
            "cloudthinker login"
        );
        assert_eq!(
            login_command_for(DEFAULT_BASE_URL, remembered),
            "cloudthinker login --url https://app.cloudthinker.io"
        );
        assert_eq!(
            login_command_for("http://127.0.0.1:8091", remembered),
            "cloudthinker login --url http://127.0.0.1:8091"
        );
    }

    // CA-CLI-19: device start preserves the URL, human code, TTL, and pacing
    // contract without requiring an existing bearer token.
    #[tokio::test]
    async fn ca_cli_19_device_start_maps_authorization_contract() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/login/cli/device/start"))
            .and(wiremock::matchers::body_bytes(Vec::<u8>::new()))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "device_code": "d".repeat(43),
                "user_code": "BCDF-GHJK",
                "verification_uri": format!("{}/auth/cli", server.uri()),
                "expires_in": 600,
                "interval": 5,
            })))
            .mount(&server)
            .await;

        let client = CtClient::new(server.uri(), Arc::new(MockTokenStore::new(None))).unwrap();
        let authorization = client.start_device_authorization().await.unwrap();

        assert_eq!(authorization.user_code, "BCDF-GHJK");
        assert_eq!(authorization.expires_in, Duration::from_secs(600));
        assert_eq!(authorization.interval, Duration::from_secs(5));
    }

    // CA-CLI-20: pending and slow-down stay non-terminal and carry the next
    // server-mandated delay; the command never has to parse an error body.
    #[tokio::test]
    async fn ca_cli_20_device_poll_maps_pending_and_slow_down() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/login/cli/device/token"))
            .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
                "error": "authorization_pending",
                "interval": 5,
            })))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v1/login/cli/device/token"))
            .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
                "error": "slow_down",
                "interval": 10,
            })))
            .mount(&server)
            .await;

        let client = CtClient::new(server.uri(), Arc::new(MockTokenStore::new(None))).unwrap();
        let pending = client.poll_device_token(&"d".repeat(43)).await.unwrap();
        let slowed = client.poll_device_token(&"d".repeat(43)).await.unwrap();

        assert!(matches!(
            pending,
            DeviceTokenPoll::Pending(delay) if delay == Duration::from_secs(5)
        ));
        assert!(matches!(
            slowed,
            DeviceTokenPoll::SlowDown(delay) if delay == Duration::from_secs(10)
        ));
    }

    // CA-CLI-21: approval returns the same stored-token shape as PKCE; denial
    // remains a stable terminal login error.
    #[tokio::test]
    async fn ca_cli_21_device_poll_maps_approval_and_denial() {
        let approved_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/login/cli/device/token"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(token_json("device-access", "device-refresh")),
            )
            .mount(&approved_server)
            .await;
        let approved_client =
            CtClient::new(approved_server.uri(), Arc::new(MockTokenStore::new(None))).unwrap();
        let approved = approved_client
            .poll_device_token(&"d".repeat(43))
            .await
            .unwrap();
        assert!(matches!(
            approved,
            DeviceTokenPoll::Token(token) if token.access_token == "device-access"
        ));

        let denied_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/login/cli/device/token"))
            .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
                "error": "access_denied",
            })))
            .mount(&denied_server)
            .await;
        let denied_client =
            CtClient::new(denied_server.uri(), Arc::new(MockTokenStore::new(None))).unwrap();
        let denied = denied_client
            .poll_device_token(&"d".repeat(43))
            .await
            .unwrap_err();
        assert!(matches!(denied, CtError::LoginDenied));
    }

    // CA-CLI-14: the secret-gate 422 returns the backend `detail` verbatim, and
    // maps to the usage exit code upstream.
    #[tokio::test]
    async fn ca_cli_14_submit_secret_gate_422_returns_detail() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/cli/runs"))
            .respond_with(ResponseTemplate::new(422).set_body_json(serde_json::json!({
                "detail": "prompt contains a secret"
            })))
            .mount(&server)
            .await;

        let client = CtClient::new(
            server.uri(),
            Arc::new(MockTokenStore::new(Some(stored("access", "r")))),
        )
        .unwrap();
        let err = client
            .submit_run("here is my aws key", None, None)
            .await
            .unwrap_err();
        match err {
            CtError::Api { status, detail } => {
                assert_eq!(status, 422);
                assert_eq!(detail.as_deref(), Some("prompt contains a secret"));
            }
            other => panic!("expected Api 422, got {other:?}"),
        }
    }

    // [ISSUE-3]: a malformed *success* body (202, but not JSON at all) must not
    // be confused with the 422 secret-gate shape — there's no recognizable
    // `{"detail": ...}` here, so it classifies as a protocol fault, not a
    // usage error a script should treat as bad input.
    #[tokio::test]
    async fn ca_cli_14_malformed_success_body_is_protocol_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/cli/runs"))
            .respond_with(ResponseTemplate::new(202).set_body_string("not json at all"))
            .mount(&server)
            .await;

        let client = CtClient::new(
            server.uri(),
            Arc::new(MockTokenStore::new(Some(stored("access", "r")))),
        )
        .unwrap();
        let err = client
            .submit_run("here is my prompt", None, None)
            .await
            .unwrap_err();
        assert!(matches!(err, CtError::Protocol(_)), "got {err:?}");
    }

    #[tokio::test]
    async fn empty_prompt_is_rejected_before_any_request() {
        let server = MockServer::start().await;
        let client = CtClient::new(
            server.uri(),
            Arc::new(MockTokenStore::new(Some(stored("access", "r")))),
        )
        .unwrap();
        let err = client.submit_run("", None, None).await.unwrap_err();
        assert!(matches!(err, CtError::Usage(_)), "got {err:?}");
    }

    fn expiring_in(seconds: i64) -> StoredToken {
        let mut token = stored("live-access", "r");
        token.expires_at = Some(Utc::now() + chrono::Duration::seconds(seconds));
        token
    }

    #[tokio::test]
    async fn ca_ad_12_provenance_tracks_rejection_and_external_replacement() {
        use crate::auth::store::CredentialSource;
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/cli/whoami"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v1/login/refresh"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;
        let store = Arc::new(MockTokenStore::new(None));
        let client = CtClient::new(server.uri(), store.clone()).unwrap();
        assert_eq!(
            client.credential_provenance().unwrap(),
            CredentialProvenance::Missing
        );
        store.save(&stored("first", "refresh")).unwrap();
        assert_eq!(
            client.credential_provenance().unwrap(),
            CredentialProvenance::Present(CredentialSource::Stored)
        );
        assert!(client.whoami().await.is_err());
        assert_eq!(
            client.credential_provenance().unwrap(),
            CredentialProvenance::Stale(CredentialSource::Stored)
        );
        store.save(&stored("replacement", "refresh")).unwrap();
        assert_eq!(
            client.credential_provenance().unwrap(),
            CredentialProvenance::Present(CredentialSource::Stored)
        );
        assert!(!format!("{:?}", client.credential_provenance().unwrap()).contains("replacement"));
    }

    // `cloudthinker auth token` hands this value to another process, so an
    // access token outside the skew window is returned untouched.
    #[tokio::test]
    async fn access_token_returns_the_stored_token_when_fresh() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/login/refresh"))
            .respond_with(ResponseTemplate::new(200).set_body_json(token_json("rotated", "r2")))
            .expect(0)
            .mount(&server)
            .await;
        let store = Arc::new(MockTokenStore::new(Some(expiring_in(
            PROACTIVE_REFRESH_SKEW_SECS + 300,
        ))));

        let client = CtClient::new(server.uri(), store).unwrap();

        assert_eq!(client.access_token().await.unwrap(), "live-access");
    }

    // Inside the skew window the caller must receive the rotated token, not one
    // that expires between this print and the consumer's first request.
    #[tokio::test]
    async fn access_token_returns_the_rotated_token_inside_the_skew_window() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/login/refresh"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(token_json("rotated-access", "rotated-refresh")),
            )
            .expect(1)
            .mount(&server)
            .await;
        let store = Arc::new(MockTokenStore::new(Some(expiring_in(
            PROACTIVE_REFRESH_SKEW_SECS - 30,
        ))));

        let client = CtClient::new(server.uri(), store).unwrap();

        assert_eq!(client.access_token().await.unwrap(), "rotated-access");
    }

    // No credential is an auth error, which `auth token` reports as exit code 3.
    #[tokio::test]
    async fn access_token_without_a_credential_is_an_auth_error() {
        let server = MockServer::start().await;

        let client = CtClient::new(server.uri(), Arc::new(MockTokenStore::new(None))).unwrap();

        let err = client.access_token().await.unwrap_err();
        assert!(matches!(err, CtError::Auth(_)), "got {err:?}");
    }

    // CA-CLI-18: with a read-only (env) credential, a 401 does NOT trigger a
    // refresh — it surfaces as auth directly.
    #[tokio::test]
    async fn ca_cli_18_env_token_401_does_not_refresh() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(
                "/api/v1/cli/runs/33333333-3333-4333-8333-333333333333",
            ))
            .respond_with(ResponseTemplate::new(401).set_body_json(serde_json::json!({
                "detail": "Authentication required."
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v1/login/refresh"))
            .respond_with(ResponseTemplate::new(200).set_body_json(token_json("x", "y")))
            .expect(0)
            .mount(&server)
            .await;

        let client = CtClient::new(
            server.uri(),
            Arc::new(MockTokenStore::read_only(Some(stored("env-access", "")))),
        )
        .unwrap();
        let run_id = Uuid::parse_str("33333333-3333-4333-8333-333333333333").unwrap();
        let err = client.get_run(run_id).await.unwrap_err();
        assert!(matches!(err, CtError::Auth(_)), "got {err:?}");
    }

    #[tokio::test]
    async fn legacy_401_refreshes_once_then_retries() {
        let server = MockServer::start().await;
        // First GET 401 (once), then 200 succeeded.
        Mock::given(method("GET"))
            .and(path(
                "/api/v1/cli/runs/33333333-3333-4333-8333-333333333333",
            ))
            .respond_with(ResponseTemplate::new(401).set_body_json(serde_json::json!({
                "detail": "Authentication required."
            })))
            .up_to_n_times(1)
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(
                "/api/v1/cli/runs/33333333-3333-4333-8333-333333333333",
            ))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(run_status_json("succeeded", Some("done"))),
            )
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v1/login/refresh"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(token_json("fresh-access", "fresh-r")),
            )
            .expect(1)
            .mount(&server)
            .await;

        let client = CtClient::new(
            server.uri(),
            Arc::new(MockTokenStore::new(Some(stored("stale-access", "r")))),
        )
        .unwrap();
        let run_id = Uuid::parse_str("33333333-3333-4333-8333-333333333333").unwrap();
        let view = client.get_run(run_id).await.unwrap();
        assert_eq!(view.status, RunStatus::Succeeded);
        assert_eq!(view.answer.as_deref(), Some("done"));
    }

    #[tokio::test]
    async fn failed_retry_after_refresh_keeps_the_rotated_credential() {
        use crate::auth::store::TokenStore;

        let server = MockServer::start().await;
        let run_path = "/api/v1/cli/runs/33333333-3333-4333-8333-333333333333";
        Mock::given(method("GET"))
            .and(path(run_path))
            .respond_with(ResponseTemplate::new(401).set_body_json(serde_json::json!({
                "detail": "Authentication required."
            })))
            .up_to_n_times(1)
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(run_path))
            .respond_with(ResponseTemplate::new(503))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v1/login/refresh"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(token_json("fresh-access", "fresh-r")),
            )
            .expect(1)
            .mount(&server)
            .await;

        let store = Arc::new(MockTokenStore::new(Some(stored("stale-access", "r"))));
        let client = CtClient::new(server.uri(), store.clone()).unwrap();
        let run_id = Uuid::parse_str("33333333-3333-4333-8333-333333333333").unwrap();
        assert!(client.get_run(run_id).await.is_err());
        let saved = store.load().unwrap().unwrap();
        assert_eq!(saved.access_token, "fresh-access");
        assert_eq!(saved.refresh_token.as_deref(), Some("fresh-r"));
    }

    // CA-CLI-10 (data half): a SUCCEEDED run surfaces the extracted answer.
    #[tokio::test]
    async fn get_run_maps_succeeded_answer() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(
                "/api/v1/cli/runs/33333333-3333-4333-8333-333333333333",
            ))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(run_status_json("succeeded", Some("the answer"))),
            )
            .mount(&server)
            .await;

        let client = CtClient::new(
            server.uri(),
            Arc::new(MockTokenStore::new(Some(stored("access", "r")))),
        )
        .unwrap();
        let run_id = Uuid::parse_str("33333333-3333-4333-8333-333333333333").unwrap();
        let view = client.get_run(run_id).await.unwrap();
        assert!(view.status.is_terminal());
        assert_eq!(view.answer.as_deref(), Some("the answer"));
    }

    // CA-CLI-16 (404 half): an older detail-only response preserves its status.
    #[tokio::test]
    async fn ca_cli_16_unknown_run_is_404() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(
                "/api/v1/cli/runs/44444444-4444-4444-8444-444444444444",
            ))
            .respond_with(ResponseTemplate::new(404).set_body_json(serde_json::json!({
                "detail": "Run not found"
            })))
            .mount(&server)
            .await;

        let client = CtClient::new(
            server.uri(),
            Arc::new(MockTokenStore::new(Some(stored("access", "r")))),
        )
        .unwrap();
        let run_id = Uuid::parse_str("44444444-4444-4444-8444-444444444444").unwrap();
        let err = client.get_run(run_id).await.unwrap_err();
        match err {
            CtError::Api { status, detail } => {
                assert_eq!(status, 404);
                assert_eq!(detail.as_deref(), Some("Run not found"));
            }
            other => panic!("expected Api 404, got {other:?}"),
        }
    }

    fn review_detail_json() -> serde_json::Value {
        serde_json::json!({
            "id": "33333333-3333-4333-8333-333333333333",
            "created_at": "2026-07-20T00:00:00Z",
            "updated_at": "2026-07-20T00:00:00Z",
            "mr_iid": 42,
            "mr_state": "open",
            "provider": "gitlab",
            "repository_name": "my-repo",
            "repository_path": "group/my-repo",
            "review_status": "review_complete",
            "severity_counts": {"critical": 1, "high": 2, "medium": 0, "low": 0},
            "title": "Fix the bug",
            "verdict": "changes_requested",
            "findings_count": 2,
            "url": "https://gitlab.com/group/my-repo/-/merge_requests/42",
            "findings": [
                {
                    "id": "44444444-4444-4444-8444-444444444444",
                    "finding_index": 0,
                    "issue_title": "possible SQL injection",
                    "issue_description": "unsanitized input reaches the query",
                    "severity": "high",
                    "severity_emoji": "🟠",
                    "provider": "gitlab",
                    "file_path": "app/db.py",
                    "line_number": 10,
                    "category": "security",
                    "resolved": false,
                    "acknowledged": false,
                    "withdrawn": false,
                    "resolved_at": null,
                    "resolved_by": null,
                    "external_comment_id": null,
                    "external_note_id": null,
                    "side": null,
                    "specialist": null,
                    "suggested_fix": null,
                    "comment_posted_at": null,
                    "created_at": "2026-07-20T00:00:00Z",
                    "updated_at": "2026-07-20T00:00:00Z",
                },
                {
                    "id": "55555555-5555-4555-8555-555555555555",
                    "finding_index": 1,
                    "issue_title": "unbounded query",
                    "issue_description": "missing LIMIT clause",
                    "severity": "critical",
                    "severity_emoji": "🔴",
                    "provider": "gitlab",
                    "file_path": "app/db.py",
                    "line_number": 20,
                    "category": "performance",
                    "resolved": false,
                    "acknowledged": false,
                    "withdrawn": false,
                    "resolved_at": null,
                    "resolved_by": null,
                    "external_comment_id": null,
                    "external_note_id": null,
                    "side": null,
                    "specialist": null,
                    "suggested_fix": null,
                    "comment_posted_at": null,
                    "created_at": "2026-07-20T00:00:00Z",
                    "updated_at": "2026-07-20T00:00:00Z",
                },
            ],
        })
    }

    // CA-RV-1: a 200 lookup maps every field, and findings are re-ordered
    // worst-severity first (the fixture lists "high" before "critical").
    #[tokio::test]
    async fn ca_rv_1_lookup_200_maps_fields() {
        for (status_wire, expected_status, verdict_wire, expected_verdict) in [
            (
                "review_complete",
                ReviewStatus::ReviewComplete,
                "changes_requested",
                ReviewVerdict::ChangesRequested,
            ),
            (
                "stopped",
                ReviewStatus::Stopped,
                "stopped",
                ReviewVerdict::Stopped,
            ),
        ] {
            let server = MockServer::start().await;
            let mut payload = review_detail_json();
            payload["review_status"] = status_wire.into();
            payload["verdict"] = verdict_wire.into();
            Mock::given(method("GET"))
                .and(path("/api/v1/code-review/merge-requests/lookup"))
                .and(query_param("mr_iid", "42"))
                .and(query_param("project_path", "group/my-repo"))
                .and(query_param("provider", "gitlab"))
                .respond_with(ResponseTemplate::new(200).set_body_json(payload))
                .mount(&server)
                .await;

            let client = CtClient::new(
                server.uri(),
                Arc::new(MockTokenStore::new(Some(stored("access", "r")))),
            )
            .unwrap();
            let coords = MrCoordinates {
                provider: MrProvider::Gitlab,
                project_path: "group/my-repo".to_string(),
                mr_iid: NonZeroU64::new(42).unwrap(),
            };
            let view = client.lookup_review(&coords).await.unwrap();

            assert_eq!(view.mr_iid, 42);
            assert_eq!(view.status, expected_status);
            assert!(view.status.is_terminal());
            assert_eq!(view.verdict, expected_verdict);
            assert_eq!(view.findings_count, 2);
            assert_eq!(view.title, "Fix the bug");
            assert_eq!(view.provider, "gitlab");
            assert_eq!(view.repository_path.as_deref(), Some("group/my-repo"));
            assert_eq!(view.severity_counts.critical, 1);
            assert_eq!(view.severity_counts.high, 2);
            assert_eq!(view.findings.len(), 2);
            assert_eq!(view.findings[0].severity, "critical", "worst-first");
            assert_eq!(view.findings[1].severity, "high");
            assert_eq!(serde_json::to_value(view.status).unwrap(), status_wire);
            assert_eq!(serde_json::to_value(view.verdict).unwrap(), verdict_wire);
        }
    }

    // CA-RV-SP4: a legacy unknown-coordinate response preserves its 404 status.
    #[tokio::test]
    async fn ca_rv_sp4_unknown_coordinates_is_404() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/code-review/merge-requests/lookup"))
            .respond_with(ResponseTemplate::new(404).set_body_json(serde_json::json!({
                "detail": "Merge request not found"
            })))
            .mount(&server)
            .await;

        let client = CtClient::new(
            server.uri(),
            Arc::new(MockTokenStore::new(Some(stored("access", "r")))),
        )
        .unwrap();
        let coords = MrCoordinates {
            provider: MrProvider::Github,
            project_path: "owner/repo".to_string(),
            mr_iid: NonZeroU64::new(99).unwrap(),
        };
        let err = client.lookup_review(&coords).await.unwrap_err();
        assert!(
            matches!(err, CtError::Api { status: 404, .. }),
            "got {err:?}"
        );
    }

    #[tokio::test]
    async fn cyber_apps_transport_uses_the_allowlisted_appsec_route() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/appsec/apps/"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        let client = CtClient::new(
            server.uri(),
            Arc::new(MockTokenStore::new(Some(stored("access", "r")))),
        )
        .unwrap();
        let err = client.cyber_list_apps().await.unwrap_err();
        assert!(
            matches!(err, CtError::Api { status: 404, .. }),
            "got {err:?}"
        );
    }

    #[tokio::test]
    async fn cyber_bind_run_uses_the_bind_route_and_conversation_body() {
        let run_id = Uuid::parse_str("11111111-1111-4111-8111-111111111111").unwrap();
        let conversation_id = Uuid::parse_str("22222222-2222-4222-8222-222222222222").unwrap();
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path(format!("/api/v1/appsec/runs/{run_id}/bind")))
            .and(body_json(
                serde_json::json!({"conversation_id": conversation_id}),
            ))
            .respond_with(ResponseTemplate::new(409))
            .mount(&server)
            .await;
        let client = CtClient::new(
            server.uri(),
            Arc::new(MockTokenStore::new(Some(stored("access", "r")))),
        )
        .unwrap();

        let err = client
            .cyber_bind_run(run_id, conversation_id)
            .await
            .unwrap_err();

        assert!(
            matches!(err, CtError::Api { status: 409, .. }),
            "got {err:?}"
        );
    }

    #[tokio::test]
    async fn cyber_launch_local_run_forwards_scope_and_propagates_backend_rejection() {
        let app_id = Uuid::from_u128(1);
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path(format!("/api/v1/appsec/apps/{app_id}/runs")))
            .and(body_json(serde_json::json!({
                "execution_host": "local",
                "run_scope": {
                    "include": ["/health"],
                    "exclude": ["/admin"]
                }
            })))
            .respond_with(ResponseTemplate::new(422).set_body_json(serde_json::json!({
                "error": {
                    "code": "validation_error",
                    "message": "invalid run scope"
                }
            })))
            .expect(1)
            .mount(&server)
            .await;
        let client = CtClient::new(
            server.uri(),
            Arc::new(MockTokenStore::new(Some(stored("access", "r")))),
        )
        .unwrap();
        let run_scope = cloudthinker_api::types::ScopeSpec {
            browser_resource_origins: Vec::new(),
            include: vec!["/health".parse().unwrap()],
            exclude: vec!["/admin".parse().unwrap()],
        };

        let err = client
            .cyber_launch_local_run(app_id, None, None, Some(run_scope))
            .await
            .unwrap_err();

        assert!(
            matches!(err, CtError::Api { status: 422, .. }),
            "got {err:?}"
        );
    }

    #[tokio::test]
    async fn cyber_launch_local_run_omits_default_scope() {
        let app_id = Uuid::from_u128(1);
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path(format!("/api/v1/appsec/apps/{app_id}/runs")))
            .and(body_json(serde_json::json!({"execution_host": "local"})))
            .respond_with(ResponseTemplate::new(422))
            .expect(1)
            .mount(&server)
            .await;
        let client = CtClient::new(
            server.uri(),
            Arc::new(MockTokenStore::new(Some(stored("access", "r")))),
        )
        .unwrap();

        let err = client
            .cyber_launch_local_run(app_id, None, None, None)
            .await
            .unwrap_err();

        assert!(
            matches!(err, CtError::Api { status: 422, .. }),
            "got {err:?}"
        );
    }

    #[tokio::test]
    async fn cyber_complete_app_setup_patches_the_allowlisted_app_route() {
        let app_id = Uuid::parse_str("11111111-1111-4111-8111-111111111111").unwrap();
        let server = MockServer::start().await;
        Mock::given(method("PATCH"))
            .and(path(format!("/api/v1/appsec/apps/{app_id}")))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        let client = CtClient::new(
            server.uri(),
            Arc::new(MockTokenStore::new(Some(stored("access", "r")))),
        )
        .unwrap();
        let err = client.cyber_complete_app_setup(app_id).await.unwrap_err();
        assert!(
            matches!(err, CtError::Api { status: 404, .. }),
            "got {err:?}"
        );
    }

    #[test]
    fn observation_batches_cover_each_row_once_and_stay_within_the_limit() {
        let observations: Vec<Observation> = (0..501)
            .map(|index| Observation {
                row_id: format!("row-{index:04}"),
                status: CoverageStatus::Covered,
                reason: String::new(),
                evidence_ref: String::new(),
                worker: "test".into(),
            })
            .collect();
        let batches: Vec<&[Observation]> = observation_batches(&observations).collect();

        assert_eq!(
            batches.iter().map(|batch| batch.len()).collect::<Vec<_>>(),
            [500, 1]
        );
        assert!(batches.iter().all(|batch| !batch.is_empty()));
        assert_eq!(
            batches
                .iter()
                .flat_map(|batch| batch.iter().map(|observation| observation.row_id.as_str()))
                .collect::<Vec<_>>(),
            observations
                .iter()
                .map(|observation| observation.row_id.as_str())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn observation_ingest_rejects_duplicate_row_ids() {
        let observations = vec![
            Observation {
                row_id: "row-1".into(),
                status: CoverageStatus::Covered,
                reason: String::new(),
                evidence_ref: String::new(),
                worker: "test".into(),
            },
            Observation {
                row_id: "row-1".into(),
                status: CoverageStatus::Blocked,
                reason: "failed".into(),
                evidence_ref: String::new(),
                worker: "test".into(),
            },
        ];
        assert!(matches!(
            validate_observation_ids(&observations),
            Err(CtError::Usage(_))
        ));
        let empty_id = [Observation {
            row_id: String::new(),
            status: CoverageStatus::Covered,
            reason: String::new(),
            evidence_ref: String::new(),
            worker: "test".into(),
        }];
        assert!(matches!(
            validate_observation_ids(&empty_id),
            Err(CtError::Usage(_))
        ));
    }

    #[tokio::test]
    async fn cyber_observation_ingest_batches_and_returns_the_complete_last_snapshot() {
        let run_id = Uuid::from_u128(1);
        let plan_id = "plan-1";
        let server = MockServer::start().await;
        let first_rows: Vec<String> = (0..500).map(|index| format!("row-{index:04}")).collect();
        let last_rows = vec!["row-0500".to_string()];
        let request_body = |row_ids: &[String]| {
            serde_json::json!({
                "observations": row_ids.iter().map(|row_id| serde_json::json!({
                    "row_id": row_id,
                    "status": "covered",
                    "reason": "",
                    "evidence_ref": "",
                    "worker": "test",
                })).collect::<Vec<_>>()
            })
        };
        let report_body = |total: usize| {
            serde_json::json!({
                "coverage": {
                    "blockers": [],
                    "plan_id": plan_id,
                    "terminal": total,
                    "total": 501,
                    "unresolved_candidates": [],
                    "untested": [],
                    "valid": total == 501,
                },
                "rows": (0..501).map(|index| serde_json::json!({
                    "asset_type": "endpoint",
                    "evidence_ref": "",
                    "executable": true,
                    "locator": format!("GET /{index}"),
                    "method": "GET",
                    "reason": "",
                    "row_id": format!("row-{index:04}"),
                    "status": "covered",
                    "url": format!("https://example.test/{index}"),
                    "worker": "test",
                })).collect::<Vec<_>>()
            })
        };
        let endpoint = format!("/api/v1/appsec/runs/{run_id}/plan/{plan_id}/observations");
        Mock::given(method("POST"))
            .and(path(endpoint.clone()))
            .and(body_json(request_body(&first_rows)))
            .respond_with(ResponseTemplate::new(200).set_body_json(report_body(500)))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path(endpoint))
            .and(body_json(request_body(&last_rows)))
            .respond_with(ResponseTemplate::new(200).set_body_json(report_body(501)))
            .expect(1)
            .mount(&server)
            .await;

        let client = CtClient::new(
            server.uri(),
            Arc::new(MockTokenStore::new(Some(stored("access", "r")))),
        )
        .unwrap();
        let observations: Vec<Observation> = (0..501)
            .map(|index| Observation {
                row_id: format!("row-{index:04}"),
                status: CoverageStatus::Covered,
                reason: String::new(),
                evidence_ref: String::new(),
                worker: "test".into(),
            })
            .collect();
        let report = client
            .cyber_ingest_observations(run_id, plan_id, observations)
            .await
            .unwrap();

        assert_eq!(report.coverage.total, 501);
        assert_eq!(report.coverage.terminal, 501);
        assert!(report.coverage.valid);
        assert_eq!(report.rows.len(), 501);
    }

    #[tokio::test]
    async fn over_long_observation_in_a_later_batch_uploads_nothing() {
        let run_id = Uuid::from_u128(1);
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path(format!(
                "/api/v1/appsec/runs/{run_id}/plan/plan-1/observations"
            )))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&server)
            .await;
        let client = CtClient::new(
            server.uri(),
            Arc::new(MockTokenStore::new(Some(stored("access", "r")))),
        )
        .unwrap();
        let observations: Vec<Observation> = (0..600)
            .map(|index| Observation {
                row_id: format!("row-{index:04}"),
                status: CoverageStatus::Covered,
                reason: if index == 599 {
                    "x".repeat(1025)
                } else {
                    String::new()
                },
                evidence_ref: String::new(),
                worker: "test".into(),
            })
            .collect();

        let error = client
            .cyber_ingest_observations(run_id, "plan-1", observations)
            .await
            .unwrap_err();

        assert!(
            matches!(&error, CtError::Usage(message) if message.contains("row-0599") && message.contains("reason")),
            "got {error:?}"
        );
    }

    #[tokio::test]
    async fn empty_observation_ingest_reads_coverage_without_posting_an_empty_batch() {
        let run_id = Uuid::from_u128(1);
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(format!("/api/v1/appsec/runs/{run_id}/coverage")))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "coverage": {
                    "blockers": [],
                    "plan_id": "plan-1",
                    "terminal": 0,
                    "total": 501,
                    "unresolved_candidates": [],
                    "untested": (0..501).map(|index| format!("row-{index:04}")).collect::<Vec<_>>(),
                    "valid": false,
                },
                "rows": [],
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path(format!(
                "/api/v1/appsec/runs/{run_id}/plan/plan-1/observations"
            )))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&server)
            .await;
        let client = CtClient::new(
            server.uri(),
            Arc::new(MockTokenStore::new(Some(stored("access", "r")))),
        )
        .unwrap();

        let report = client
            .cyber_ingest_observations(run_id, "plan-1", Vec::new())
            .await
            .unwrap();

        assert_eq!(report.coverage.total, 501);
        assert_eq!(report.coverage.untested.len(), 501);
        assert!(!report.coverage.valid);
    }
}
