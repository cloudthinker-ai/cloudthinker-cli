use std::future::Future;
use std::time::Duration;

use uuid::Uuid;

use crate::{CtClient, CtError, CtResult, CyberRunResult};

const RUN_STATUS_POLL_INTERVAL: Duration = Duration::from_secs(2);

pub async fn while_run_running<T, F>(client: &CtClient, run_id: Uuid, operation: F) -> CtResult<T>
where
    F: Future<Output = CtResult<T>>,
{
    while_run_running_at_interval(client, run_id, operation, RUN_STATUS_POLL_INTERVAL).await
}

async fn while_run_running_at_interval<T, F>(
    client: &CtClient,
    run_id: Uuid,
    operation: F,
    interval: Duration,
) -> CtResult<T>
where
    F: Future<Output = CtResult<T>>,
{
    ensure_run_running(client, run_id).await?;
    tokio::pin!(operation);

    loop {
        tokio::select! {
            result = &mut operation => {
                return match result {
                    Ok(value) => {
                        ensure_run_running(client, run_id).await?;
                        Ok(value)
                    }
                    Err(error) => Err(error),
                };
            }
            _ = tokio::time::sleep(interval) => ensure_run_running(client, run_id).await?,
        }
    }
}

async fn ensure_run_running(client: &CtClient, run_id: Uuid) -> CtResult<()> {
    let run = client.cyber_get_run(run_id).await?;
    if run.result == CyberRunResult::Running {
        Ok(())
    } else {
        Err(CtError::Protocol(format!(
            "local Cyber run is no longer running ({:?})",
            run.result
        )))
    }
}

pub async fn wait_for_run_stop(client: &CtClient, run_id: Uuid) -> CtError {
    loop {
        match client.cyber_get_run(run_id).await {
            Ok(run) if run.result == CyberRunResult::Running => {}
            Ok(run) => {
                return CtError::Protocol(format!(
                    "local Cyber run is no longer running ({:?})",
                    run.result
                ));
            }
            Err(error) => return error,
        }
        tokio::time::sleep(RUN_STATUS_POLL_INTERVAL).await;
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use serde_json::json;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;
    use crate::test_support::{MockTokenStore, stored};

    const RUN_ID: &str = "11111111-1111-4111-8111-111111111111";

    fn run_body(result: &str) -> serde_json::Value {
        json!({
            "id": RUN_ID,
            "app_id": "22222222-2222-4222-8222-222222222222",
            "conversation_id": "33333333-3333-4333-8333-333333333333",
            "frameworks": ["owasp_api"],
            "mode": "white",
            "intensity": "full",
            "run_scope": {"include": [], "exclude": []},
            "scan_mode": "full",
            "selection": {"option_id": "default", "thinking_effort": null},
            "result": result,
            "auth_status": "not_required",
            "counts": {},
            "surface_delta": {},
            "started_at": "2026-09-27T00:00:00Z",
            "finished_at": null,
            "created_at": "2026-09-27T00:00:00Z",
            "execution_host": "local"
        })
    }

    async fn client(server: &MockServer) -> CtClient {
        CtClient::new(
            server.uri(),
            Arc::new(MockTokenStore::new(Some(stored("access", "refresh")))),
        )
        .unwrap()
    }

    #[tokio::test]
    async fn stopped_run_drops_operation_before_it_completes() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(format!("/api/v1/appsec/runs/{RUN_ID}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(run_body("cancelled")))
            .mount(&server)
            .await;
        let client = client(&server).await;
        let completed = Arc::new(AtomicBool::new(false));
        let operation_completed = completed.clone();
        let operation = async move {
            tokio::time::sleep(Duration::from_secs(10)).await;
            operation_completed.store(true, Ordering::SeqCst);
            Ok::<_, CtError>(())
        };

        let error = while_run_running_at_interval(
            &client,
            Uuid::parse_str(RUN_ID).unwrap(),
            operation,
            Duration::from_millis(5),
        )
        .await
        .unwrap_err();

        assert!(matches!(error, CtError::Protocol(_)));
        assert!(!completed.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn run_status_read_failure_drops_operation() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(format!("/api/v1/appsec/runs/{RUN_ID}")))
            .respond_with(ResponseTemplate::new(503))
            .mount(&server)
            .await;
        let client = client(&server).await;
        let completed = Arc::new(AtomicBool::new(false));
        let operation_completed = completed.clone();
        let operation = async move {
            tokio::time::sleep(Duration::from_secs(10)).await;
            operation_completed.store(true, Ordering::SeqCst);
            Ok::<_, CtError>(())
        };

        let error = while_run_running_at_interval(
            &client,
            Uuid::parse_str(RUN_ID).unwrap(),
            operation,
            Duration::from_millis(5),
        )
        .await
        .unwrap_err();

        assert!(matches!(error, CtError::Api { status: 503, .. }));
        assert!(!completed.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn run_stop_watcher_returns_terminal_status_without_retrying() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(format!("/api/v1/appsec/runs/{RUN_ID}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(run_body("failed")))
            .mount(&server)
            .await;
        let client = client(&server).await;

        let error = wait_for_run_stop(&client, Uuid::parse_str(RUN_ID).unwrap()).await;

        assert!(matches!(error, CtError::Protocol(_)));
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn successful_operation_checks_run_status_before_returning() {
        let server = MockServer::start().await;
        let status_reads = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let responder_reads = status_reads.clone();
        Mock::given(method("GET"))
            .and(path(format!("/api/v1/appsec/runs/{RUN_ID}")))
            .respond_with(move |_: &wiremock::Request| {
                let state = if responder_reads.fetch_add(1, Ordering::SeqCst) == 0 {
                    "running"
                } else {
                    "cancelled"
                };
                ResponseTemplate::new(200).set_body_json(run_body(state))
            })
            .mount(&server)
            .await;
        let client = client(&server).await;

        let error = while_run_running_at_interval(
            &client,
            Uuid::parse_str(RUN_ID).unwrap(),
            async { Ok::<_, CtError>("collected") },
            Duration::from_secs(1),
        )
        .await
        .unwrap_err();

        assert!(matches!(error, CtError::Protocol(_)));
    }

    #[tokio::test]
    async fn terminal_run_stops_the_remaining_probe_rows() {
        let server = MockServer::start().await;
        let status_reads = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let responder_reads = status_reads.clone();
        Mock::given(method("GET"))
            .and(path(format!("/api/v1/appsec/runs/{RUN_ID}")))
            .respond_with(move |_: &wiremock::Request| {
                let state = if responder_reads.fetch_add(1, Ordering::SeqCst) == 0 {
                    "running"
                } else {
                    "cancelled"
                };
                ResponseTemplate::new(200).set_body_json(run_body(state))
            })
            .mount(&server)
            .await;
        let client = client(&server).await;
        let probes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let operation_probes = probes.clone();
        let operation = async move {
            for _ in 0..3 {
                operation_probes.fetch_add(1, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            Ok::<_, CtError>(())
        };

        let error = while_run_running_at_interval(
            &client,
            Uuid::parse_str(RUN_ID).unwrap(),
            operation,
            Duration::from_millis(5),
        )
        .await
        .unwrap_err();

        assert!(matches!(error, CtError::Protocol(_)));
        assert_eq!(probes.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn cancellation_during_discovery_prevents_upload() {
        let server = MockServer::start().await;
        let status_reads = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let responder_reads = status_reads.clone();
        Mock::given(method("GET"))
            .and(path(format!("/api/v1/appsec/runs/{RUN_ID}")))
            .respond_with(move |_: &wiremock::Request| {
                let state = if responder_reads.fetch_add(1, Ordering::SeqCst) == 0 {
                    "running"
                } else {
                    "cancelled"
                };
                ResponseTemplate::new(200).set_body_json(run_body(state))
            })
            .mount(&server)
            .await;
        let client = client(&server).await;
        let uploaded = Arc::new(AtomicBool::new(false));

        let collected = while_run_running_at_interval(
            &client,
            Uuid::parse_str(RUN_ID).unwrap(),
            async {
                tokio::time::sleep(Duration::from_millis(40)).await;
                Ok::<_, CtError>("artifacts")
            },
            Duration::from_millis(5),
        )
        .await;
        if collected.is_ok() {
            uploaded.store(true, Ordering::SeqCst);
        }

        assert!(matches!(collected, Err(CtError::Protocol(_))));
        assert!(!uploaded.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn backend_terminal_status_cancels_a_controlled_child() {
        let server = MockServer::start().await;
        let status_reads = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let responder_reads = status_reads.clone();
        Mock::given(method("GET"))
            .and(path(format!("/api/v1/appsec/runs/{RUN_ID}")))
            .respond_with(move |_: &wiremock::Request| {
                let state = if responder_reads.fetch_add(1, Ordering::SeqCst) == 0 {
                    "running"
                } else {
                    "cancelled"
                };
                ResponseTemplate::new(200).set_body_json(run_body(state))
            })
            .mount(&server)
            .await;
        let client = client(&server).await;
        let mut child = tokio::process::Command::new("sleep")
            .arg("30")
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let child_pid = child.id().unwrap().to_string();
        let operation = async move {
            child
                .wait()
                .await
                .map(|_| ())
                .map_err(|error| CtError::Transport(error.to_string()))
        };

        let error = while_run_running_at_interval(
            &client,
            Uuid::parse_str(RUN_ID).unwrap(),
            operation,
            Duration::from_millis(5),
        )
        .await
        .unwrap_err();

        assert!(matches!(error, CtError::Protocol(_)));
        let status = std::process::Command::new("ps")
            .args(["-o", "stat=", "-p", &child_pid])
            .output()
            .unwrap();
        let state = String::from_utf8_lossy(&status.stdout);
        let state = state.trim();
        assert!(
            state.is_empty() || state.starts_with('Z'),
            "cancelled child is still running with process state {state:?}"
        );
    }
}
