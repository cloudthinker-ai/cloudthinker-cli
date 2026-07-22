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
use crate::auth::store::{AutoStore, EnvTokenStore, StoredToken, TokenStore};
use crate::error::{CtError, CtResult, to_ct_error};
use crate::review_url::{MrCoordinates, MrProvider};

const REQUEST_TIMEOUT_SECS: u64 = 30;

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
    fn from_api(status: &cloudthinker_api::types::AgentRunStatus) -> Self {
        use cloudthinker_api::types::AgentRunStatus as A;
        match status {
            A::Pending => Self::Pending,
            A::Running => Self::Running,
            A::Succeeded => Self::Succeeded,
            A::Failed => Self::Failed,
            A::RequiredApproval => Self::RequiredApproval,
        }
    }

    /// True once the run has reached a state that will not change on its own.
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Succeeded | Self::Failed | Self::RequiredApproval
        )
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
            status: RunStatus::from_api(&api.status),
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
            status: RunStatus::from_api(&api.status),
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
    fn from_api(status: &cloudthinker_api::types::ReviewStatus) -> Self {
        use cloudthinker_api::types::ReviewStatus as A;
        match status {
            A::InReview => Self::InReview,
            A::ReviewComplete => Self::ReviewComplete,
            A::Filtered => Self::Filtered,
            A::Failed => Self::Failed,
        }
    }

    /// True once the review has reached a state that will not change on its
    /// own (`review_complete`, `filtered`, or `failed`).
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::ReviewComplete | Self::Filtered | Self::Failed)
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

impl ReviewVerdict {
    fn from_api(verdict: &cloudthinker_api::types::CodeReviewOverviewVerdict) -> Self {
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

impl ReviewSeverityCounts {
    fn from_api(api: &cloudthinker_api::types::CodeReviewSeverityCounts) -> Self {
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

impl ReviewFinding {
    fn from_api(api: cloudthinker_api::types::CodeReviewDetailFinding) -> Self {
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
        let mut findings: Vec<ReviewFinding> = api
            .findings
            .into_iter()
            .map(ReviewFinding::from_api)
            .collect();
        findings.sort_by_key(|f| severity_rank(&f.severity));
        Self {
            mr_iid: api.mr_iid,
            status: ReviewStatus::from_api(&api.review_status),
            verdict: ReviewVerdict::from_api(&api.verdict),
            findings_count: api.findings_count,
            title: api.title,
            url: api.url,
            repository_path: api.repository_path,
            provider: api.provider.to_string(),
            severity_counts: ReviewSeverityCounts::from_api(&api.severity_counts),
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
            Ok(rv) => Ok(StoredToken::from_token(&rv.into_inner())),
            Err(e) => Err(match to_ct_error(e).await {
                CtError::Transport(m) => CtError::Transport(m),
                _ => {
                    CtError::Auth("could not complete login; run `cloudthinker login` again".into())
                }
            }),
        }
    }

    /// Best-effort logout: revoke the refresh token server-side, then clear the
    /// local store. Always succeeds (the server call is fire-and-forget).
    pub async fn logout(&self) -> CtResult<()> {
        if let Ok(Some(current)) = self.store.load()
            && let Some(refresh_token) = current.refresh_token
        {
            let body = cloudthinker_api::types::RevokeTokenRequest {
                refresh_token: Some(refresh_token),
            };
            let api =
                cloudthinker_api::Client::new_with_client(&self.base_url, self.anon_http.clone());
            let _ = api.login_logout(&body).await;
        }
        self.store.clear()
    }

    // -- internals -----------------------------------------------------------

    /// Run `call` with a fresh bearer, retrying exactly once through a refresh
    /// on a 401. Proactive refresh happens in `ensure_fresh` before the first
    /// attempt.
    async fn authed<T, F>(&self, call: F) -> CtResult<T>
    where
        F: AsyncFn(
            cloudthinker_api::Client,
        ) -> Result<
            cloudthinker_api::ResponseValue<T>,
            cloudthinker_api::Error<cloudthinker_api::types::HttpValidationError>,
        >,
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

/// Extract the host from a base URL — the token store account key.
pub fn host_of(base_url: &str) -> CtResult<String> {
    let url =
        url::Url::parse(base_url).map_err(|e| CtError::Usage(format!("invalid --url: {e}")))?;
    url.host_str()
        .map(str::to_string)
        .ok_or_else(|| CtError::Usage("--url has no host".into()))
}

/// Resolve the read/refresh store: env override if `CLOUDTHINKER_TOKEN` is set,
/// otherwise the keyring-preferred `Auto` store.
pub fn resolve_store(base_url: &str) -> CtResult<Arc<dyn TokenStore>> {
    if let Some(env) = EnvTokenStore::from_env() {
        return Ok(Arc::new(env));
    }
    Ok(Arc::new(AutoStore::default_for(host_of(base_url)?)?))
}

/// The persistent store used by `login`/`logout` (never the env override — those
/// commands must write real credentials).
pub fn persistent_store(base_url: &str) -> CtResult<Arc<dyn TokenStore>> {
    Ok(Arc::new(AutoStore::default_for(host_of(base_url)?)?))
}

#[cfg(test)]
mod tests {
    use super::*;
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

    fn valid_verifier() -> String {
        "a".repeat(43)
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
            .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
                "detail": "invalid"
            })))
            .mount(&server)
            .await;

        let client = CtClient::new(server.uri(), Arc::new(MockTokenStore::new(None))).unwrap();
        let err = client
            .exchange_code("one-time-code", &valid_verifier())
            .await
            .unwrap_err();
        assert!(matches!(err, CtError::Auth(_)), "got {err:?}");
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
            .respond_with(ResponseTemplate::new(401))
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
    async fn refreshes_once_on_401_then_retries() {
        let server = MockServer::start().await;
        // First GET 401 (once), then 200 succeeded.
        Mock::given(method("GET"))
            .and(path(
                "/api/v1/cli/runs/33333333-3333-4333-8333-333333333333",
            ))
            .respond_with(ResponseTemplate::new(401))
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

    // CA-CLI-16 (404 half): an unknown run id surfaces a 404 Api error.
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
        assert!(
            matches!(err, CtError::Api { status: 404, .. }),
            "got {err:?}"
        );
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

    // CA-RV-SP4: unknown coordinates surface as a 404 Api error.
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
