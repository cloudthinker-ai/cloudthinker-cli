use std::collections::BTreeMap;
use std::sync::Arc;

use base64::Engine;
use cloudthinker_client::{CtError, CtResult, worker_api::WorkerClient, worker_types as api};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::{
    background::{self, ShimImage},
    config::WorkdirIdentity,
    files, shell,
    skill_bundles::{SKILL_BUNDLES_ENV, SkillBundleChunk, SkillBundleStore},
};

type Dispatched = Result<(Value, Option<Vec<u8>>), &'static str>;

pub struct WorkdirExecutor {
    pub identity: Arc<WorkdirIdentity>,
    environment: BTreeMap<String, String>,
    secrets: Vec<String>,
    bundles: SkillBundleStore,
    shim: Arc<ShimImage>,
}

impl WorkdirExecutor {
    pub fn new(
        identity: Arc<WorkdirIdentity>,
        mut environment: BTreeMap<String, String>,
        mut secrets: Vec<String>,
        shim: ShimImage,
    ) -> Self {
        secrets.push(identity.path.to_string_lossy().into_owned());
        let bundles = SkillBundleStore::new(&identity.state);
        let bundles_path = bundles.root().to_string_lossy().into_owned();
        secrets.push(bundles_path.clone());
        environment.insert(SKILL_BUNDLES_ENV.into(), bundles_path);
        Self {
            identity,
            environment,
            secrets,
            bundles,
            shim: Arc::new(shim),
        }
    }

    pub async fn execute(
        &self,
        envelope: &api::OperationEnvelope,
        cancel: CancellationToken,
        worker: uuid::Uuid,
        client: &WorkerClient,
    ) -> CtResult<api::WorkerOperationResult> {
        self.identity.revalidate()?;
        let output = self.dispatch(envelope, cancel, worker, client).await?;
        if matches!(
            output,
            Err("EXECUTOR_ARTIFACT_UPLOAD_UNCONFIRMED"
                | "EXECUTOR_ARTIFACT_RECEIPT_INVALID"
                | "EXECUTOR_ARTIFACT_UPLOAD_CANCELLED")
        ) {
            return Err(CtError::Transport(
                "artifact effect is unconfirmed; reconciliation required".into(),
            ));
        }
        let (mut value, raw, status, mut state) = match output {
            Ok((value, raw)) => (value, raw, 200, api::WorkerOperationResultState::Succeeded),
            Err(code) => (
                json!({"status":"error","error":{"code":code,"message":code},"message":code}),
                None,
                422,
                api::WorkerOperationResultState::Failed,
            ),
        };
        if value.pointer("/result/error_code").and_then(Value::as_str) == Some("CANCELLED") {
            state = api::WorkerOperationResultState::Cancelled;
        }
        let mut secrets = self.secrets.clone();
        secrets.extend([envelope.lease_token.clone(), envelope.nonce.clone()]);
        sanitize(&mut value, &secrets);
        let binary = raw.is_some();
        let bytes = if let Some(bytes) = raw {
            bytes
        } else {
            serde_json::to_vec(&value)
                .map_err(|_| CtError::Protocol("worker result encoding failed".into()))?
        };
        if bytes.len() > 2_097_152 {
            return failure("RESULT_TOO_LARGE");
        }
        Ok(api::WorkerOperationResult {
            state,
            status_code: status,
            content_type: if binary {
                api::WorkerOperationResultContentType::ApplicationOctetStream
            } else {
                api::WorkerOperationResultContentType::ApplicationJson
            },
            content_base64: encode_content(&bytes)?,
        })
    }

    async fn dispatch(
        &self,
        envelope: &api::OperationEnvelope,
        cancel: CancellationToken,
        worker: uuid::Uuid,
        client: &WorkerClient,
    ) -> CtResult<Dispatched> {
        Ok(match &envelope.payload {
            api::Payload::SkillBundleChunk(chunk) => match SkillBundleChunk::from_api(chunk) {
                Err(error) => Err(error),
                Ok(chunk) => {
                    let bundles = self.bundles.clone();
                    match tokio::task::spawn_blocking(move || bundles.install(&chunk)).await {
                        Ok(result) => result.map(|value| (value, None)),
                        Err(_) => Err("BUNDLE_OPERATION_INTERRUPTED"),
                    }
                }
            },
            api::Payload::FlushOutput(_) => {
                super::artifacts::flush(client, self.identity.clone(), envelope, worker, cancel)
                    .await
                    .map(|v| (v, None))
            }
            api::Payload::ScriptRun(script) => shell::execute(
                &self.identity.root,
                script,
                &self.environment,
                envelope.deadline_at,
                cancel,
            )
            .await
            .map(|value| (value, None)),
            api::Payload::BackgroundOperation(op) => background::execute(
                self.identity.clone(),
                self.shim.clone(),
                op.clone(),
                self.environment.clone(),
                envelope.deadline_at,
                cancel,
            )
            .await
            .map(|value| (value, None)),
            operation => {
                let dir = self
                    .identity
                    .root
                    .try_clone()
                    .map_err(|_| CtError::Store("worker directory unavailable".into()))?;
                let operation = operation.clone();
                tokio::task::spawn_blocking(move || match operation {
                    api::Payload::FileOperation(operation) => {
                        files::execute(&dir, &operation).map(|v| (v, None))
                    }
                    api::Payload::FilesDeliverables(_) => {
                        files::deliverables(&dir).map(|v| (v, None))
                    }
                    api::Payload::FilesList(request) => {
                        files::list(&dir, &request).map(|v| (v, None))
                    }
                    api::Payload::FileContent(request) => {
                        files::content(&dir, &request).map(|v| (v, None))
                    }
                    api::Payload::FileDownload(request) => {
                        files::read(&dir, &request.path, files::MAX_BYTES)
                            .map(|v| (Value::Null, Some(v)))
                    }
                    _ => Err("EXECUTOR_OPERATION_UNSUPPORTED"),
                })
                .await
                .map_err(|_| CtError::Store("worker file operation interrupted".into()))?
            }
        })
    }
}

pub fn failure(code: &str) -> CtResult<api::WorkerOperationResult> {
    let bytes = serde_json::to_vec(
        &json!({"status":"error","message":code,"error":{"code":code,"message":code}}),
    )
    .map_err(|_| CtError::Protocol("worker failure encoding failed".into()))?;
    Ok(api::WorkerOperationResult {
        state: api::WorkerOperationResultState::Failed,
        status_code: 422,
        content_type: api::WorkerOperationResultContentType::ApplicationJson,
        content_base64: encode_content(&bytes)?,
    })
}

fn encode_content(bytes: &[u8]) -> CtResult<api::WorkerOperationResultContentBase64> {
    base64::engine::general_purpose::STANDARD
        .encode(bytes)
        .parse()
        .map_err(|_| CtError::Protocol("worker result exceeds protocol limit".into()))
}

fn sanitize(value: &mut Value, secrets: &[String]) {
    match value {
        Value::String(text) => {
            for secret in secrets.iter().filter(|s| !s.is_empty()) {
                *text = text.replace(secret, "[redacted]");
            }
        }
        Value::Array(values) => {
            for value in values {
                sanitize(value, secrets);
            }
        }
        Value::Object(values) => {
            for value in values.values_mut() {
                sanitize(value, secrets);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use cloudthinker_client::auth::worker_store::private_directory;

    use super::*;

    struct Fixture {
        _temporary: tempfile::TempDir,
        executor: WorkdirExecutor,
        client: WorkerClient,
    }

    async fn fixture() -> Fixture {
        let temporary = tempfile::tempdir().unwrap();
        let work = temporary.path().join("project");
        std::fs::create_dir(&work).unwrap();
        let state = temporary.path().join("state");
        private_directory(&state).unwrap();
        let identity =
            Arc::new(WorkdirIdentity::open(&work, &state, uuid::Uuid::new_v4()).unwrap());
        let shim = background::ShimImage::open("/bin/true".into(), &BTreeMap::new())
            .await
            .unwrap();
        Fixture {
            _temporary: temporary,
            executor: WorkdirExecutor::new(identity, BTreeMap::new(), Vec::new(), shim),
            client: WorkerClient::new("http://127.0.0.1:1", "test-token").unwrap(),
        }
    }

    fn envelope(payload: Value) -> api::OperationEnvelope {
        serde_json::from_value(json!({
            "target_id": uuid::Uuid::new_v4(),
            "session_id": uuid::Uuid::new_v4(),
            "assignment_id": uuid::Uuid::new_v4(),
            "operation_id": uuid::Uuid::new_v4(),
            "operation_sequence": 1,
            "fence_token": 1,
            "lease_token": "lease-secret",
            "nonce": "nonce-secret",
            "deadline_at": (Utc::now() + chrono::Duration::seconds(30)).to_rfc3339(),
            "effect": "mutation",
            "payload": payload,
            "request_digest": "a".repeat(64),
        }))
        .unwrap()
    }

    fn decode(result: &api::WorkerOperationResult) -> Value {
        serde_json::from_slice(
            &base64::engine::general_purpose::STANDARD
                .decode(result.content_base64.as_str())
                .unwrap(),
        )
        .unwrap()
    }

    impl Fixture {
        async fn run(&self, payload: Value) -> CtResult<api::WorkerOperationResult> {
            self.run_with(payload, CancellationToken::new()).await
        }

        async fn run_with(
            &self,
            payload: Value,
            cancel: CancellationToken,
        ) -> CtResult<api::WorkerOperationResult> {
            self.executor
                .execute(
                    &envelope(payload),
                    cancel,
                    uuid::Uuid::new_v4(),
                    &self.client,
                )
                .await
        }
    }

    #[test]
    fn sanitize_replaces_every_secret_at_any_depth_and_ignores_empty_ones() {
        let mut value = json!({
            "text": "root=/served/dir token=lease-secret",
            "list": ["nonce-secret", {"nested": "/served/dir"}],
            "count": 7,
        });
        sanitize(
            &mut value,
            &[
                "/served/dir".into(),
                "lease-secret".into(),
                "nonce-secret".into(),
                String::new(),
            ],
        );
        assert_eq!(value["text"], "root=[redacted] token=[redacted]");
        assert_eq!(value["list"][0], "[redacted]");
        assert_eq!(value["list"][1]["nested"], "[redacted]");
        assert_eq!(value["count"], 7);
    }

    #[tokio::test]
    async fn a_result_redacts_the_workdir_bundle_root_lease_and_nonce() {
        let fixture = fixture().await;
        let secret = format!(
            "{} {} lease-secret nonce-secret",
            fixture.executor.identity.path.display(),
            fixture.executor.bundles.root().display()
        );
        let result = fixture
            .run(json!({
                "kind": "file_operation",
                "endpoint": "write",
                "request": {"file_path": "leak.txt", "content": secret},
            }))
            .await
            .unwrap();
        assert_eq!(result.state, api::WorkerOperationResultState::Succeeded);
        let body = decode(&result);
        assert_eq!(
            body["content"],
            "[redacted] [redacted] [redacted] [redacted]"
        );
        assert!(!serde_json::to_string(&body).unwrap().contains("secret"));
    }

    #[tokio::test]
    async fn an_unsupported_payload_fails_the_operation_without_stopping_the_worker() {
        let fixture = fixture().await;
        let result = fixture
            .run(json!({"kind": "convert_office_pdf", "path": "deck.pptx"}))
            .await
            .unwrap();
        assert_eq!(result.state, api::WorkerOperationResultState::Failed);
        assert_eq!(result.status_code, 422);
        assert_eq!(
            decode(&result)["error"]["code"],
            "EXECUTOR_OPERATION_UNSUPPORTED"
        );
    }

    #[tokio::test]
    async fn a_cancelled_script_settles_as_cancelled_not_as_a_failure() {
        let fixture = fixture().await;
        let cancel = CancellationToken::new();
        let stopping = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(150)).await;
            stopping.cancel();
        });
        let result = fixture
            .run_with(
                json!({"kind": "script", "script": "sleep 30 & wait"}),
                cancel,
            )
            .await
            .unwrap();
        assert_eq!(result.state, api::WorkerOperationResultState::Cancelled);
        assert_eq!(result.status_code, 200);
        assert_eq!(decode(&result)["result"]["error_code"], "CANCELLED");
    }

    #[tokio::test]
    async fn an_unconfirmed_artifact_effect_stops_the_assignment_for_reconciliation() {
        let fixture = fixture().await;
        let output = fixture.executor.identity.path.join("output");
        std::fs::create_dir(&output).unwrap();
        std::fs::write(output.join("report.txt"), b"payload").unwrap();
        let cancel = CancellationToken::new();
        cancel.cancel();
        assert!(matches!(
            fixture
                .run_with(json!({"kind": "flush_output"}), cancel)
                .await,
            Err(CtError::Transport(message))
                if message.contains("unconfirmed")
        ));
    }

    #[tokio::test]
    async fn a_result_over_the_protocol_ceiling_is_replaced_by_a_failure() {
        let fixture = fixture().await;
        let root = &fixture.executor.identity.path;
        std::fs::create_dir_all(root.join("a/b/c/d")).unwrap();
        std::fs::write(
            root.join("a/b/c/d/big.bin"),
            vec![0xff_u8; files::MAX_BYTES],
        )
        .unwrap();
        for parent in ["a/b/c/d", "a/b/c", "a/b", "a", ""] {
            for name in ["AGENTS.md", "CONVENTIONS.md", "RULES.md"] {
                std::fs::write(root.join(parent).join(name), vec![b'x'; 20_000]).unwrap();
            }
        }
        let result = fixture
            .run(json!({
                "kind": "file_operation",
                "endpoint": "read",
                "request": {
                    "file_path": "a/b/c/d/big.bin",
                    "binary": true,
                    "convention_filenames": ["AGENTS.md", "CONVENTIONS.md", "RULES.md"],
                },
            }))
            .await
            .unwrap();
        assert_eq!(result.state, api::WorkerOperationResultState::Failed);
        assert_eq!(result.status_code, 422);
        assert_eq!(decode(&result)["error"]["code"], "RESULT_TOO_LARGE");
    }
}
