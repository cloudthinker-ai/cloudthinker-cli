use std::num::NonZeroU64;
use std::time::Duration;

use uuid::Uuid;

use crate::{CtError, CtResult, origin_of, worker_types as api};

#[derive(Clone)]
pub struct WorkerClient {
    client: cloudthinker_api::Client,
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
        })
    }

    pub async fn exchange(base_url: &str, reference: &str) -> CtResult<api::WorkerBootstrap> {
        let client = client(base_url, reqwest::header::HeaderMap::new())?;
        response(
            client
                .executor_targets_exchange_registration(&api::ExchangeWorkerRegistrationRequest {
                    reference: reference.into(),
                })
                .await,
        )
        .await
    }

    pub async fn register(
        &self,
        request: &api::RegisterWorkerRequest,
    ) -> CtResult<api::WorkerPublic> {
        response(self.client.executor_targets_register_worker(request).await).await
    }

    pub async fn conformance(&self, worker_id: Uuid) -> CtResult<api::WorkerConformancePublic> {
        response(
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
        response(
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
        response(
            self.client
                .executor_targets_pending_assignments(Some(15.0), &worker)
                .await,
        )
        .await
    }

    pub async fn claim(&self, worker: Uuid, assignment: Uuid) -> CtResult<api::AssignmentLease> {
        response(
            self.client
                .executor_targets_claim_assignment(
                    &assignment,
                    &api::ClaimAssignmentRequest { worker_id: worker },
                )
                .await,
        )
        .await
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
        response(
            self.client
                .executor_targets_heartbeat_assignment(&lease.assignment_id, &body)
                .await,
        )
        .await
    }

    pub async fn operations(
        &self,
        lease: &api::AssignmentLease,
        after: u64,
        batch_size: u64,
    ) -> CtResult<Vec<api::OperationEnvelope>> {
        response(
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
    }

    pub async fn artifact_grant(
        &self,
        assignment: Uuid,
        request: &api::IssueArtifactGrantRequest,
    ) -> CtResult<api::ArtifactGrantPublic> {
        response(
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
        response(
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
        response(
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
        response(
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
        response(
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
        response(
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
    result: Result<cloudthinker_api::ResponseValue<T>, cloudthinker_api::Error<()>>,
) -> CtResult<T> {
    match result {
        Ok(response) => Ok(response.into_inner()),
        Err(cloudthinker_api::Error::InvalidResponsePayload(_, _)) => Err(CtError::Protocol(
            "worker response does not match the protocol".into(),
        )),
        Err(error) => match error.status().map(|s| s.as_u16()) {
            Some(401 | 403) => Err(CtError::Auth("worker credential expired or revoked".into())),
            Some(status) => Err(CtError::Api {
                status,
                detail: Some("worker request rejected".into()),
            }),
            None => Err(CtError::Transport("worker request failed".into())),
        },
    }
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
