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
use super::journal::{self, Delivery, Journal, JournalState};
use crate::engine::output;
use crate::engine::watch::{Poll, WatchConfig, watch};

const WORKER_TRANSPORT_RETRY_TIMEOUT: Duration = Duration::from_secs(60);
const WORKER_HEARTBEAT_RETRY_TIMEOUT: Duration = Duration::from_secs(30);
const WORKER_LEASE_SAFETY_MARGIN: Duration = Duration::from_secs(5);
const GC_EVERY_HEARTBEATS: u32 = 4;
const RECONNECT_MIN_DELAY: Duration = Duration::from_secs(1);
const RECONNECT_MAX_DELAY: Duration = Duration::from_secs(60);
const RECONNECT_STABLE_AFTER: Duration = Duration::from_secs(300);
const ABANDONED_JOURNAL_AGE: Duration = Duration::from_secs(7 * 24 * 60 * 60);

#[derive(Clone, Default)]
pub struct Signals {
    pub drain: CancellationToken,
    pub abort: CancellationToken,
}

pub async fn run(
    client: WorkerClient,
    executor: Arc<WorkdirExecutor>,
    registration: api::RegisterWorkerRequest,
    signals: Signals,
    expected_target_id: Uuid,
) -> CtResult<()> {
    let verified = std::sync::atomic::AtomicBool::new(false);
    reconnect(&signals.drain, || {
        session(
            &client,
            &executor,
            &registration,
            &signals,
            expected_target_id,
            &verified,
        )
    })
    .await
}

async fn reconnect<F, Fut>(drain: &CancellationToken, mut connect: F) -> CtResult<()>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = CtResult<()>>,
{
    let mut delay = RECONNECT_MIN_DELAY;
    loop {
        let started = Instant::now();
        let error = match connect().await {
            Ok(()) => return Ok(()),
            Err(error) => error,
        };
        if is_fatal(&error) {
            return Err(error);
        }
        if drain.is_cancelled() {
            return Ok(());
        }
        if started.elapsed() >= RECONNECT_STABLE_AFTER {
            delay = RECONNECT_MIN_DELAY;
        }
        output::worker_event(&format!(
            "Lost the connection to CloudThinker ({error}); reconnecting in {}s.",
            delay.as_secs()
        ));
        tokio::select! {
            _=drain.cancelled()=>return Ok(()),
            _=tokio::time::sleep(delay)=>{},
        }
        delay = (delay * 2).min(RECONNECT_MAX_DELAY);
    }
}

fn is_fatal(error: &CtError) -> bool {
    match error {
        CtError::Api { status, .. } => (400..500).contains(status) && *status != 409,
        error => stops_worker(error),
    }
}

fn stops_worker(error: &CtError) -> bool {
    matches!(
        error,
        CtError::Auth(_)
            | CtError::ObsoleteCredentials { .. }
            | CtError::Usage(_)
            | CtError::Store(_)
    )
}

async fn session(
    client: &WorkerClient,
    executor: &Arc<WorkdirExecutor>,
    registration: &api::RegisterWorkerRequest,
    signals: &Signals,
    expected_target_id: Uuid,
    verified: &std::sync::atomic::AtomicBool,
) -> CtResult<()> {
    let shutdown = signals.drain.clone();
    maintain(executor).await;
    let Some(worker) = until_drained(
        &shutdown,
        retry_transport({
            let client = client.clone();
            let registration = registration.clone();
            move || {
                let client = client.clone();
                let registration = registration.clone();
                async move { client.register(&registration).await }
            }
        }),
    )
    .await?
    else {
        return Ok(());
    };
    validate_registration_target(expected_target_id, worker.target_id)?;
    if !verified.load(std::sync::atomic::Ordering::SeqCst) {
        if until_drained(&shutdown, client.conformance(worker.worker_id))
            .await?
            .is_none()
        {
            return Ok(());
        }
        verified.store(true, std::sync::atomic::Ordering::SeqCst);
    }
    if until_drained(
        &shutdown,
        retry_heartbeat({
            let client = client.clone();
            move || {
                let client = client.clone();
                async move { client.worker_heartbeat(worker.worker_id, false).await }
            }
        }),
    )
    .await?
    .is_none()
    {
        return Ok(());
    }
    output::worker_event(&format!("Worker {} is online.", worker.worker_id));
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
    let stop = CancellationToken::new();
    let mut seen = HashSet::new();
    let outcome = async { 'serve: loop {
        if shutdown.is_cancelled() {break Ok(());}
        tokio::select! {
            _=shutdown.cancelled()=>break Ok(()),
            completed=tasks.join_next(), if !tasks.is_empty()=>{
                match completed {
                    Some(Ok((id,Ok(()))))=>{seen.remove(&id);output::worker_event(&format!("Assignment {id} finished."));},
                    Some(Ok((id,Err(error))))=>{seen.remove(&id); if stops_worker(&error) {break Err(error);} output::warn(&format!("Assignment {id} stopped: {error}. Check its status in CloudThinker."));},
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
                            output::worker_event(&format!("Assignment {} claimed for session {}.", lease.assignment_id, lease.session_id));
                            let client=client.clone();let executor=executor.clone();let tokens=(stop.child_token(),signals.abort.clone());let permits=permits.clone();
                            tasks.spawn(async move {(lease.assignment_id,serve(client,executor,lease,tokens,permits).await)});
                        }
                        Err(CtError::Api {status:409,detail})=>output::worker_debug(&format!("Assignment {} was not claimed: {}.", assignment.assignment_id, detail.unwrap_or_default())),
                        Err(error) if error.is_transport()=>output::warn(&format!("Could not claim assignment {} ({error}); it will be retried.", assignment.assignment_id)),
                        Err(error)=>break 'serve Err(error),
                    }
                }
            }
            result=&mut heartbeat=>break if shutdown.is_cancelled() {Ok(())} else {result.unwrap_or_else(|_|Err(CtError::Protocol("worker heartbeat task stopped".into())))},
        }
    } }.await;
    let draining = shutdown.is_cancelled();
    if draining {
        output::worker_event(&format!(
            "Draining {} running operations; press Ctrl-C again to cancel them.",
            executor.running()
        ));
        let _ = client.worker_heartbeat(worker.worker_id, true).await;
    }
    stop.cancel();
    heartbeat.abort();
    let mut outcome = outcome;
    while let Some(result) = tasks.join_next().await {
        if outcome.is_ok() {
            outcome = result
                .map_err(|_| CtError::Protocol("worker drain interrupted".into()))
                .and_then(|(_, result)| result);
        }
    }
    outcome
}

async fn until_drained<T>(
    drain: &CancellationToken,
    work: impl Future<Output = CtResult<T>>,
) -> CtResult<Option<T>> {
    tokio::select! {
        _=drain.cancelled()=>Ok(None),
        result=work=>result.map(Some),
    }
}

async fn maintain(executor: &Arc<WorkdirExecutor>) {
    let executor = executor.clone();
    let _ = tokio::task::spawn_blocking(move || {
        let now = SystemTime::now();
        background::gc(&executor.identity, now);
        journal::remove_abandoned(&executor.identity.state, now, ABANDONED_JOURNAL_AGE);
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
    tokens: (CancellationToken, CancellationToken),
    permits: Arc<Semaphore>,
) -> CtResult<()> {
    let (drain, abort) = tokens;
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
    let cancel = abort.child_token();
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
        if cancel.is_cancelled() {break stopped(&abort);}
        if drain.is_cancelled() && operations.is_empty() {break Ok(());}
        if operations.is_empty() && last_operation.elapsed()>Duration::from_secs(30) {break Ok(());}
        let available = 4 - operations.len();
        tokio::select! {
            _=cancel.cancelled()=>break stopped(&abort),
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
                    let delivery = Delivery::of(&envelope);
                    let state=journal.apply(move |j| j.received(delivery)).await?;
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
    settle(
        outcome,
        release,
        &journal,
        executor
            .identity
            .state
            .join(lease.assignment_id.to_string()),
    )
    .await
}

async fn settle(
    outcome: CtResult<()>,
    release: CtResult<()>,
    journal: &Journal,
    path: std::path::PathBuf,
) -> CtResult<()> {
    if outcome.is_err() || release.is_err() {
        return outcome.and(release);
    }
    journal.retire(path).await
}

fn stopped(abort: &CancellationToken) -> CtResult<()> {
    if abort.is_cancelled() {
        Ok(())
    } else {
        Err(CtError::Protocol(
            "assignment lease lost; effects stopped".into(),
        ))
    }
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
                | api::OperationReceiptState::Unknown
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
    let envelope = Arc::new(envelope);
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
        executor.identity.revalidate()?;
        journal.apply(move |j| j.started(operation_id)).await?;
        client.start(worker, &envelope).await?;
        let result = if cancel.is_cancelled() {
            super::executor::cancelled()?
        } else {
            executor.execute(&envelope, cancel, worker, &client).await?
        };
        let durable_result = result.clone();
        journal
            .apply(move |j| j.terminal(operation_id, &durable_result))
            .await?;
        result
    };
    let receipt = retry_transport({
        let client = client.clone();
        let envelope = envelope.clone();
        move || {
            let client = client.clone();
            let envelope = envelope.clone();
            let result = result.clone();
            async move { client.complete(worker, &envelope, result).await }
        }
    })
    .await?;
    if receipt.operation_id != envelope.operation_id
        || receipt.request_digest != envelope.request_digest
    {
        return Err(CtError::Protocol("receipt identity mismatch".into()));
    }
    journal.apply(move |j| j.acknowledge(operation_id)).await?;
    if receipt.assignment_complete {
        drain.cancel();
    }
    Ok(())
}

fn validate(lease: &api::AssignmentLease, envelope: &api::OperationEnvelope) -> CtResult<()> {
    if envelope.protocol_version != 1
        || envelope.assignment_id != lease.assignment_id
        || envelope.session_id != lease.session_id
        || envelope.target_id != lease.target_id
        || envelope.fence_token != lease.fence_token
        || envelope.lease_token != lease.lease_token
        || envelope.operation_sequence < 1
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
    retry_within(
        fetch,
        &worker_retry_config(),
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
    retry_within(
        fetch,
        &worker_retry_config(),
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
    retry_within(
        fetch,
        &worker_retry_config(),
        WORKER_TRANSPORT_RETRY_TIMEOUT,
        "worker transport retry deadline exceeded",
    )
    .await
}

fn worker_retry_config() -> WatchConfig {
    WatchConfig {
        max_transport_errors: u32::MAX,
        ..WatchConfig::for_run(WORKER_TRANSPORT_RETRY_TIMEOUT)
    }
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
    let deadline = Instant::now() + timeout;
    tokio::select! {
        biased;
        _ = tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)) => {
            Err(CtError::Transport(expired.into()))
        }
        result = watch(|| async { fetch().await.map(Poll::Terminal) }, config) => {
            if Instant::now() >= deadline {
                Err(CtError::Transport(expired.into()))
            } else {
                result
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tokio::sync::oneshot;

    use super::*;

    #[tokio::test(start_paused = true)]
    async fn a_lost_connection_reconnects_until_the_session_ends_cleanly() {
        let attempts = AtomicUsize::new(0);
        let drain = CancellationToken::new();

        let result = reconnect(&drain, || async {
            match attempts.fetch_add(1, Ordering::SeqCst) {
                0 => Err(CtError::Transport("connection reset".into())),
                1 => Err(CtError::Api {
                    status: 503,
                    detail: None,
                }),
                2 => Err(CtError::Protocol("worker heartbeat task stopped".into())),
                _ => Ok(()),
            }
        })
        .await;

        assert!(result.is_ok());
        assert_eq!(attempts.load(Ordering::SeqCst), 4);
    }

    #[tokio::test(start_paused = true)]
    async fn a_revoked_credential_or_replaced_folder_stops_the_worker() {
        for fatal in [
            CtError::Auth("worker credential expired or revoked".into()),
            CtError::Usage("WORKDIR_IDENTITY_CHANGED".into()),
            CtError::Api {
                status: 404,
                detail: None,
            },
        ] {
            let attempts = AtomicUsize::new(0);
            let mut error = Some(fatal);
            let result = reconnect(&CancellationToken::new(), || {
                attempts.fetch_add(1, Ordering::SeqCst);
                let error = error.take();
                async move { Err(error.unwrap_or(CtError::Transport("again".into()))) }
            })
            .await;
            assert!(result.is_err());
            assert_eq!(attempts.load(Ordering::SeqCst), 1);
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_drain_request_ends_the_reconnect_wait() {
        let drain = CancellationToken::new();
        let stopping = drain.clone();
        let result = reconnect(&drain, || {
            stopping.cancel();
            async { Err(CtError::Transport("offline".into())) }
        })
        .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn a_failed_release_keeps_the_receipt_journal() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("state").join("assignment");
        let journal = Journal::open(path.clone()).await.unwrap();

        let result = settle(
            Ok(()),
            Err(CtError::Transport("release unavailable".into())),
            &journal,
            path.clone(),
        )
        .await;
        assert!(matches!(result, Err(CtError::Transport(_))));
        assert!(path.exists());

        let result = settle(
            Err(CtError::Protocol("operation failed".into())),
            Ok(()),
            &journal,
            path.clone(),
        )
        .await;
        assert!(matches!(result, Err(CtError::Protocol(_))));
        assert!(path.exists());

        settle(Ok(()), Ok(()), &journal, path.clone())
            .await
            .unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn a_second_interrupt_ends_the_assignment_without_a_lease_error() {
        let abort = CancellationToken::new();
        assert!(matches!(stopped(&abort), Err(CtError::Protocol(_))));
        abort.cancel();
        assert!(stopped(&abort).is_ok());
    }

    #[tokio::test]
    async fn a_bad_gateway_on_lease_renewal_is_retried_within_the_lease() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let lease = api::AssignmentLease {
            assignment_id: Uuid::new_v4(),
            fence_token: 1,
            lease_expires_at: Utc::now() + chrono::Duration::seconds(60),
            lease_token: "lease".into(),
            session_id: Uuid::new_v4(),
            state: api::AssignmentState::Active,
            target_id: Uuid::new_v4(),
            worker_id: Uuid::new_v4(),
        };
        let body = serde_json::to_string(&lease).unwrap();
        let attempts = Arc::new(AtomicUsize::new(0));
        let server_attempts = attempts.clone();
        let server = tokio::spawn(async move {
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = [0_u8; 8192];
                let _ = stream.read(&mut request).await.unwrap();
                let response = if server_attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                    "HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                        .to_owned()
                } else {
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                };
                stream.write_all(response.as_bytes()).await.unwrap();
            }
        });
        let client = WorkerClient::new(&format!("http://{address}"), "test-token").unwrap();

        let renewed = retry_transport_until(lease_retry_deadline(&lease), {
            let lease = lease.clone();
            move || {
                let client = client.clone();
                let lease = lease.clone();
                async move {
                    client
                        .heartbeat(&lease, api::HeartbeatAssignmentRequestState::Active)
                        .await
                }
            }
        })
        .await
        .expect("the lease survives one bad gateway");

        assert_eq!(renewed.assignment_id, lease.assignment_id);
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
        server.await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn worker_retries_are_bounded_by_time_not_by_error_count() {
        let attempts = std::sync::Arc::new(AtomicUsize::new(0));
        let result = retry_transport({
            let attempts = attempts.clone();
            move || {
                let attempts = attempts.clone();
                async move {
                    if attempts.fetch_add(1, Ordering::SeqCst) < 8 {
                        Err(CtError::Transport("offline".into()))
                    } else {
                        Ok(7)
                    }
                }
            }
        })
        .await;

        assert_eq!(result.unwrap(), 7);
        assert_eq!(attempts.load(Ordering::SeqCst), 9);
    }

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

    #[tokio::test(start_paused = true)]
    async fn transport_retry_deadline_includes_a_slow_fetch() {
        let config = WatchConfig {
            base_delay: Duration::from_millis(1),
            max_delay: Duration::from_millis(2),
            factor: 1.5,
            jitter: 0.0,
            max_transport_errors: 5,
            overall_timeout: Duration::from_secs(1),
        };
        let task = tokio::spawn(async move {
            retry_within(
                || async {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    Ok::<(), CtError>(())
                },
                &config,
                Duration::from_millis(5),
                "worker transport retry deadline exceeded",
            )
            .await
        });
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_millis(60)).await;
        let result = task.await.expect("retry task");

        assert!(matches!(result, Err(CtError::Transport(_))));
    }
}
