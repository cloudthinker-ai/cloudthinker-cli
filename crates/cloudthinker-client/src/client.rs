//! `CtClient` — the typed wire surface every command calls.
//!
//! Wraps the generated `cloudthinker_api::Client`, injects the bearer token,
//! and owns the 401-retry / proactive-refresh dance so commands never touch
//! reqwest, serde, or refresh logic directly.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Serialize;
use uuid::Uuid;

use crate::auth::refresh::RefreshCoordinator;
use crate::auth::store::{
    AutoStore, EnvTokenStore, StoredToken, TOKEN_ENV_VAR, TokenStore, WorkspaceSelector,
    acquire_credential_lock,
};
use crate::error::{CtError, CtResult, to_ct_error};
use crate::review_url::{MrCoordinates, MrProvider};

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
    Token(StoredToken),
}

#[derive(Debug, Clone, Serialize)]
pub struct CliIdentity {
    pub host: String,
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
}

impl RunStatus {
    /// True once the run has reached a state that will not change on its own.
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Succeeded | Self::Failed | Self::RequiredApproval
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

/// Terminal + non-terminal review states, serialized with the API's wire
/// values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewStatus {
    InReview,
    ReviewComplete,
    Filtered,
    Failed,
}

impl ReviewStatus {
    /// True once the review has reached a state that will not change on its
    /// own (`review_complete`, `filtered`, or `failed`).
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::ReviewComplete | Self::Filtered | Self::Failed)
    }
}

impl From<cloudthinker_api::types::ReviewStatus> for ReviewStatus {
    fn from(status: cloudthinker_api::types::ReviewStatus) -> Self {
        use cloudthinker_api::types::ReviewStatus as A;
        match status {
            A::InReview => Self::InReview,
            A::ReviewComplete => Self::ReviewComplete,
            A::Filtered => Self::Filtered,
            A::Failed => Self::Failed,
        }
    }
}

/// The review's overall verdict — advisory display, never drives the exit
/// code (only `ReviewStatus::Failed` does, CA-RV-3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewVerdict {
    InReview,
    Approved,
    ReviewSuggested,
    ChangesRequested,
    Failed,
    Filtered,
}

impl From<cloudthinker_api::types::CodeReviewOverviewVerdict> for ReviewVerdict {
    fn from(verdict: cloudthinker_api::types::CodeReviewOverviewVerdict) -> Self {
        use cloudthinker_api::types::CodeReviewOverviewVerdict as A;
        match verdict {
            A::InReview => Self::InReview,
            A::Approved => Self::Approved,
            A::ReviewSuggested => Self::ReviewSuggested,
            A::ChangesRequested => Self::ChangesRequested,
            A::Failed => Self::Failed,
            A::Filtered => Self::Filtered,
        }
    }
}

/// Unresolved finding counts per severity (mirrors the review-summary panel).
#[derive(Debug, Clone, Copy, Serialize)]
pub struct ReviewSeverityCounts {
    pub critical: i64,
    pub high: i64,
    pub medium: i64,
    pub low: i64,
}

impl From<cloudthinker_api::types::CodeReviewSeverityCounts> for ReviewSeverityCounts {
    fn from(api: cloudthinker_api::types::CodeReviewSeverityCounts) -> Self {
        Self {
            critical: api.critical,
            high: api.high,
            medium: api.medium,
            low: api.low,
        }
    }
}

/// A single finding on the reviewed merge request.
#[derive(Debug, Clone, Serialize)]
pub struct ReviewFinding {
    pub severity: String,
    pub file_path: Option<String>,
    pub line_number: Option<i64>,
    pub issue_title: String,
    pub category: Option<String>,
    pub resolved: bool,
}

impl From<cloudthinker_api::types::CodeReviewDetailFinding> for ReviewFinding {
    fn from(api: cloudthinker_api::types::CodeReviewDetailFinding) -> Self {
        Self {
            severity: api.severity,
            file_path: api.file_path,
            line_number: api.line_number,
            issue_title: api.issue_title,
            category: api.category,
            resolved: api.resolved,
        }
    }
}

/// Rank a severity string worst-first for sorting (`findings`, CA-RV-2); an
/// unrecognized value sorts last rather than panicking.
fn severity_rank(severity: &str) -> u8 {
    match severity {
        "critical" => 0,
        "high" => 1,
        "medium" => 2,
        "low" => 3,
        _ => 4,
    }
}

/// A resolved review detail (`GET /code-review/merge-requests/lookup`).
#[derive(Debug, Clone)]
pub struct ReviewView {
    pub mr_iid: i64,
    pub status: ReviewStatus,
    pub verdict: ReviewVerdict,
    pub findings_count: i64,
    pub title: String,
    pub url: Option<String>,
    pub repository_path: Option<String>,
    pub provider: String,
    pub severity_counts: ReviewSeverityCounts,
    /// Worst-severity-first (CA-RV-2).
    pub findings: Vec<ReviewFinding>,
}

impl ReviewView {
    fn from_api(api: cloudthinker_api::types::CodeReviewMergeRequestDetail) -> Self {
        let mut findings: Vec<ReviewFinding> =
            api.findings.into_iter().map(ReviewFinding::from).collect();
        findings.sort_by_key(|f| severity_rank(&f.severity));
        Self {
            mr_iid: api.mr_iid,
            status: api.review_status.into(),
            verdict: api.verdict.into(),
            findings_count: api.findings_count,
            title: api.title,
            url: api.url,
            repository_path: api.repository_path,
            provider: api.provider.to_string(),
            severity_counts: api.severity_counts.into(),
            findings,
        }
    }
}

fn to_api_provider(provider: MrProvider) -> cloudthinker_api::types::CodeReviewProvider {
    match provider {
        MrProvider::Gitlab => cloudthinker_api::types::CodeReviewProvider::Gitlab,
        MrProvider::Github => cloudthinker_api::types::CodeReviewProvider::Github,
    }
}

/// The authenticated client. Cheap to clone-share via `Arc`.
pub struct CtClient {
    base_url: String,
    store: Arc<dyn TokenStore>,
    refresh: RefreshCoordinator,
    // Reqwest pools connections; rebuild only when the bearer token rotates so
    // polling reuses one TLS connection across the run.
    http_cache: Mutex<Option<(String, reqwest::Client)>>,
    // Anonymous client for endpoints that carry no bearer (exchange, refresh,
    // best-effort logout).
    anon_http: reqwest::Client,
}

impl CtClient {
    pub fn new(base_url: impl Into<String>, store: Arc<dyn TokenStore>) -> CtResult<Self> {
        let base_url = base_url.into();
        let timeout = Duration::from_secs(REQUEST_TIMEOUT_SECS);
        let anon_http = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .map_err(|e| CtError::Transport(format!("http client build: {e}")))?;
        let refresh = RefreshCoordinator::new(base_url.clone(), store.clone(), anon_http.clone());
        Ok(Self {
            base_url,
            store,
            refresh,
            http_cache: Mutex::new(None),
            anon_http,
        })
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    // -- authenticated calls -------------------------------------------------

    /// Submit a headless run for `prompt`. Returns 202 identifiers.
    pub async fn submit_run(&self, prompt: &str) -> CtResult<SubmittedRun> {
        let prompt_field = prompt
            .parse::<cloudthinker_api::types::Prompt>()
            .map_err(|_| CtError::Usage("prompt must be 1–50000 characters".into()))?;
        let body = cloudthinker_api::types::SubmitHeadlessRunRequest {
            // V1 submits are at-least-once: the CLI does not retry a submit, so
            // there is no key to replay against.
            idempotency_key: None,
            prompt: prompt_field,
        };
        let submitted = self
            .authed(async |c: cloudthinker_api::Client| {
                c.cli_submit_headless_run(None, &body).await
            })
            .await?;
        Ok(SubmittedRun::from_api(submitted))
    }

    /// Poll one run's current status.
    pub async fn get_run(&self, run_id: Uuid) -> CtResult<RunView> {
        let view = self
            .authed(async |c: cloudthinker_api::Client| c.cli_get_headless_run(&run_id, None).await)
            .await?;
        Ok(RunView::from_api(view))
    }

    /// Resolve MR coordinates (parsed client-side from a pasted URL) to their
    /// tracked review detail. Unknown coordinates surface as
    /// `CtError::Api { status: 404, .. }`.
    pub async fn lookup_review(&self, coords: &MrCoordinates) -> CtResult<ReviewView> {
        let provider = to_api_provider(coords.provider);
        let mr_iid = coords.mr_iid;
        let project_path = coords.project_path.as_str();
        let view = self
            .authed(async |c: cloudthinker_api::Client| {
                c.code_review_lookup_code_review_merge_request(mr_iid, project_path, provider, None)
                    .await
            })
            .await?;
        Ok(ReviewView::from_api(view))
    }

    /// Resolve the live account and workspace carried by this credential.
    pub async fn whoami(&self) -> CtResult<CliIdentity> {
        let identity = self
            .authed(async |c: cloudthinker_api::Client| c.login_cli_whoami().await)
            .await?;
        Ok(self.identity_from_api(identity))
    }

    // -- unauthenticated calls -----------------------------------------------

    /// Exchange a one-time code + verifier for a token. Any rejection collapses
    /// to a single generic auth error (the backend does not distinguish burned
    /// vs expired codes, CA-CLI-4).
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
                CtError::Transport(m) => CtError::Transport(m),
                _ => {
                    CtError::Auth("could not complete login; run `cloudthinker login` again".into())
                }
            }),
        }
    }

    async fn whoami_with_access(&self, access: &str) -> CtResult<CliIdentity> {
        let api = self.api_client(access)?;
        let response = match api.login_cli_whoami().await {
            Ok(response) => response,
            Err(error) => return Err(to_ct_error(error).await),
        };
        Ok(self.identity_from_api(response.into_inner()))
    }

    fn identity_from_api(&self, api: cloudthinker_api::types::CliWhoAmIResponse) -> CliIdentity {
        CliIdentity {
            host: origin_of(&self.base_url).unwrap_or_else(|_| self.base_url.clone()),
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
                    Code::ExpiredToken => Err(CtError::Timeout(
                        "device code expired; run `cloudthinker login --device-auth` again".into(),
                    )),
                }
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
    pub async fn logout(&self) -> CtResult<()> {
        let lock = acquire_credential_lock(self.store.clone()).await?;
        let revoke_failed = match self.store.load_locked(&lock)? {
            Some(current) => !self.revoke(current.refresh_token).await,
            None => false,
        };
        self.store.clear_locked(&lock)?;
        if revoke_failed {
            return Err(CtError::Logout(
                "server session could not be revoked; local credential was cleared".into(),
            ));
        }
        Ok(())
    }

    /// Revoke every stored workspace session for this host, then clear them.
    pub async fn logout_all(&self) -> CtResult<()> {
        let lock = acquire_credential_lock(self.store.clone()).await?;
        let tokens = self.store.load_all_locked(&lock)?;
        let mut attempted = 0_usize;
        let mut failed = 0_usize;
        for token in tokens {
            if token.refresh_token.is_some() {
                attempted += 1;
                if !self.revoke(token.refresh_token).await {
                    failed += 1;
                }
            }
        }
        self.store.clear_all_locked(&lock)?;
        if failed > 0 {
            return Err(CtError::Logout(format!(
                "server revocation failed for {failed} of {attempted} sessions; local credentials were cleared"
            )));
        }
        Ok(())
    }

    async fn revoke(&self, refresh_token: Option<String>) -> bool {
        let Some(refresh_token) = refresh_token else {
            return true;
        };
        let body = cloudthinker_api::types::RevokeTokenRequest {
            refresh_token: Some(refresh_token),
        };
        cloudthinker_api::Client::new_with_client(&self.base_url, self.anon_http.clone())
            .login_logout(&body)
            .await
            .is_ok()
    }

    // -- internals -----------------------------------------------------------

    /// Run `call` with a fresh bearer, retrying exactly once through a refresh
    /// on a 401. Proactive refresh happens in `ensure_fresh` before the first
    /// attempt.
    async fn authed<T, F>(&self, call: F) -> CtResult<T>
    where
        F: AsyncFn(
            cloudthinker_api::Client,
        )
            -> Result<cloudthinker_api::ResponseValue<T>, cloudthinker_api::Error<()>>,
    {
        let access = self.ensure_fresh().await?;
        let client = self.api_client(&access)?;
        match call(client).await {
            Ok(rv) => Ok(rv.into_inner()),
            Err(err) => {
                let is_unauthorized = err.status().map(|s| s.as_u16()) == Some(401);
                if is_unauthorized && self.store.refresh_enabled() {
                    let rotated = self.refresh.refresh(&access).await?;
                    let client = self.api_client(&rotated.access_token)?;
                    match call(client).await {
                        Ok(rv) => Ok(rv.into_inner()),
                        Err(err2) => Err(reclassify(to_ct_error(err2).await)),
                    }
                } else {
                    Err(reclassify(to_ct_error(err).await))
                }
            }
        }
    }

    /// Load the current access token, refreshing proactively when it is within
    /// the skew window of expiry.
    async fn ensure_fresh(&self) -> CtResult<String> {
        let current = self
            .store
            .load()?
            .ok_or_else(|| CtError::Auth("not logged in; run `cloudthinker login`".into()))?;
        if self.store.refresh_enabled()
            && current.expires_within(RefreshCoordinator::proactive_skew())
        {
            let rotated = self.refresh.refresh(&current.access_token).await?;
            Ok(rotated.access_token)
        } else {
            Ok(current.access_token)
        }
    }

    /// Build (or reuse) an API client whose bearer header is `access`.
    fn api_client(&self, access: &str) -> CtResult<cloudthinker_api::Client> {
        let mut guard = self
            .http_cache
            .lock()
            .map_err(|_| CtError::Transport("http cache poisoned".into()))?;
        let cached = match guard.as_ref() {
            Some((tok, client)) if tok == access => Some(client.clone()),
            _ => None,
        };
        let http = match cached {
            Some(client) => client,
            None => {
                let client = build_authed_http(access)?;
                *guard = Some((access.to_string(), client.clone()));
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

fn build_authed_http(access: &str) -> CtResult<reqwest::Client> {
    let mut headers = reqwest::header::HeaderMap::new();
    let value = reqwest::header::HeaderValue::from_str(&format!("Bearer {access}"))
        .map_err(|e| CtError::Auth(format!("invalid token header: {e}")))?;
    headers.insert(reqwest::header::AUTHORIZATION, value);
    reqwest::Client::builder()
        .timeout(Duration::from_secs(REQUEST_TIMEOUT_SECS))
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

/// A 401/403 surfacing here (rather than being cured by refresh) means the user
/// must log in again.
fn reclassify(err: CtError) -> CtError {
    match err {
        CtError::Api {
            status: 401 | 403, ..
        } => CtError::Auth("run `cloudthinker login`".into()),
        other => other,
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

fn is_loopback_host(url: &url::Url) -> bool {
    match url.host() {
        Some(url::Host::Domain(d)) => d.eq_ignore_ascii_case("localhost"),
        Some(url::Host::Ipv4(addr)) => addr.is_loopback(),
        Some(url::Host::Ipv6(addr)) => addr.is_loopback(),
        None => false,
    }
}

/// Resolve the read/refresh store: env override if `CLOUDTHINKER_TOKEN` is set,
/// otherwise the keyring-preferred `Auto` store.
pub fn resolve_store(base_url: &str, workspace: Option<&str>) -> CtResult<Arc<dyn TokenStore>> {
    if let Some(env) = EnvTokenStore::from_env() {
        if workspace.is_some() {
            return Err(CtError::Usage(format!(
                "--workspace cannot be used with {TOKEN_ENV_VAR}"
            )));
        }
        return Ok(Arc::new(env));
    }
    auto_store(base_url, workspace)
}

/// The persistent store used by `login`/`logout` (never the env override — those
/// commands must write real credentials).
pub fn persistent_store(base_url: &str, workspace: Option<&str>) -> CtResult<Arc<dyn TokenStore>> {
    auto_store(base_url, workspace)
}

fn auto_store(base_url: &str, workspace: Option<&str>) -> CtResult<Arc<dyn TokenStore>> {
    let selector = workspace.map_or(WorkspaceSelector::Active, |value| {
        WorkspaceSelector::IdOrName(value.to_string())
    });
    Ok(Arc::new(AutoStore::default_for(
        origin_of(base_url)?,
        selector,
    )?))
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::*;
    use crate::auth::store::SaveLocation;
    use crate::test_support::{MockTokenStore, stored, token_json};
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

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

        fn save(&self, _token: &StoredToken) -> CtResult<SaveLocation> {
            Ok(SaveLocation::File)
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

    // CA-CLI-4: any exchange failure collapses to a generic auth error.
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
        let err = client.submit_run("here is my aws key").await.unwrap_err();
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
        let err = client.submit_run("here is my prompt").await.unwrap_err();
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
        let err = client.submit_run("").await.unwrap_err();
        assert!(matches!(err, CtError::Usage(_)), "got {err:?}");
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
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/code-review/merge-requests/lookup"))
            .and(query_param("mr_iid", "42"))
            .and(query_param("project_path", "group/my-repo"))
            .and(query_param("provider", "gitlab"))
            .respond_with(ResponseTemplate::new(200).set_body_json(review_detail_json()))
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
            mr_iid: 42,
        };
        let view = client.lookup_review(&coords).await.unwrap();

        assert_eq!(view.mr_iid, 42);
        assert_eq!(view.status, ReviewStatus::ReviewComplete);
        assert!(view.status.is_terminal());
        assert_eq!(view.verdict, ReviewVerdict::ChangesRequested);
        assert_eq!(view.findings_count, 2);
        assert_eq!(view.title, "Fix the bug");
        assert_eq!(view.provider, "gitlab");
        assert_eq!(view.repository_path.as_deref(), Some("group/my-repo"));
        assert_eq!(view.severity_counts.critical, 1);
        assert_eq!(view.severity_counts.high, 2);
        assert_eq!(view.findings.len(), 2);
        assert_eq!(view.findings[0].severity, "critical", "worst-first");
        assert_eq!(view.findings[1].severity, "high");
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
            mr_iid: 99,
        };
        let err = client.lookup_review(&coords).await.unwrap_err();
        assert!(
            matches!(err, CtError::Api { status: 404, .. }),
            "got {err:?}"
        );
    }
}
