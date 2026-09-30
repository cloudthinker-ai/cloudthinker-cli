use std::num::NonZeroU64;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use chrono::{DateTime, Utc};
use reqwest::header::{DATE, HeaderMap, RETRY_AFTER};
use uuid::Uuid;

use crate::{CtError, CtResult, origin_of, worker_types as api};

const MIN_CLOCK_SKEW_MS: i64 = 2_000;
const MAX_RETRY_AFTER: Duration = Duration::from_secs(30);

#[derive(Clone)]
pub struct WorkerClient {
    client: cloudthinker_api::Client,
    skew_ms: Arc<AtomicI64>,
}

impl WorkerClient {
    pub fn new(base_url: &str, credential: &str) -> CtResult<Self> {
        let mut headers = reqwest::header::HeaderMap::new();
        let mut bearer = reqwest::header::HeaderValue::from_str(&format!("Bearer {credential}"))
            .map_err(|_| CtError::Auth("invalid worker credential".into()))?;
        bearer.set_sensitive(true);
        headers.insert(reqwest::header::AUTHORIZATION, bearer);
        Ok(Self {
            client: client(base_url, headers)?,
            skew_ms: Arc::new(AtomicI64::new(0)),
        })
    }

    pub async fn exchange(base_url: &str, reference: &str) -> CtResult<api::WorkerBootstrap> {
        let client = client(base_url, reqwest::header::HeaderMap::new())?;
        let skew = AtomicI64::new(0);
        response(
            &skew,
            client
                .executor_targets_exchange_registration(&api::ExchangeWorkerRegistrationRequest {
                    reference: reference.into(),
                })
                .await,
        )
        .await
    }

    pub fn server_skew(&self) -> chrono::Duration {
        chrono::Duration::milliseconds(self.skew_ms.load(Ordering::Relaxed))
    }

    fn local_time(&self, server: DateTime<Utc>) -> DateTime<Utc> {
        server - self.server_skew()
    }

    async fn call<T>(
        &self,
        result: Result<cloudthinker_api::ResponseValue<T>, cloudthinker_api::Error<()>>,
    ) -> CtResult<T> {
        response(&self.skew_ms, result).await
    }

    pub async fn register(
        &self,
        request: &api::RegisterWorkerRequest,
    ) -> CtResult<api::WorkerPublic> {
        self.call(self.client.executor_targets_register_worker(request).await)
            .await
    }

    pub async fn conformance(&self, worker_id: Uuid) -> CtResult<api::WorkerConformancePublic> {
        self.call(
            self.client
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
    ) -> CtResult<api::WorkerPublic> {
        let state = if draining {
            api::WorkerHeartbeatRequestState::Draining
        } else {
            api::WorkerHeartbeatRequestState::Online
        };
        self.call(
            self.client
                .executor_targets_worker_heartbeat(&api::WorkerHeartbeatRequest {
                    worker_id: worker,
                    state,
                })
                .await,
        )
        .await
    }

    pub async fn pending(&self, worker: Uuid) -> CtResult<Vec<api::PendingAssignmentPublic>> {
        self.call(
            self.client
                .executor_targets_pending_assignments(Some(15.0), &worker)
                .await,
        )
        .await
    }

    pub async fn claim(&self, worker: Uuid, assignment: Uuid) -> CtResult<api::AssignmentLease> {
        self.call(
            self.client
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
            self.client
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
            self.client
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
            self.client
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
            self.client
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
            self.client
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
            self.client
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
            self.client
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
            self.client
                .executor_targets_release_assignment(&lease.assignment_id, &body)
                .await,
        )
        .await
    }
}

fn client(
    base_url: &str,
    headers: reqwest::header::HeaderMap,
) -> CtResult<cloudthinker_api::Client> {
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
    let http = reqwest::Client::builder()
        .default_headers(headers)
        .timeout(Duration::from_secs(25))
        .connect_timeout(Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| CtError::Transport("worker HTTP client unavailable".into()))?;
    Ok(cloudthinker_api::Client::new_with_client(&origin, http))
}

async fn response<T>(
    skew_ms: &AtomicI64,
    result: Result<cloudthinker_api::ResponseValue<T>, cloudthinker_api::Error<()>>,
) -> CtResult<T> {
    match result {
        Ok(value) => {
            observe_clock(skew_ms, value.headers());
            Ok(value.into_inner())
        }
        Err(error) => Err(worker_error(skew_ms, error).await),
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

async fn worker_error(skew_ms: &AtomicI64, error: cloudthinker_api::Error<()>) -> CtError {
    match error {
        cloudthinker_api::Error::InvalidResponsePayload(_, _) => {
            CtError::Protocol("worker response does not match the protocol".into())
        }
        cloudthinker_api::Error::ErrorResponse(value) => {
            observe_clock(skew_ms, value.headers());
            let wait = retry_after(value.headers());
            status_error(value.status().as_u16(), None, wait).await
        }
        cloudthinker_api::Error::UnexpectedResponse(response) => {
            observe_clock(skew_ms, response.headers());
            let status = response.status().as_u16();
            let wait = retry_after(response.headers());
            let body = response.text().await.ok();
            status_error(status, body.as_deref().and_then(rejection), wait).await
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

    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn worker(base_url: &str) -> WorkerClient {
        WorkerClient::new(base_url, "worker-token").unwrap()
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
        }
    }

    async fn register_error(status: u16, body: &str) -> CtError {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/executor-workers/register"))
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
