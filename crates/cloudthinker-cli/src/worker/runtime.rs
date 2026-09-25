use std::collections::HashSet;
use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use chrono::Utc;
use cloudthinker_client::{CtError, CtResult, worker_api::WorkerClient, worker_types as api};
use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tokio_util::task::AbortOnDropHandle;
use uuid::Uuid;

use super::background;
use super::executor::WorkdirExecutor;
use super::journal::{Journal, JournalState};
use crate::engine::watch::{Poll, WatchConfig, watch};

const WORKER_TRANSPORT_RETRY_TIMEOUT: Duration = Duration::from_secs(60);
const WORKER_HEARTBEAT_RETRY_TIMEOUT: Duration = Duration::from_secs(30);
const WORKER_LEASE_SAFETY_MARGIN: Duration = Duration::from_secs(5);
const GC_EVERY_HEARTBEATS: u32 = 4;

pub async fn run(
    client: WorkerClient,
    executor: Arc<WorkdirExecutor>,
    registration: api::RegisterWorkerRequest,
    shutdown: CancellationToken,
    expected_target_id: Uuid,
) -> CtResult<()> {
    maintain(&executor).await;
    let worker = client.register(&registration).await?;
    validate_registration_target(expected_target_id, worker.target_id)?;
    client.conformance(worker.worker_id).await?;
    let mut tasks = JoinSet::new();
    let permits = Arc::new(Semaphore::new(16));
    let heartbeat_client = client.clone();
    let heartbeat_shutdown = shutdown.clone();
    let heartbeat_executor = executor.clone();
    let mut heartbeat = AbortOnDropHandle::new(tokio::spawn(async move {
        let mut ticks: u32 = 0;
        loop {
            tokio::select! {
                _=heartbeat_shutdown.cancelled()=>return Ok(()),
                _=tokio::time::sleep(Duration::from_secs(15))=>{
                    let retry_client = heartbeat_client.clone();
                    let result = retry_heartbeat(move || {
                        let client = retry_client.clone();
                        async move { client.worker_heartbeat(worker.worker_id, false).await }
                    })
                    .await;
                    result?;
                    ticks += 1;
                    if ticks.is_multiple_of(GC_EVERY_HEARTBEATS) {
                        maintain(&heartbeat_executor).await;
                    }
                }
            }
        }
    }));
    let mut seen = HashSet::new();
    let mut outcome = async { loop {
        if shutdown.is_cancelled() {break Ok(());}
        tokio::select! {
            _=shutdown.cancelled()=>break Ok(()),
            completed=tasks.join_next(), if !tasks.is_empty()=>{
                match completed {
                    Some(Ok((id,Ok(()))))=>{seen.remove(&id);},
                    Some(Ok((id,Err(error))))=>{seen.remove(&id); if matches!(error, CtError::Auth(_) | CtError::Store(_)) {break Err(error);} crate::engine::output::warn("An assignment stopped; check its status in CloudThinker.");},
                    _=>break Err(CtError::Protocol("worker assignment task stopped".into())),
                }
            }
            pending=retry_transport({
                let retry_client = client.clone();
                move || {
                    let client = retry_client.clone();
                    async move { client.pending(worker.worker_id).await }
                }
            }), if tasks.len()<registration.max_assignments.get() as usize=>{
                let pending=match pending {
                    Ok(pending)=>pending,
                    Err(error)=>break Err(error),
                };
                for assignment in pending {
                    if tasks.len()>=registration.max_assignments.get() as usize {break;}
                    if seen.contains(&assignment.assignment_id) {continue;}
                    match client.claim(worker.worker_id,assignment.assignment_id).await {
                        Ok(lease)=>{
                            seen.insert(lease.assignment_id);
                            let client=client.clone();let executor=executor.clone();let shutdown=shutdown.child_token();let permits=permits.clone();
                            tasks.spawn(async move {(lease.assignment_id,serve(client,executor,lease,shutdown,permits).await)});
                        }
                        Err(CtError::Api {status:409,..})=>{},
                        Err(error) if error.is_transport()=>crate::engine::output::warn("Could not claim an assignment; it will be retried."),
                        Err(error)=>{shutdown.cancel();return Err(error);}
                    }
                }
            }
            result=&mut heartbeat=>break if shutdown.is_cancelled() {Ok(())} else {result.unwrap_or_else(|_|Err(CtError::Protocol("worker heartbeat task stopped".into())))},
        }
    } }.await;
    let _ = client.worker_heartbeat(worker.worker_id, true).await;
    shutdown.cancel();
    heartbeat.abort();
    while let Some(result) = tasks.join_next().await {
        if outcome.is_ok() {
            outcome = result
                .map_err(|_| CtError::Protocol("worker drain interrupted".into()))
                .and_then(|(_, result)| result);
        }
    }
    outcome
}

async fn maintain(executor: &Arc<WorkdirExecutor>) {
    let executor = executor.clone();
    let _ = tokio::task::spawn_blocking(move || {
        background::gc(&executor.identity, SystemTime::now());
    })
    .await;
}

fn validate_registration_target(expected: Uuid, actual: Uuid) -> CtResult<()> {
    if expected == actual {
        Ok(())
    } else {
        Err(CtError::Auth("worker credential target mismatch".into()))
    }
}

async fn serve(
    client: WorkerClient,
    executor: Arc<WorkdirExecutor>,
    lease: api::AssignmentLease,
    drain: CancellationToken,
    permits: Arc<Semaphore>,
) -> CtResult<()> {
    let Some(lease) = announce(
        &client,
        lease,
        &drain,
        api::HeartbeatAssignmentRequestState::Starting,
    )
    .await?
    else {
        return Ok(());
    };
    let Some(lease) = announce(
        &client,
        lease,
        &drain,
        api::HeartbeatAssignmentRequestState::Active,
    )
    .await?
    else {
        return Ok(());
    };
    let cancel = CancellationToken::new();
    let _cancel_on_drop = cancel.clone().drop_guard();
    let journal = Journal::open(
        executor
            .identity
            .state
            .join(lease.assignment_id.to_string()),
    )
    .await?;
    if !reconcile(&client, &journal, &lease, &drain).await? {
        return Ok(());
    }
    let Some(lease) = announce(
        &client,
        lease,
        &drain,
        api::HeartbeatAssignmentRequestState::Active,
    )
    .await?
    else {
        return Ok(());
    };
    let renew_client = client.clone();
    let initial_lease = lease.clone();
    let renew_cancel = cancel.clone();
    let heartbeat = AbortOnDropHandle::new(tokio::spawn(async move {
        let mut renew_lease = initial_lease;
        loop {
            let renewal_wait = renewal_wait(&renew_lease);
            tokio::select! {
                _=renew_cancel.cancelled()=>return,
                _=tokio::time::sleep(renewal_wait)=>{
                    let retry_client = renew_client.clone();
                    let retry_lease = renew_lease.clone();
                    let result = tokio::select! {
                        _=renew_cancel.cancelled()=>return,
                        result=retry_transport_until(lease_retry_deadline(&renew_lease), move || {
                        let client = retry_client.clone();
                        let lease = retry_lease.clone();
                        async move {
                            client
                                .heartbeat(&lease,api::HeartbeatAssignmentRequestState::Active)
                                .await
                        }
                        })=>result,
                    };
                    match result {
                        Ok(lease) => renew_lease = lease,
                        Err(_) => {renew_cancel.cancel();return;}
                    }
                }
            }
        }
    }));
    let mut operations = JoinSet::new();
    let mut active = HashSet::new();
    let mut after = 0;
    let mut last_operation = std::time::Instant::now();
    let mut outcome=async { loop {
        if cancel.is_cancelled() {break Err(CtError::Protocol("assignment lease lost; effects stopped".into()));}
        if drain.is_cancelled() && operations.is_empty() {break Ok(());}
        if operations.is_empty() && last_operation.elapsed()>Duration::from_secs(30) {break Ok(());}
        let available = 4 - operations.len();
        tokio::select! {
            _=cancel.cancelled()=>break Err(CtError::Protocol("assignment lease lost; effects stopped".into())),
            completed=operations.join_next(), if !operations.is_empty()=>{
                match completed {
                    Some(Ok((id,Ok(()))))=>{active.remove(&id);},
                    Some(Ok((_,Err(error))))=>break Err(error),
                    _=>break Err(CtError::Protocol("operation task stopped".into())),
                }
            }
            batch=retry_transport({
                let retry_client = client.clone();
                let retry_lease = lease.clone();
                let retry_after = after;
                let retry_batch_size = available as u64;
                move || {
                    let client = retry_client.clone();
                    let lease = retry_lease.clone();
                    async move {
                        client
                            .operations(&lease, retry_after, retry_batch_size)
                            .await
                    }
                }
            }), if !drain.is_cancelled() && operations.len()<4=>{
                let batch=match batch {Ok(batch)=>batch,Err(error)=>break Err(error)};
                if batch.len() > available { break Err(CtError::Protocol("operation batch exceeds available capacity".into())); }
                for envelope in batch {
                    validate(&lease,&envelope)?;
                    if active.contains(&envelope.operation_id) {continue;}
                    let received = envelope.clone();
                    let state=journal.apply(move |j| j.received(&received)).await?;
                    after=after.max(envelope.operation_sequence as u64);
                    if state==JournalState::Acknowledged {continue;}
                    last_operation=std::time::Instant::now();
                    active.insert(envelope.operation_id);
                    let client=client.clone();let executor=executor.clone();let journal=journal.clone();let cancel=cancel.child_token();let operation_drain=drain.clone();let permits=permits.clone();
                    operations.spawn(async move {
                        let permit = tokio::select! {
                            _=cancel.cancelled()=>return (envelope.operation_id,Err(CtError::Protocol("assignment cancelled before dispatch".into()))),
                            permit=permits.acquire_owned()=>permit,
                        };
                        let _permit=match permit {Ok(permit)=>permit,Err(_)=>return (envelope.operation_id,Err(CtError::Protocol("worker operation capacity unavailable".into())))};
                        (envelope.operation_id,execute(client,executor,journal,envelope,lease.worker_id,(cancel,operation_drain),state).await)});
                }
                journal.apply(move |j| j.advance_cursor(j.cursor().max(after))).await?;
            }
            _=tokio::time::sleep(Duration::from_secs(30).saturating_sub(last_operation.elapsed())), if operations.is_empty()=>break Ok(()),
        }
    } }.await;
    if outcome.is_err() {
        cancel.cancel();
    }
    while let Some(result) = operations.join_next().await {
        if outcome.is_ok() {
            outcome = result
                .map_err(|_| CtError::Protocol("operation drain interrupted".into()))
                .and_then(|(_, result)| result);
            if outcome.is_err() {
                cancel.cancel();
            }
        }
    }
    cancel.cancel();
    heartbeat.abort();
    let release = client.release(&lease).await;
    if outcome.is_ok() && release.is_ok() {
        journal
            .retire(
                executor
                    .identity
                    .state
                    .join(lease.assignment_id.to_string()),
            )
            .await?;
    }
    outcome.and(release)
}

async fn announce(
    client: &WorkerClient,
    lease: api::AssignmentLease,
    drain: &CancellationToken,
    state: api::HeartbeatAssignmentRequestState,
) -> CtResult<Option<api::AssignmentLease>> {
    let heartbeat_client = client.clone();
    let heartbeat_lease = lease.clone();
    tokio::select! {
        _=drain.cancelled()=>client.release(&lease).await.map(|()| None),
        result=retry_transport_until(lease_retry_deadline(&lease), move || {
            let client = heartbeat_client.clone();
            let lease = heartbeat_lease.clone();
            async move { client.heartbeat(&lease, state).await }
        })=>result.map(Some),
    }
}

async fn reconcile(
    client: &WorkerClient,
    journal: &Journal,
    lease: &api::AssignmentLease,
    drain: &CancellationToken,
) -> CtResult<bool> {
    for record in journal.apply(|j| Ok(j.records())).await? {
        if record.state == JournalState::Acknowledged {
            continue;
        }
        let receipt_client = client.clone();
        let worker_id = lease.worker_id;
        let assignment_id = record.assignment_id;
        let operation_id = record.operation_id;
        let receipt = tokio::select! {
            _=drain.cancelled()=>return client.release(lease).await.map(|()| false),
            result=retry_transport_until(lease_retry_deadline(lease), move || {
                let client = receipt_client.clone();
                async move { client.receipt(worker_id, assignment_id, operation_id).await }
            })=>result?,
        };
        if receipt.request_digest != record.digest || receipt.session_id != record.session_id {
            return Err(CtError::Protocol(
                "worker recovery identity mismatch".into(),
            ));
        }
        if matches!(
            receipt.state,
            api::OperationReceiptState::Succeeded
                | api::OperationReceiptState::Failed
                | api::OperationReceiptState::Cancelled
        ) {
            journal
                .apply(move |j| j.acknowledge(record.operation_id))
                .await?;
        }
    }
    Ok(true)
}

async fn execute(
    client: WorkerClient,
    executor: Arc<WorkdirExecutor>,
    journal: Journal,
    envelope: api::OperationEnvelope,
    worker: Uuid,
    tokens: (CancellationToken, CancellationToken),
    state: JournalState,
) -> CtResult<()> {
    let (cancel, drain) = tokens;
    let operation_id = envelope.operation_id;
    let result = if state == JournalState::Terminal {
        let record = journal
            .apply(move |j| {
                j.records()
                    .into_iter()
                    .find(|r| r.operation_id == operation_id)
                    .ok_or_else(|| CtError::Store("worker receipt journal unavailable".into()))
            })
            .await?;
        if record.fence != envelope.fence_token {
            return Err(CtError::Protocol(
                "terminal receipt belongs to an earlier fence; reconciliation required".into(),
            ));
        }
        journal.apply(move |j| j.result(operation_id)).await?
    } else {
        if state != JournalState::Received {
            return Err(CtError::Protocol(
                "operation may already have executed; reconciliation required".into(),
            ));
        }
        journal.apply(move |j| j.started(operation_id)).await?;
        client.start(worker, &envelope).await?;
        if cancel.is_cancelled() {
            return Err(CtError::Protocol(
                "assignment cancelled before dispatch".into(),
            ));
        }
        let result = executor.execute(&envelope, cancel, worker, &client).await?;
        let durable_result = result.clone();
        journal
            .apply(move |j| j.terminal(operation_id, &durable_result))
            .await?;
        result
    };
    for attempt in 0..5 {
        match client.complete(worker, &envelope, result.clone()).await {
            Ok(receipt) => {
                if receipt.operation_id != envelope.operation_id
                    || receipt.request_digest != envelope.request_digest
                {
                    return Err(CtError::Protocol("receipt identity mismatch".into()));
                }
                journal.apply(move |j| j.acknowledge(operation_id)).await?;
                if receipt.assignment_complete {
                    drain.cancel();
                }
                return Ok(());
            }
            Err(error) if error.is_transport() && attempt < 4 => {
                tokio::time::sleep(Duration::from_secs(1)).await
            }
            Err(error) => return Err(error),
        }
    }
    Err(CtError::Transport(
        "worker result acknowledgement unavailable".into(),
    ))
}

fn validate(lease: &api::AssignmentLease, envelope: &api::OperationEnvelope) -> CtResult<()> {
    if envelope.protocol_version != 1
        || envelope.assignment_id != lease.assignment_id
        || envelope.session_id != lease.session_id
        || envelope.target_id != lease.target_id
        || envelope.fence_token != lease.fence_token
        || envelope.lease_token != lease.lease_token
        || envelope.operation_sequence < 1
        || envelope.deadline_at <= Utc::now()
    {
        return Err(CtError::Protocol(
            "worker envelope failed lease validation".into(),
        ));
    }
    Ok(())
}

async fn retry_heartbeat<T, F, Fut>(fetch: F) -> CtResult<T>
where
    F: Fn() -> Fut + Send + Sync + 'static,
    Fut: Future<Output = CtResult<T>> + Send + 'static,
    T: Send + 'static,
{
    let config = WatchConfig::for_run(WORKER_TRANSPORT_RETRY_TIMEOUT);
    retry_within(
        fetch,
        &config,
        WORKER_HEARTBEAT_RETRY_TIMEOUT,
        "worker heartbeat retry deadline exceeded",
    )
    .await
}

fn lease_retry_deadline(lease: &api::AssignmentLease) -> Instant {
    let remaining = lease
        .lease_expires_at
        .signed_duration_since(Utc::now())
        .to_std()
        .unwrap_or_default();
    Instant::now() + remaining.saturating_sub(WORKER_LEASE_SAFETY_MARGIN)
}

fn renewal_wait(lease: &api::AssignmentLease) -> Duration {
    Duration::from_secs(15)
        .min(lease_retry_deadline(lease).saturating_duration_since(Instant::now()))
}

async fn retry_transport_until<T, F, Fut>(deadline: Instant, fetch: F) -> CtResult<T>
where
    F: Fn() -> Fut + Send + Sync + 'static,
    Fut: Future<Output = CtResult<T>> + Send + 'static,
    T: Send + 'static,
{
    let remaining = deadline.saturating_duration_since(Instant::now());
    let config = WatchConfig::for_run(WORKER_TRANSPORT_RETRY_TIMEOUT);
    retry_within(
        fetch,
        &config,
        remaining,
        "worker lease retry deadline exceeded",
    )
    .await
}

async fn retry_transport<T, F, Fut>(fetch: F) -> CtResult<T>
where
    F: Fn() -> Fut + Send + Sync + 'static,
    Fut: Future<Output = CtResult<T>> + Send + 'static,
    T: Send + 'static,
{
    let config = WatchConfig::for_run(WORKER_TRANSPORT_RETRY_TIMEOUT);
    retry_within(
        fetch,
        &config,
        WORKER_TRANSPORT_RETRY_TIMEOUT,
        "worker transport retry deadline exceeded",
    )
    .await
}

async fn retry_within<T, F, Fut>(
    fetch: F,
    config: &WatchConfig,
    timeout: Duration,
    expired: &'static str,
) -> CtResult<T>
where
    F: Fn() -> Fut + Send + Sync,
    Fut: Future<Output = CtResult<T>> + Send,
    T: Send,
{
    if timeout.is_zero() {
        return Err(CtError::Transport(expired.into()));
    }
    tokio::time::timeout(
        timeout,
        watch(|| async { fetch().await.map(Poll::Terminal) }, config),
    )
    .await
    .map_err(|_| CtError::Transport(expired.into()))?
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tokio::sync::oneshot;

    use super::*;

    #[test]
    fn registration_target_must_match_the_saved_credential() {
        let target = Uuid::from_u128(1);
        assert!(validate_registration_target(target, target).is_ok());
        assert!(matches!(
            validate_registration_target(target, Uuid::from_u128(2)),
            Err(CtError::Auth(message)) if message == "worker credential target mismatch"
        ));
    }

    #[tokio::test]
    async fn retries_transport_error_before_returning_success() {
        let attempts = AtomicUsize::new(0);
        let config = WatchConfig {
            base_delay: Duration::from_millis(1),
            max_delay: Duration::from_millis(2),
            factor: 1.5,
            jitter: 0.0,
            max_transport_errors: 2,
            overall_timeout: Duration::from_secs(1),
        };
        let result: CtResult<u8> = retry_within(
            || async {
                if attempts.fetch_add(1, Ordering::SeqCst) < 2 {
                    Err(CtError::Transport("temporary disconnect".into()))
                } else {
                    Ok(42)
                }
            },
            &config,
            Duration::from_secs(1),
            "expired",
        )
        .await;

        assert_eq!(result.unwrap(), 42);
        assert_eq!(attempts.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn retry_transport_returns_non_transport_error_without_retrying() {
        let attempts = std::sync::Arc::new(AtomicUsize::new(0));
        let result = retry_transport({
            let attempts = attempts.clone();
            move || {
                let attempts = attempts.clone();
                async move {
                    attempts.fetch_add(1, Ordering::SeqCst);
                    Err::<(), _>(CtError::Auth("revoked".into()))
                }
            }
        })
        .await;

        assert!(matches!(result, Err(CtError::Auth(_))));
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn heartbeat_retry_deadline_returns_transport_error() {
        let attempts = std::sync::Arc::new(AtomicUsize::new(0));
        let config = WatchConfig {
            base_delay: Duration::from_millis(1),
            max_delay: Duration::from_millis(2),
            factor: 1.5,
            jitter: 0.0,
            max_transport_errors: 5,
            overall_timeout: Duration::from_secs(1),
        };
        let result = retry_within(
            {
                let attempts = attempts.clone();
                move || {
                    let attempts = attempts.clone();
                    async move {
                        attempts.fetch_add(1, Ordering::SeqCst);
                        Err::<(), _>(CtError::Transport("temporary disconnect".into()))
                    }
                }
            },
            &config,
            Duration::from_millis(5),
            "worker heartbeat retry deadline exceeded",
        )
        .await;

        assert!(matches!(result, Err(CtError::Transport(_))));
        assert!(attempts.load(Ordering::SeqCst) >= 1);
    }

    #[tokio::test]
    async fn heartbeat_retries_a_dropped_http_connection() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let attempts = Arc::new(AtomicUsize::new(0));
        let server_attempts = attempts.clone();
        let (stop, mut stopped) = oneshot::channel();
        let target_id = Uuid::new_v4();
        let worker_id = Uuid::new_v4();
        let server = tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = &mut stopped => break,
                    accepted = listener.accept() => {
                        let Ok((mut stream, _)) = accepted else { break; };
                        let mut request = [0_u8; 8192];
                        if stream.read(&mut request).await.is_err() { continue; }
                        let attempt = server_attempts.fetch_add(1, Ordering::SeqCst) + 1;
                        if attempt < 3 { continue; }
                        let body = format!(
                            r#"{{"max_assignments":2,"state":"online","target_id":"{target_id}","worker_id":"{worker_id}"}}"#
                        );
                        let response = format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                            body.len(), body
                        );
                        let _ = stream.write_all(response.as_bytes()).await;
                    }
                }
            }
        });
        let client = WorkerClient::new(&format!("http://{address}"), "test-token").unwrap();
        let config = WatchConfig {
            base_delay: Duration::from_millis(1),
            max_delay: Duration::from_millis(2),
            factor: 1.5,
            jitter: 0.0,
            max_transport_errors: 2,
            overall_timeout: Duration::from_secs(1),
        };
        let result = retry_within(
            {
                let client = client.clone();
                move || {
                    let client = client.clone();
                    async move { client.worker_heartbeat(worker_id, false).await }
                }
            },
            &config,
            Duration::from_secs(1),
            "worker heartbeat retry deadline exceeded",
        )
        .await;

        assert_eq!(result.unwrap().worker_id, worker_id);
        assert_eq!(attempts.load(Ordering::SeqCst), 3);
        let _ = stop.send(());
        server.await.unwrap();
    }

    #[tokio::test]
    async fn heartbeat_retry_stops_before_a_delayed_response_outlives_the_lease() {
        let lease = api::AssignmentLease {
            assignment_id: Uuid::new_v4(),
            fence_token: 1,
            lease_expires_at: Utc::now() + chrono::Duration::milliseconds(200),
            lease_token: "lease".into(),
            session_id: Uuid::new_v4(),
            state: api::AssignmentState::Active,
            target_id: Uuid::new_v4(),
            worker_id: Uuid::new_v4(),
        };
        let deadline = lease_retry_deadline(&lease);
        let remaining = deadline.saturating_duration_since(Instant::now());
        assert!(remaining < Duration::from_millis(200));

        let started = Instant::now();
        let result = retry_transport_until(deadline, || async {
            tokio::time::sleep(Duration::from_millis(500)).await;
            Ok::<(), CtError>(())
        })
        .await;

        assert!(matches!(result, Err(CtError::Transport(_))));
        assert!(started.elapsed() < Duration::from_millis(350));
    }

    #[test]
    fn renewal_wait_is_bounded_by_a_short_lease() {
        let lease = api::AssignmentLease {
            assignment_id: Uuid::new_v4(),
            fence_token: 1,
            lease_expires_at: Utc::now() + chrono::Duration::seconds(6),
            lease_token: "lease".into(),
            session_id: Uuid::new_v4(),
            state: api::AssignmentState::Active,
            target_id: Uuid::new_v4(),
            worker_id: Uuid::new_v4(),
        };

        assert!(renewal_wait(&lease) < Duration::from_secs(2));
    }

    #[tokio::test]
    async fn transport_retry_deadline_includes_a_slow_fetch() {
        let config = WatchConfig {
            base_delay: Duration::from_millis(1),
            max_delay: Duration::from_millis(2),
            factor: 1.5,
            jitter: 0.0,
            max_transport_errors: 5,
            overall_timeout: Duration::from_secs(1),
        };
        let started = Instant::now();
        let result = retry_within(
            || async {
                tokio::time::sleep(Duration::from_millis(50)).await;
                Ok::<(), CtError>(())
            },
            &config,
            Duration::from_millis(5),
            "worker transport retry deadline exceeded",
        )
        .await;

        assert!(matches!(result, Err(CtError::Transport(_))));
        assert!(started.elapsed() < Duration::from_millis(40));
    }
}
