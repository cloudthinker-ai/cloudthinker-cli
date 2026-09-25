use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use base64::Engine;
use chrono::Utc;
use cloudthinker_client::{worker_api::WorkerClient, worker_types as api};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::{config::WorkdirIdentity, files};

const UPLOAD_CONCURRENCY: usize = 4;

pub async fn flush(
    client: &WorkerClient,
    identity: Arc<WorkdirIdentity>,
    envelope: &api::OperationEnvelope,
    worker: Uuid,
    cancel: CancellationToken,
) -> Result<Value, &'static str> {
    let scanning = identity.clone();
    let paths = tokio::task::spawn_blocking(move || files::artifact_paths(&scanning.root))
        .await
        .map_err(|_| "EXECUTOR_ARTIFACT_SCAN_FAILED")??;
    let envelope = Arc::new(envelope.clone());
    let total = Arc::new(AtomicUsize::new(0));
    let mut queued = paths.into_iter();
    let mut running = JoinSet::new();
    let mut uploaded = Vec::new();
    let mut failure = None;
    loop {
        while failure.is_none() && running.len() < UPLOAD_CONCURRENCY {
            let Some(path) = queued.next() else { break };
            running.spawn(upload(
                client.clone(),
                identity.clone(),
                envelope.clone(),
                worker,
                cancel.clone(),
                total.clone(),
                path,
            ));
        }
        let Some(joined) = running.join_next().await else {
            break;
        };
        match joined.map_err(|_| "EXECUTOR_ARTIFACT_READ_FAILED") {
            Ok(Ok(path)) => uploaded.push(path),
            Ok(Err(error)) | Err(error) => failure = failure.or(Some(error)),
        }
    }
    if let Some(error) = failure {
        return Err(error);
    }
    uploaded.sort();
    Ok(json!({"status":"synced", "count":uploaded.len(), "uploaded":uploaded}))
}

async fn upload(
    client: WorkerClient,
    identity: Arc<WorkdirIdentity>,
    envelope: Arc<api::OperationEnvelope>,
    worker: Uuid,
    cancel: CancellationToken,
    total: Arc<AtomicUsize>,
    path: PathBuf,
) -> Result<String, &'static str> {
    if cancel.is_cancelled() || Utc::now() >= envelope.deadline_at {
        return Err("EXECUTOR_ARTIFACT_UPLOAD_CANCELLED");
    }
    if total.load(Ordering::SeqCst) >= files::MAX_ARTIFACT_BYTES {
        return Err("EXECUTOR_ARTIFACT_LIMIT_EXCEEDED");
    }
    let path = path.to_string_lossy().into_owned();
    let reading = identity.clone();
    let reading_path = path.clone();
    let bytes = tokio::task::spawn_blocking(move || {
        reading.revalidate().map_err(|_| "TRUSTED_ROOT_REJECTED")?;
        files::read(&reading.root, &reading_path, files::MAX_BYTES)
    })
    .await
    .map_err(|_| "EXECUTOR_ARTIFACT_READ_FAILED")??;
    if total.fetch_add(bytes.len(), Ordering::SeqCst) + bytes.len() > files::MAX_ARTIFACT_BYTES {
        return Err("EXECUTOR_ARTIFACT_LIMIT_EXCEEDED");
    }
    let digest = format!("{:x}", Sha256::digest(&bytes));
    let fence = (envelope.fence_token as u64)
        .try_into()
        .map_err(|_| "EXECUTOR_ARTIFACT_LEASE_INVALID")?;
    let grant = client
        .artifact_grant(
            envelope.assignment_id,
            &api::IssueArtifactGrantRequest {
                worker_id: worker,
                fence_token: fence,
                lease_token: envelope.lease_token.clone(),
                operation_id: envelope.operation_id,
                path: path.parse().map_err(|_| "EXECUTOR_ARTIFACT_PATH_INVALID")?,
                digest: digest
                    .parse()
                    .map_err(|_| "EXECUTOR_ARTIFACT_DIGEST_INVALID")?,
                size: bytes.len() as i64,
            },
        )
        .await
        .map_err(|_| "EXECUTOR_ARTIFACT_GRANT_REJECTED")?;
    let body = api::UploadWorkerArtifactRequest {
        worker_id: worker,
        fence_token: fence,
        lease_token: envelope.lease_token.clone(),
        token: grant.token,
        content_base64: base64::engine::general_purpose::STANDARD
            .encode(&bytes)
            .parse()
            .map_err(|_| "EXECUTOR_ARTIFACT_LIMIT_EXCEEDED")?,
    };
    let receipt = tokio::select! {
        _=cancel.cancelled()=>return Err("EXECUTOR_ARTIFACT_UPLOAD_CANCELLED"),
        result=client.upload_artifact(envelope.assignment_id, grant.grant_id, &body)=>result.map_err(|_| "EXECUTOR_ARTIFACT_UPLOAD_UNCONFIRMED")?,
    };
    if receipt.path != path || receipt.digest != digest || receipt.size != bytes.len() as i64 {
        return Err("EXECUTOR_ARTIFACT_RECEIPT_INVALID");
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Mutex;
    use std::time::Duration;

    use cloudthinker_client::auth::worker_store::private_directory;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    use super::*;

    struct Stub {
        address: std::net::SocketAddr,
        peak: Arc<AtomicUsize>,
        uploads: Arc<AtomicUsize>,
    }

    #[derive(Clone, Copy, PartialEq)]
    enum Receipt {
        Honest,
        WrongDigest,
    }

    fn body_of(request: &str) -> Value {
        let body = request.split_once("\r\n\r\n").map_or("", |(_, body)| body);
        serde_json::from_str(body).unwrap()
    }

    async fn stub(receipt: Receipt, delay: Duration) -> Stub {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let grants: Arc<Mutex<HashMap<String, Value>>> = Arc::new(Mutex::new(HashMap::new()));
        let live = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let uploads = Arc::new(AtomicUsize::new(0));
        let stub = Stub {
            address,
            peak: peak.clone(),
            uploads: uploads.clone(),
        };
        tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    return;
                };
                let grants = grants.clone();
                let live = live.clone();
                let peak = peak.clone();
                let uploads = uploads.clone();
                tokio::spawn(async move {
                    let mut request = Vec::new();
                    loop {
                        let mut buffer = [0_u8; 65536];
                        let Ok(count) = stream.read(&mut buffer).await else {
                            return;
                        };
                        if count == 0 {
                            return;
                        }
                        request.extend_from_slice(&buffer[..count]);
                        let text = String::from_utf8_lossy(&request).into_owned();
                        let Some((head, body)) = text.split_once("\r\n\r\n") else {
                            continue;
                        };
                        let length: usize = head
                            .lines()
                            .find_map(|line| {
                                line.strip_prefix("content-length: ")
                                    .or_else(|| line.strip_prefix("Content-Length: "))
                            })
                            .and_then(|value| value.trim().parse().ok())
                            .unwrap_or(0);
                        if body.len() < length {
                            continue;
                        }
                        let target = head
                            .lines()
                            .next()
                            .and_then(|line| line.split(' ').nth(1))
                            .unwrap_or_default()
                            .to_owned();
                        let last = target.rsplit('/').next().unwrap_or_default().to_owned();
                        let body = body_of(&text);
                        let payload = if last == "artifact-grants" {
                            let grant_id = Uuid::new_v4();
                            grants
                                .lock()
                                .unwrap()
                                .insert(grant_id.to_string(), body.clone());
                            json!({
                                "expires_at": "2030-01-01T00:00:00Z",
                                "grant_id": grant_id,
                                "token": "grant-token",
                            })
                        } else {
                            let inflight = live.fetch_add(1, Ordering::SeqCst) + 1;
                            peak.fetch_max(inflight, Ordering::SeqCst);
                            uploads.fetch_add(1, Ordering::SeqCst);
                            tokio::time::sleep(delay).await;
                            live.fetch_sub(1, Ordering::SeqCst);
                            let granted = grants.lock().unwrap().get(&last).cloned().unwrap();
                            json!({
                                "digest": if receipt == Receipt::WrongDigest {
                                    json!("f".repeat(64))
                                } else {
                                    granted["digest"].clone()
                                },
                                "path": granted["path"],
                                "size": granted["size"],
                            })
                        };
                        let payload = payload.to_string();
                        let response = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
                            payload.len()
                        );
                        let _ = stream.write_all(response.as_bytes()).await;
                        let _ = stream.shutdown().await;
                        return;
                    }
                });
            }
        });
        stub
    }

    struct Fixture {
        _temporary: tempfile::TempDir,
        identity: Arc<WorkdirIdentity>,
    }

    fn fixture() -> Fixture {
        let temporary = tempfile::tempdir().unwrap();
        let work = temporary.path().join("project");
        std::fs::create_dir(&work).unwrap();
        let state = temporary.path().join("state");
        private_directory(&state).unwrap();
        let identity = Arc::new(WorkdirIdentity::open(&work, &state, Uuid::new_v4()).unwrap());
        Fixture {
            _temporary: temporary,
            identity,
        }
    }

    fn deliverable(fixture: &Fixture, name: &str, content: &[u8]) {
        let path = fixture.identity.path.join("output");
        std::fs::create_dir_all(&path).unwrap();
        std::fs::write(path.join(name), content).unwrap();
    }

    fn envelope(deadline: chrono::DateTime<Utc>) -> api::OperationEnvelope {
        serde_json::from_value(json!({
            "target_id": Uuid::new_v4(),
            "session_id": Uuid::new_v4(),
            "assignment_id": Uuid::new_v4(),
            "operation_id": Uuid::new_v4(),
            "operation_sequence": 1,
            "fence_token": 1,
            "lease_token": "lease",
            "nonce": "nonce",
            "deadline_at": deadline.to_rfc3339(),
            "effect": "mutation",
            "payload": {"kind": "flush_output"},
            "request_digest": "a".repeat(64),
        }))
        .unwrap()
    }

    fn client(stub: &Stub) -> WorkerClient {
        WorkerClient::new(&format!("http://{}", stub.address), "test-token").unwrap()
    }

    fn later() -> chrono::DateTime<Utc> {
        Utc::now() + chrono::Duration::seconds(60)
    }

    #[tokio::test]
    async fn flush_reports_nothing_to_upload_for_an_empty_workdir() {
        let fixture = fixture();
        let stub = stub(Receipt::Honest, Duration::ZERO).await;
        let result = flush(
            &client(&stub),
            fixture.identity.clone(),
            &envelope(later()),
            Uuid::new_v4(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(result, json!({"status":"synced","count":0,"uploaded":[]}));
        assert_eq!(stub.uploads.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn ca_wo_19_flush_posts_every_deliverable_to_its_own_grant() {
        let fixture = fixture();
        for index in 0..8 {
            deliverable(&fixture, &format!("file-{index}.txt"), b"payload");
        }
        let stub = stub(Receipt::Honest, Duration::ZERO).await;
        let result = flush(
            &client(&stub),
            fixture.identity.clone(),
            &envelope(later()),
            Uuid::new_v4(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(result["count"], 8);
        let uploaded: Vec<&str> = result["uploaded"]
            .as_array()
            .unwrap()
            .iter()
            .map(|value| value.as_str().unwrap())
            .collect();
        assert_eq!(uploaded[0], "output/file-0.txt");
        assert!(uploaded.windows(2).all(|pair| pair[0] < pair[1]));
        assert_eq!(stub.uploads.load(Ordering::SeqCst), 8);
    }

    #[tokio::test]
    async fn flush_uploads_several_artifacts_at_once_under_a_bound() {
        let fixture = fixture();
        for index in 0..8 {
            deliverable(&fixture, &format!("file-{index}.txt"), b"payload");
        }
        let stub = stub(Receipt::Honest, Duration::from_millis(150)).await;
        let result = flush(
            &client(&stub),
            fixture.identity.clone(),
            &envelope(later()),
            Uuid::new_v4(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(result["count"], 8);
        let peak = stub.peak.load(Ordering::SeqCst);
        assert!(peak > 1, "uploads never overlapped");
        assert!(peak <= UPLOAD_CONCURRENCY, "unbounded fan-out: {peak}");
    }

    #[tokio::test]
    async fn ca_wo_19_flush_rejects_a_receipt_whose_digest_differs_from_what_was_sent() {
        let fixture = fixture();
        deliverable(&fixture, "report.txt", b"payload");
        let stub = stub(Receipt::WrongDigest, Duration::ZERO).await;
        assert_eq!(
            flush(
                &client(&stub),
                fixture.identity.clone(),
                &envelope(later()),
                Uuid::new_v4(),
                CancellationToken::new(),
            )
            .await,
            Err("EXECUTOR_ARTIFACT_RECEIPT_INVALID")
        );
    }

    #[tokio::test]
    async fn flush_stops_before_the_first_upload_when_cancelled_or_past_the_deadline() {
        let fixture = fixture();
        deliverable(&fixture, "report.txt", b"payload");
        let stub = stub(Receipt::Honest, Duration::ZERO).await;
        let cancelled = CancellationToken::new();
        cancelled.cancel();
        assert_eq!(
            flush(
                &client(&stub),
                fixture.identity.clone(),
                &envelope(later()),
                Uuid::new_v4(),
                cancelled,
            )
            .await,
            Err("EXECUTOR_ARTIFACT_UPLOAD_CANCELLED")
        );
        assert_eq!(
            flush(
                &client(&stub),
                fixture.identity.clone(),
                &envelope(Utc::now() - chrono::Duration::seconds(1)),
                Uuid::new_v4(),
                CancellationToken::new(),
            )
            .await,
            Err("EXECUTOR_ARTIFACT_UPLOAD_CANCELLED")
        );
        assert_eq!(stub.uploads.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn flush_refuses_a_workdir_that_was_replaced_under_it() {
        let fixture = fixture();
        deliverable(&fixture, "report.txt", b"payload");
        let stub = stub(Receipt::Honest, Duration::ZERO).await;
        let moved = fixture.identity.path.with_extension("moved");
        std::fs::rename(&fixture.identity.path, &moved).unwrap();
        std::fs::create_dir(&fixture.identity.path).unwrap();
        assert_eq!(
            flush(
                &client(&stub),
                fixture.identity.clone(),
                &envelope(later()),
                Uuid::new_v4(),
                CancellationToken::new(),
            )
            .await,
            Err("TRUSTED_ROOT_REJECTED")
        );
        assert_eq!(stub.uploads.load(Ordering::SeqCst), 0);
    }
}
