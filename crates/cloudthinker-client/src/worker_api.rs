use std::num::NonZeroU64;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{PoisonError, RwLock};
use std::time::Duration;

use chrono::{DateTime, Utc};
use reqwest::header::{DATE, HeaderMap, HeaderValue, RETRY_AFTER};
use serde::Deserialize;
use uuid::Uuid;

use crate::{CtError, CtResult, origin_of, worker_types as api};

const MIN_CLOCK_SKEW_MS: i64 = 2_000;
const MAX_RETRY_AFTER: Duration = Duration::from_secs(30);
const CLI_VERSION_HEADER: &str = "x-cloudthinker-cli-version";
const ROTATE_HEADER: &str = "x-cloudthinker-credential-rotate";
const ROTATE_PATH: &str = "/api/v1/executor-workers/credential/rotate";
const STATUS_PATH: &str = "/api/v1/executor-workers/credential";
const MAX_REPLY_BYTES: usize = 64 * 1024;
const UNAUTHORIZED_CODE: &str = "EXECUTOR_WORKER_UNAUTHORIZED";
const UNAUTHORIZED_FALLBACK: &str = "This Worker's credential expired or was revoked. Enroll it again with the command from the Workers page.";
const OUTDATED_CODE: &str = "EXECUTOR_WORKER_OUTDATED";
const OUTPOSTS_DISABLED_CODE: &str = "EXECUTOR_OUTPOSTS_DISABLED";
const OUTPOSTS_DISABLED_FALLBACK: &str = "Workers are turned off.";
const QUOTA_EXCEEDED_CODE: &str = "EXECUTOR_QUOTA_EXCEEDED";
const OUTDATED_FALLBACK: &str =
    "Your CLI is out of date. Run `cloudthinker update`, then run a new command.";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeFailure {
    Dns,
    Connect,
    Timeout,
    Tls,
    Other,
}

#[derive(Debug, Clone)]
pub struct OriginProbe {
    pub status: u16,
    pub skew: Option<chrono::Duration>,
    pub elapsed: Duration,
}

#[derive(Default)]
struct Observed {
    skew_ms: AtomicI64,
    rotate_due: AtomicBool,
}

#[derive(Clone)]
struct Wire {
    api: cloudthinker_api::Client,
    http: reqwest::Client,
    origin: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct WorkerCredentialStatus {
    pub target_id: Uuid,
    pub generation: i64,
    pub expires_at: DateTime<Utc>,
    pub rotate_after: DateTime<Utc>,
}

#[derive(Clone)]
pub struct WorkerClient {
    base_url: String,
    cli_version: String,
    wire: Arc<RwLock<Wire>>,
    observed: Arc<Observed>,
}

impl WorkerClient {
    pub fn new(base_url: &str, credential: &str, cli_version: &str) -> CtResult<Self> {
        let headers = authorized_headers(cli_version, credential)?;
        Ok(Self {
            base_url: base_url.to_owned(),
            cli_version: cli_version.to_owned(),
            wire: Arc::new(RwLock::new(client(base_url, headers)?)),
            observed: Arc::new(Observed::default()),
        })
    }

    fn wire(&self) -> Wire {
        self.wire
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn api(&self) -> cloudthinker_api::Client {
        self.wire().api
    }

    pub fn rotation_due(&self) -> bool {
        self.observed.rotate_due.load(Ordering::Relaxed)
    }

    pub fn adopt(&self, token: &str) -> CtResult<()> {
        let headers = authorized_headers(&self.cli_version, token)?;
        let wire = client(&self.base_url, headers)?;
        *self.wire.write().unwrap_or_else(PoisonError::into_inner) = wire;
        self.observed.rotate_due.store(false, Ordering::Relaxed);
        Ok(())
    }

    pub async fn rotate(&self) -> CtResult<api::WorkerBootstrap> {
        let wire = self.wire();
        self.send(wire.http.post(format!("{}{ROTATE_PATH}", wire.origin)))
            .await
    }

    pub async fn credential_status(&self) -> CtResult<WorkerCredentialStatus> {
        let wire = self.wire();
        self.send(wire.http.get(format!("{}{STATUS_PATH}", wire.origin)))
            .await
    }

    async fn send<T: serde::de::DeserializeOwned>(
        &self,
        request: reqwest::RequestBuilder,
    ) -> CtResult<T> {
        let reply = request
            .send()
            .await
            .map_err(|_| CtError::Transport("worker request failed".into()))?;
        observe(&self.observed, reply.headers());
        let status = reply.status();
        if status.is_success() {
            let body = read_capped(reply).await.ok_or_else(protocol_mismatch)?;
            return serde_json::from_slice(&body).map_err(|_| protocol_mismatch());
        }
        let wait = retry_after(reply.headers());
        let body = read_capped_text(reply).await;
        Err(rejected(status.as_u16(), body.as_deref(), wait).await)
    }

    pub async fn exchange(
        base_url: &str,
        reference: &str,
        cli_version: &str,
    ) -> CtResult<api::WorkerBootstrap> {
        let client = client(base_url, version_headers(cli_version)?)?.api;
        response(
            &Observed::default(),
            client
                .executor_targets_exchange_registration(&api::ExchangeWorkerRegistrationRequest {
                    reference: reference.into(),
                })
                .await,
        )
        .await
    }

    pub fn server_skew(&self) -> chrono::Duration {
        chrono::Duration::milliseconds(self.observed.skew_ms.load(Ordering::Relaxed))
    }

    fn local_time(&self, server: DateTime<Utc>) -> DateTime<Utc> {
        server - self.server_skew()
    }

    async fn call<T>(
        &self,
        result: Result<cloudthinker_api::ResponseValue<T>, cloudthinker_api::Error<()>>,
    ) -> CtResult<T> {
        response(&self.observed, result).await
    }

    pub async fn register(
        &self,
        request: &api::RegisterWorkerRequest,
    ) -> CtResult<api::WorkerPublic> {
        self.call(self.api().executor_targets_register_worker(request).await)
            .await
    }

    pub async fn conformance(&self, worker_id: Uuid) -> CtResult<api::WorkerConformancePublic> {
        self.call(
            self.api()
                .executor_targets_worker_conformance(&api::StartWorkerConformanceRequest {
                    worker_id,
                })
                .await,
        )
        .await
    }

    pub async fn worker_heartbeat(
        &self,
        worker: Uuid,
        draining: bool,
        repository: Option<&api::RepositorySummary>,
        full: bool,
    ) -> CtResult<api::WorkerPublic> {
        let state = if draining {
            api::WorkerHeartbeatRequestState::Draining
        } else {
            api::WorkerHeartbeatRequestState::Online
        };
        self.call(
            self.api()
                .executor_targets_worker_heartbeat(&api::WorkerHeartbeatRequest {
                    worker_id: worker,
                    state,
                    repository_digest: repository.and_then(|r| r.digest.parse().ok()),
                    repository: repository.filter(|_| full).cloned(),
                })
                .await,
        )
        .await
    }

    pub async fn pending(&self, worker: Uuid) -> CtResult<Vec<api::PendingAssignmentPublic>> {
        self.call(
            self.api()
                .executor_targets_pending_assignments(Some(15.0), &worker)
                .await,
        )
        .await
    }

    pub async fn claim(&self, worker: Uuid, assignment: Uuid) -> CtResult<api::AssignmentLease> {
        self.call(
            self.api()
                .executor_targets_claim_assignment(
                    &assignment,
                    &api::ClaimAssignmentRequest { worker_id: worker },
                )
                .await,
        )
        .await
        .map(|lease| self.local_lease(lease))
    }

    fn local_lease(&self, mut lease: api::AssignmentLease) -> api::AssignmentLease {
        lease.lease_expires_at = self.local_time(lease.lease_expires_at);
        lease
    }

    pub async fn heartbeat(
        &self,
        lease: &api::AssignmentLease,
        state: api::HeartbeatAssignmentRequestState,
    ) -> CtResult<api::AssignmentLease> {
        let body = api::HeartbeatAssignmentRequest {
            worker_id: lease.worker_id,
            fence_token: positive(lease.fence_token)?,
            lease_token: lease.lease_token.clone(),
            state,
        };
        self.call(
            self.api()
                .executor_targets_heartbeat_assignment(&lease.assignment_id, &body)
                .await,
        )
        .await
        .map(|lease| self.local_lease(lease))
    }

    pub async fn operations(
        &self,
        lease: &api::AssignmentLease,
        after: u64,
        batch_size: u64,
    ) -> CtResult<Vec<api::OperationEnvelope>> {
        self.call(
            self.api()
                .executor_targets_assignment_operations(
                    &lease.assignment_id,
                    Some(after),
                    Some(NonZeroU64::new(batch_size).ok_or_else(invalid_envelope)?),
                    Some(15.0),
                    &lease.worker_id,
                    positive(lease.fence_token)?,
                    &lease.lease_token,
                )
                .await,
        )
        .await
        .map(|envelopes| {
            envelopes
                .into_iter()
                .map(|mut envelope| {
                    envelope.deadline_at = self.local_time(envelope.deadline_at);
                    envelope
                })
                .collect()
        })
    }

    pub async fn artifact_grant(
        &self,
        assignment: Uuid,
        request: &api::IssueArtifactGrantRequest,
    ) -> CtResult<api::ArtifactGrantPublic> {
        self.call(
            self.api()
                .executor_targets_issue_artifact_grant(&assignment, request)
                .await,
        )
        .await
    }

    pub async fn upload_artifact(
        &self,
        assignment: Uuid,
        grant: Uuid,
        request: &api::UploadWorkerArtifactRequest,
    ) -> CtResult<api::WorkerArtifactPublic> {
        self.call(
            self.api()
                .executor_targets_upload_worker_artifact(&assignment, &grant, request)
                .await,
        )
        .await
    }

    pub async fn start(&self, worker: Uuid, envelope: &api::OperationEnvelope) -> CtResult<()> {
        let body = api::OperationStartRequest {
            worker_id: worker,
            fence_token: positive(envelope.fence_token)?,
            lease_token: envelope.lease_token.clone(),
            nonce: envelope.nonce.clone(),
            operation_sequence: positive(envelope.operation_sequence)?,
            request_digest: envelope
                .request_digest
                .parse()
                .map_err(|_| invalid_envelope())?,
        };
        self.call(
            self.api()
                .executor_targets_start_operation(
                    &envelope.assignment_id,
                    &envelope.operation_id,
                    &body,
                )
                .await,
        )
        .await
    }

    pub async fn complete(
        &self,
        worker: Uuid,
        envelope: &api::OperationEnvelope,
        result: api::WorkerOperationResult,
    ) -> CtResult<api::OperationReceiptPublic> {
        let body = api::CompleteWorkerOperationRequest {
            worker_id: worker,
            fence_token: positive(envelope.fence_token)?,
            lease_token: envelope.lease_token.clone(),
            nonce: envelope.nonce.clone(),
            operation_sequence: positive(envelope.operation_sequence)?,
            request_digest: envelope
                .request_digest
                .parse()
                .map_err(|_| invalid_envelope())?,
            session_id: envelope.session_id,
            result,
        };
        self.call(
            self.api()
                .executor_targets_complete_operation(
                    &envelope.assignment_id,
                    &envelope.operation_id,
                    &body,
                )
                .await,
        )
        .await
    }

    pub async fn receipt(
        &self,
        worker: Uuid,
        assignment: Uuid,
        operation: Uuid,
    ) -> CtResult<api::OperationReceiptPublic> {
        self.call(
            self.api()
                .executor_targets_read_receipt(&assignment, &operation, &worker)
                .await,
        )
        .await
    }

    pub async fn release(&self, lease: &api::AssignmentLease) -> CtResult<()> {
        let body = api::AssignmentLeaseRequest {
            worker_id: lease.worker_id,
            fence_token: positive(lease.fence_token)?,
            lease_token: lease.lease_token.clone(),
        };
        self.call(
            self.api()
                .executor_targets_release_assignment(&lease.assignment_id, &body)
                .await,
        )
        .await
    }
}

fn version_headers(cli_version: &str) -> CtResult<HeaderMap> {
    let mut headers = HeaderMap::new();
    headers.insert(
        CLI_VERSION_HEADER,
        reqwest::header::HeaderValue::from_str(cli_version)
            .map_err(|_| CtError::Usage("invalid CLI version".into()))?,
    );
    Ok(headers)
}

fn authorized_headers(cli_version: &str, credential: &str) -> CtResult<HeaderMap> {
    let mut headers = version_headers(cli_version)?;
    let mut bearer = HeaderValue::from_str(&format!("Bearer {credential}"))
        .map_err(|_| CtError::Auth("invalid worker credential".into()))?;
    bearer.set_sensitive(true);
    headers.insert(reqwest::header::AUTHORIZATION, bearer);
    Ok(headers)
}

fn client(base_url: &str, headers: reqwest::header::HeaderMap) -> CtResult<Wire> {
    let origin = origin_of(base_url)?;
    let url =
        url::Url::parse(&origin).map_err(|_| CtError::Usage("invalid worker origin".into()))?;
    if url.scheme() != "https"
        && !(url.scheme() == "http"
            && matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]")))
    {
        return Err(CtError::Usage(
            "worker requires HTTPS; HTTP is allowed only on loopback".into(),
        ));
    }
    let http = crate::tls::http_client_builder()
        .default_headers(headers)
        .timeout(Duration::from_secs(25))
        .connect_timeout(Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| CtError::Transport("worker HTTP client unavailable".into()))?;
    Ok(Wire {
        api: cloudthinker_api::Client::new_with_client(&origin, http.clone()),
        http,
        origin,
    })
}

async fn response<T>(
    observed: &Observed,
    result: Result<cloudthinker_api::ResponseValue<T>, cloudthinker_api::Error<()>>,
) -> CtResult<T> {
    match result {
        Ok(value) => {
            observe(observed, value.headers());
            Ok(value.into_inner())
        }
        Err(error) => Err(worker_error(observed, error).await),
    }
}

fn observe(observed: &Observed, headers: &HeaderMap) {
    if headers
        .get(ROTATE_HEADER)
        .is_some_and(|value| value.as_bytes() == b"1")
    {
        observed.rotate_due.store(true, Ordering::Relaxed);
    }
    observe_clock(&observed.skew_ms, headers);
}

pub async fn probe_origin(base_url: &str) -> Result<OriginProbe, ProbeFailure> {
    let origin = origin_of(base_url).map_err(|_| ProbeFailure::Other)?;
    let http = crate::tls::http_client_builder()
        .timeout(Duration::from_secs(10))
        .connect_timeout(Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| ProbeFailure::Other)?;
    let started = std::time::Instant::now();
    let sent = Utc::now();
    let response = http
        .get(format!("{origin}/api/v1/utils/health-check/"))
        .send()
        .await
        .map_err(|error| probe_failure(&error))?;
    let elapsed = started.elapsed();
    let midpoint = sent + chrono::Duration::from_std(elapsed / 2).unwrap_or_default();
    let skew = response
        .headers()
        .get(DATE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| DateTime::parse_from_rfc2822(value).ok())
        .map(|server| server.with_timezone(&Utc).signed_duration_since(midpoint));
    Ok(OriginProbe {
        status: response.status().as_u16(),
        skew,
        elapsed,
    })
}

fn probe_failure(error: &reqwest::Error) -> ProbeFailure {
    if error.is_timeout() {
        return ProbeFailure::Timeout;
    }
    let mut text = String::new();
    let mut source: Option<&(dyn std::error::Error + 'static)> = Some(error);
    while let Some(current) = source {
        text.push_str(&current.to_string().to_ascii_lowercase());
        text.push(' ');
        source = current.source();
    }
    if text.contains("certificate") || text.contains("tls") || text.contains("handshake") {
        ProbeFailure::Tls
    } else if text.contains("dns") || text.contains("resolve") || text.contains("lookup") {
        ProbeFailure::Dns
    } else if error.is_connect() {
        ProbeFailure::Connect
    } else {
        ProbeFailure::Other
    }
}

fn observe_clock(skew_ms: &AtomicI64, headers: &HeaderMap) {
    let Some(server) = headers
        .get(DATE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| DateTime::parse_from_rfc2822(value).ok())
    else {
        return;
    };
    let skew = server
        .with_timezone(&Utc)
        .signed_duration_since(Utc::now())
        .num_milliseconds();
    let skew = if skew.abs() < MIN_CLOCK_SKEW_MS {
        0
    } else {
        skew
    };
    skew_ms.store(skew, Ordering::Relaxed);
}

fn retry_after(headers: &HeaderMap) -> Option<Duration> {
    let value = headers.get(RETRY_AFTER)?.to_str().ok()?.trim();
    let wait = match value.parse::<u64>() {
        Ok(seconds) => Duration::from_secs(seconds),
        Err(_) => DateTime::parse_from_rfc2822(value)
            .ok()?
            .with_timezone(&Utc)
            .signed_duration_since(Utc::now())
            .to_std()
            .ok()?,
    };
    Some(wait.min(MAX_RETRY_AFTER))
}

fn is_retryable(status: u16) -> bool {
    matches!(status, 408 | 429) || (500..=599).contains(&status)
}

async fn read_capped(mut reply: reqwest::Response) -> Option<Vec<u8>> {
    if reply
        .content_length()
        .is_some_and(|len| len > MAX_REPLY_BYTES as u64)
    {
        return None;
    }
    let mut body = Vec::new();
    while let Some(chunk) = reply.chunk().await.ok()? {
        if body.len().saturating_add(chunk.len()) > MAX_REPLY_BYTES {
            return None;
        }
        body.extend_from_slice(&chunk);
    }
    Some(body)
}

async fn read_capped_text(reply: reqwest::Response) -> Option<String> {
    String::from_utf8(read_capped(reply).await?).ok()
}

fn protocol_mismatch() -> CtError {
    CtError::Protocol(
        "worker response does not match the protocol; run `cloudthinker update` and try again"
            .into(),
    )
}

async fn rejected(status: u16, body: Option<&str>, wait: Option<Duration>) -> CtError {
    if let Some(message) = body.and_then(outdated) {
        return CtError::Usage(message);
    }
    if status == 401
        && let Some(message) = body.and_then(unauthorized)
    {
        return CtError::Auth(message);
    }
    if status == 403
        && let Some(message) = body.and_then(quota_exceeded)
    {
        return CtError::Usage(message);
    }
    if status == 403
        && let Some(message) = body.and_then(outposts_disabled)
    {
        return CtError::Api {
            status,
            detail: Some(format!("{OUTPOSTS_DISABLED_CODE}: {message}")),
        };
    }
    status_error(status, body.and_then(rejection), wait).await
}

async fn worker_error(observed: &Observed, error: cloudthinker_api::Error<()>) -> CtError {
    match error {
        cloudthinker_api::Error::InvalidResponsePayload(_, _) => protocol_mismatch(),
        cloudthinker_api::Error::ErrorResponse(value) => {
            observe(observed, value.headers());
            let wait = retry_after(value.headers());
            status_error(value.status().as_u16(), None, wait).await
        }
        cloudthinker_api::Error::UnexpectedResponse(response) => {
            observe(observed, response.headers());
            let status = response.status().as_u16();
            let wait = retry_after(response.headers());
            let body = read_capped_text(response).await;
            rejected(status, body.as_deref(), wait).await
        }
        other => match other.status().map(|status| status.as_u16()) {
            Some(status) => status_error(status, None, None).await,
            None => CtError::Transport("worker request failed".into()),
        },
    }
}

async fn status_error(status: u16, rejection: Option<String>, wait: Option<Duration>) -> CtError {
    let detail = rejection.unwrap_or_else(|| "worker request rejected".into());
    if matches!(status, 401 | 403) {
        return CtError::Auth(format!("worker credential expired or revoked ({detail})"));
    }
    if is_retryable(status) {
        if let Some(wait) = wait {
            tokio::time::sleep(wait).await;
        }
        return CtError::Transport(format!(
            "worker request failed with HTTP {status}: {detail}"
        ));
    }
    CtError::Api {
        status,
        detail: Some(detail),
    }
}

pub fn outposts_disabled_message(error: &CtError) -> Option<&str> {
    match error {
        CtError::Api {
            status: 403,
            detail: Some(detail),
        } => detail
            .strip_prefix(OUTPOSTS_DISABLED_CODE)?
            .strip_prefix(": "),
        _ => None,
    }
}

fn outposts_disabled(body: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    let code = value
        .pointer("/error/code")
        .and_then(serde_json::Value::as_str)?;
    (code == OUTPOSTS_DISABLED_CODE).then(|| {
        crate::error::parse_error_message(body).unwrap_or_else(|| OUTPOSTS_DISABLED_FALLBACK.into())
    })
}

fn quota_exceeded(body: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    let code = value
        .pointer("/error/code")
        .and_then(serde_json::Value::as_str)?;
    (code == QUOTA_EXCEEDED_CODE).then(|| {
        crate::error::parse_error_message(body)
            .unwrap_or_else(|| "This Worker limit is reached.".into())
    })
}

fn outdated(body: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    let code = value
        .pointer("/error/code")
        .and_then(serde_json::Value::as_str)?;
    (code == OUTDATED_CODE).then(|| {
        crate::error::parse_error_message(body).unwrap_or_else(|| OUTDATED_FALLBACK.into())
    })
}

fn unauthorized(body: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    let code = value
        .pointer("/error/code")
        .and_then(serde_json::Value::as_str)?;
    (code == UNAUTHORIZED_CODE).then(|| {
        crate::error::parse_error_message(body).unwrap_or_else(|| UNAUTHORIZED_FALLBACK.into())
    })
}

fn rejection(body: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    let code = value
        .pointer("/error/code")
        .and_then(serde_json::Value::as_str)
        .filter(|code| !code.trim().is_empty());
    let message = crate::error::parse_error_message(body);
    let request = value
        .get("request_id")
        .and_then(serde_json::Value::as_str)
        .filter(|id| !id.trim().is_empty());
    let text = match (code, message) {
        (Some(code), Some(message)) if code != message => format!("{code}: {message}"),
        (Some(code), _) => code.to_owned(),
        (None, Some(message)) => message,
        (None, None) => return None,
    };
    Some(match request {
        Some(request) => format!("{text} (request {request})"),
        None => text,
    })
}

fn positive(value: i64) -> CtResult<NonZeroU64> {
    u64::try_from(value)
        .ok()
        .and_then(NonZeroU64::new)
        .ok_or_else(invalid_envelope)
}

fn invalid_envelope() -> CtError {
    CtError::Protocol("invalid worker operation identity".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn worker(base_url: &str) -> WorkerClient {
        WorkerClient::new(base_url, "worker-token", "0.8.0").unwrap()
    }

    fn registration() -> api::RegisterWorkerRequest {
        api::RegisterWorkerRequest {
            worker_instance_id: Uuid::from_u128(1),
            worker_installation_id: Uuid::from_u128(2),
            workdir_id: Uuid::from_u128(3),
            protocol_version: 1.try_into().unwrap(),
            max_assignments: 1.try_into().unwrap(),
            os_arch: "linux-x86_64".parse().unwrap(),
            capabilities: vec![api::ExecutorCapability::Shell],
            label: None,
            repository: None,
        }
    }

    async fn register_error(status: u16, body: &str) -> CtError {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/executor-workers/register"))
            .and(header("X-CloudThinker-CLI-Version", "0.8.0"))
            .respond_with(
                ResponseTemplate::new(status).set_body_raw(body.to_owned(), "application/json"),
            )
            .mount(&server)
            .await;

        worker(&server.uri())
            .register(&registration())
            .await
            .expect_err("the mocked response is not a worker")
    }

    #[tokio::test]
    async fn an_oversized_credential_reply_is_a_protocol_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(STATUS_PATH))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_raw(vec![b' '; MAX_REPLY_BYTES + 1], "application/json"),
            )
            .mount(&server)
            .await;

        let error = worker(&server.uri())
            .credential_status()
            .await
            .expect_err("oversized reply is refused");

        assert!(matches!(error, CtError::Protocol(_)), "got {error:?}");
    }

    #[test]
    fn worker_requires_https_off_loopback() {
        let headers = reqwest::header::HeaderMap::new;

        assert!(client("https://app.example.com", headers()).is_ok());
        assert!(client("http://localhost:8000", headers()).is_ok());
        assert!(client("http://127.0.0.1:8000", headers()).is_ok());
        assert!(client("http://[::1]:8000", headers()).is_ok());
        assert!(matches!(
            client("http://app.example.com", headers()),
            Err(CtError::Usage(_))
        ));
    }

    #[test]
    fn a_fence_token_or_sequence_must_be_a_positive_number() {
        assert_eq!(positive(5).unwrap().get(), 5);
        assert!(matches!(positive(0), Err(CtError::Protocol(_))));
        assert!(matches!(positive(-1), Err(CtError::Protocol(_))));
    }

    #[tokio::test]
    async fn a_rejected_worker_credential_maps_to_auth() {
        for status in [401, 403] {
            let error = register_error(status, "{}").await;
            assert!(matches!(error, CtError::Auth(_)), "got {error:?}");
        }
    }

    #[tokio::test]
    async fn an_unauthorized_worker_gets_the_server_message_once() {
        let message = "This Worker's credential expired or was revoked. Enroll it again with the command from the Workers page.";
        let error = register_error(
            401,
            &format!(
                r#"{{"error":{{"code":"EXECUTOR_WORKER_UNAUTHORIZED","message":"{message}"}},"request_id":"req-4"}}"#
            ),
        )
        .await;
        assert!(
            matches!(&error, CtError::Auth(text) if text == message),
            "got {error:?}"
        );
    }

    #[tokio::test]
    async fn a_rotate_header_asks_for_rotation_and_an_adopted_token_becomes_the_bearer() {
        let server = MockServer::start().await;
        let target = Uuid::from_u128(5);
        let status = serde_json::json!({
            "target_id": target,
            "generation": 1,
            "expires_at": Utc::now(),
            "rotate_after": Utc::now(),
        });
        Mock::given(method("GET"))
            .and(path("/api/v1/executor-workers/credential"))
            .and(header("Authorization", "Bearer worker-token"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("X-CloudThinker-Credential-Rotate", "1")
                    .set_body_json(&status),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/executor-workers/credential"))
            .and(header("Authorization", "Bearer rotated-token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&status))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v1/executor-workers/credential/rotate"))
            .and(header("Authorization", "Bearer worker-token"))
            .and(header("X-CloudThinker-CLI-Version", "0.8.0"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "target_id": target,
                "credential": "rotated-token",
                "credential_generation": 2,
                "expires_at": Utc::now(),
            })))
            .mount(&server)
            .await;
        let client = worker(&server.uri());
        assert!(!client.rotation_due());

        client.credential_status().await.expect("status");
        assert!(client.rotation_due());
        let boot = client.rotate().await.expect("rotated");
        assert_eq!(boot.credential, "rotated-token");
        client.adopt(&boot.credential).expect("adopted");

        assert!(!client.rotation_due());
        let clone = client.clone();
        assert_eq!(
            clone.credential_status().await.expect("status").target_id,
            target
        );
    }

    #[tokio::test]
    async fn turned_off_workers_are_a_wait_state_not_a_rejected_credential() {
        let message =
            "Workers are turned off for this workspace. Ask a workspace admin to turn them on.";
        let error = register_error(
            403,
            &format!(
                r#"{{"error":{{"code":"EXECUTOR_OUTPOSTS_DISABLED","message":"{message}","context":{{"level":"workspace"}}}},"request_id":"req-4"}}"#
            ),
        )
        .await;
        assert_eq!(outposts_disabled_message(&error), Some(message));
    }

    #[tokio::test]
    async fn a_full_quota_is_a_usage_error_not_a_rejected_credential() {
        let message = "The limit of 25 shared Workers in this workspace is reached. Archive an unused Worker, or ask your admin to raise the limit.";
        let error = register_error(
            403,
            &format!(
                r#"{{"error":{{"code":"EXECUTOR_QUOTA_EXCEEDED","message":"{message}"}},"request_id":"req-5"}}"#
            ),
        )
        .await;
        assert!(matches!(error, CtError::Usage(ref text) if text == message));
    }

    #[tokio::test]
    async fn any_other_rejection_keeps_its_status() {
        let error = register_error(409, "{}").await;
        assert!(
            matches!(error, CtError::Api { status: 409, .. }),
            "got {error:?}"
        );
    }

    #[tokio::test]
    async fn a_server_failure_is_retryable_and_keeps_the_server_code() {
        for status in [408, 429, 500, 502, 503, 504] {
            let error = register_error(
                status,
                r#"{"error":{"code":"dependency_unavailable","message":"Try again."},"request_id":"req-7"}"#,
            )
            .await;
            assert!(
                matches!(&error, CtError::Transport(message) if message.contains(&format!("HTTP {status}")) && message.contains("dependency_unavailable: Try again. (request req-7)")),
                "got {error:?}"
            );
        }
    }

    #[tokio::test]
    async fn a_rejection_names_the_server_code() {
        let error = register_error(
            409,
            r#"{"error":{"code":"EXECUTOR_FILESYSTEM_UNAVAILABLE","message":"The folder is unavailable."},"request_id":"req-9"}"#,
        )
        .await;
        assert!(
            matches!(&error, CtError::Api { status: 409, detail: Some(detail) } if detail == "EXECUTOR_FILESYSTEM_UNAVAILABLE: The folder is unavailable. (request req-9)"),
            "got {error:?}"
        );
    }

    #[tokio::test]
    async fn an_outdated_cli_gets_the_server_message_and_stops() {
        let message = "Your CLI is out of date (unknown; minimum 0.8.0). Run `cloudthinker update`, then run a new command.";
        let error = register_error(
            409,
            &format!(
                r#"{{"error":{{"code":"EXECUTOR_WORKER_OUTDATED","message":"{message}"}},"request_id":"req-3"}}"#
            ),
        )
        .await;
        assert!(
            matches!(&error, CtError::Usage(text) if text == message),
            "got {error:?}"
        );
    }

    #[tokio::test]
    async fn a_rate_limited_request_waits_for_retry_after() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/executor-workers/register"))
            .respond_with(ResponseTemplate::new(429).insert_header("Retry-After", "1"))
            .mount(&server)
            .await;
        let started = std::time::Instant::now();

        let error = worker(&server.uri())
            .register(&registration())
            .await
            .expect_err("rate limited");

        assert!(matches!(error, CtError::Transport(_)), "got {error:?}");
        assert!(started.elapsed() >= Duration::from_secs(1));
    }

    #[tokio::test]
    async fn a_lease_deadline_uses_this_machine_clock() {
        let server = MockServer::start().await;
        let assignment = Uuid::from_u128(7);
        let server_now = Utc::now() + chrono::Duration::hours(1);
        let expires = server_now + chrono::Duration::seconds(60);
        Mock::given(method("POST"))
            .and(path(format!(
                "/api/v1/executor-workers/assignments/{assignment}/claim"
            )))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("Date", server_now.to_rfc2822().replace("+0000", "GMT"))
                    .set_body_json(serde_json::json!({
                        "assignment_id": assignment,
                        "fence_token": 1,
                        "lease_expires_at": expires,
                        "lease_token": "lease",
                        "session_id": Uuid::from_u128(8),
                        "state": "active",
                        "target_id": Uuid::from_u128(9),
                        "worker_id": Uuid::from_u128(10),
                    })),
            )
            .mount(&server)
            .await;

        let lease = worker(&server.uri())
            .claim(Uuid::from_u128(10), assignment)
            .await
            .expect("lease");

        let remaining = lease.lease_expires_at - Utc::now();
        assert!(
            remaining > chrono::Duration::seconds(55) && remaining <= chrono::Duration::seconds(61),
            "remaining {remaining}"
        );
    }

    #[tokio::test]
    async fn a_body_that_is_not_the_protocol_is_a_protocol_error() {
        let error = register_error(200, r#"{"worker_id":"not-a-uuid"}"#).await;
        assert!(matches!(error, CtError::Protocol(_)), "got {error:?}");
    }

    #[tokio::test]
    async fn an_unreachable_broker_is_a_transport_error() {
        let error = worker("http://127.0.0.1:1")
            .register(&registration())
            .await
            .expect_err("nothing listens on the reserved port");
        assert!(matches!(error, CtError::Transport(_)), "got {error:?}");
    }
}
