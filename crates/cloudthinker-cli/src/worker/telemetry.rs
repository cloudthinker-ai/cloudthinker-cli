use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use axum::Router;
use axum::extract::State;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use chrono::{Local, SecondsFormat, Utc};
use cloudthinker_client::{CtError, CtResult};
use serde::Serialize;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::engine::output::{self, ConsoleEvent, WorkerActivity, WorkerConsole};

const CLI_VERSION: &str = env!("CARGO_PKG_VERSION");
const HEALTHY_POLL_AGE: Duration = Duration::from_secs(60);
const NO_POLL: u64 = u64::MAX;
const DURATION_BUCKETS: [f64; 9] = [0.1, 0.5, 1.0, 5.0, 15.0, 60.0, 300.0, 900.0, 3600.0];
const POLL_ERROR_REASONS: [&str; 4] = ["transport", "auth", "server", "other"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Debug,
    Info,
    Warn,
}

impl Level {
    fn name(self) -> &'static str {
        match self {
            Self::Debug => "debug",
            Self::Info => "info",
            Self::Warn => "warn",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkerState {
    Starting = 0,
    Online = 1,
    Reconnecting = 2,
    Draining = 3,
}

impl WorkerState {
    fn name(self) -> &'static str {
        match self {
            Self::Starting => "starting",
            Self::Online => "online",
            Self::Reconnecting => "reconnecting",
            Self::Draining => "draining",
        }
    }

    fn from_code(code: u8) -> Self {
        match code {
            1 => Self::Online,
            2 => Self::Reconnecting,
            3 => Self::Draining,
            _ => Self::Starting,
        }
    }
}

#[derive(Default, Clone, Copy)]
pub struct Fields<'a> {
    pub assignment_id: Option<Uuid>,
    pub operation_kind: Option<&'a str>,
    pub outcome: Option<&'a str>,
    pub duration_ms: Option<u128>,
    pub operation_id: Option<Uuid>,
    pub activity: Option<WorkerActivity>,
    pub error_code: Option<&'a str>,
}

#[derive(Default)]
struct Histogram {
    buckets: [u64; DURATION_BUCKETS.len()],
    count: u64,
    sum: f64,
}

#[derive(Default)]
struct Series {
    operations: BTreeMap<(String, String), u64>,
    durations: BTreeMap<String, Histogram>,
    poll_errors: BTreeMap<&'static str, u64>,
}

struct Telemetry {
    json: bool,
    verbose: bool,
    console: Mutex<WorkerConsole>,
    capacity: usize,
    started: Instant,
    state: AtomicU8,
    active: AtomicUsize,
    last_poll_ms: AtomicU64,
    credential_valid: AtomicBool,
    series: Mutex<Series>,
}

static TELEMETRY: OnceLock<Arc<Telemetry>> = OnceLock::new();

impl Telemetry {
    fn new(json: bool, capacity: usize, verbose: bool) -> Self {
        Self {
            json,
            verbose,
            console: Mutex::new(WorkerConsole::new(output::worker_console_style(), verbose)),
            capacity,
            started: Instant::now(),
            state: AtomicU8::new(WorkerState::Starting as u8),
            active: AtomicUsize::new(0),
            last_poll_ms: AtomicU64::new(NO_POLL),
            credential_valid: AtomicBool::new(true),
            series: Mutex::new(Series::default()),
        }
    }

    fn set_state(&self, state: WorkerState) {
        self.state.store(state as u8, Ordering::Relaxed);
    }

    fn poll_ok(&self) {
        let elapsed = u64::try_from(self.started.elapsed().as_millis()).unwrap_or(0);
        self.last_poll_ms.store(elapsed, Ordering::Relaxed);
        self.credential_valid.store(true, Ordering::Relaxed);
    }

    fn poll_error(&self, error: &CtError) -> &'static str {
        let reason = poll_error_reason(error);
        if reason == "auth" {
            self.credential_valid.store(false, Ordering::Relaxed);
        }
        if let Ok(mut series) = self.series.lock() {
            *series.poll_errors.entry(reason).or_default() += 1;
        }
        reason
    }

    fn operation(&self, kind: &str, outcome: &str, elapsed: Duration) {
        let Ok(mut series) = self.series.lock() else {
            return;
        };
        *series
            .operations
            .entry((kind.to_owned(), outcome.to_owned()))
            .or_default() += 1;
        let seconds = elapsed.as_secs_f64();
        let histogram = series.durations.entry(kind.to_owned()).or_default();
        histogram.count += 1;
        histogram.sum += seconds;
        for (slot, bound) in histogram.buckets.iter_mut().zip(DURATION_BUCKETS) {
            if seconds <= bound {
                *slot += 1;
            }
        }
    }
}

pub fn init(json: bool, capacity: usize, verbose: bool) {
    let verbose = verbose || debug_enabled();
    let _ = TELEMETRY.set(Arc::new(Telemetry::new(json, capacity, verbose)));
}

pub fn set_state(state: WorkerState) {
    if let Some(telemetry) = TELEMETRY.get() {
        telemetry.set_state(state);
    }
}

pub fn set_active(active: usize) {
    if let Some(telemetry) = TELEMETRY.get() {
        telemetry.active.store(active, Ordering::Relaxed);
    }
}

pub fn poll_ok() {
    if let Some(telemetry) = TELEMETRY.get() {
        telemetry.poll_ok();
    }
}

pub fn poll_error(error: &CtError) -> &'static str {
    match TELEMETRY.get() {
        Some(telemetry) => telemetry.poll_error(error),
        None => poll_error_reason(error),
    }
}

fn poll_error_reason(error: &CtError) -> &'static str {
    match error {
        CtError::Transport(message) if message.contains("failed with HTTP") => "server",
        CtError::Transport(_) | CtError::Timeout(_) => "transport",
        CtError::Auth(_) | CtError::ObsoleteCredentials { .. } => "auth",
        CtError::Api { status, .. } if *status >= 500 => "server",
        _ => "other",
    }
}

pub fn operation(kind: &str, outcome: &str, elapsed: Duration) {
    if let Some(telemetry) = TELEMETRY.get() {
        telemetry.operation(kind, outcome, elapsed);
    }
}

pub fn emit(level: Level, event: &str, fields: Fields<'_>, text: &str) {
    match TELEMETRY.get() {
        Some(telemetry) if telemetry.json => {
            if level == Level::Debug && !telemetry.verbose {
                return;
            }
            eprintln!("{}", json_line(level, event, fields));
        }
        Some(telemetry) => telemetry.show(console_event(level, event, &fields, text)),
        None => match level {
            Level::Debug => output::worker_debug(text),
            Level::Info => output::worker_event(text),
            Level::Warn => output::warn(text),
        },
    }
}

pub fn stopping(running: usize) {
    show(ConsoleEvent::Stopping { running });
}

pub fn stopped() {
    show(ConsoleEvent::Stopped);
}

fn show(event: ConsoleEvent<'_>) {
    if let Some(telemetry) = TELEMETRY.get()
        && !telemetry.json
    {
        telemetry.show(event);
    }
}

impl Telemetry {
    fn show(&self, event: ConsoleEvent<'_>) {
        let Ok(mut console) = self.console.lock() else {
            return;
        };
        let lines = console.handle(Local::now(), event);
        output::print_worker_lines(&lines);
    }
}

fn console_event<'a>(
    level: Level,
    event: &str,
    fields: &Fields<'a>,
    text: &'a str,
) -> ConsoleEvent<'a> {
    match (event, fields.assignment_id) {
        ("worker_online", _) => ConsoleEvent::Online,
        ("connection_lost", _) => ConsoleEvent::ConnectionLost { detail: text },
        ("worker_draining", _) => ConsoleEvent::Silent,
        ("worker_cancelling", _) => ConsoleEvent::Cancelling,
        ("assignment_claimed", Some(run)) => ConsoleEvent::RunStarted { run },
        ("assignment_finished", Some(run)) => ConsoleEvent::RunFinished { run },
        ("assignment_stopped", Some(run)) => ConsoleEvent::RunStopped { run, reason: text },
        ("operation_started", Some(run)) => ConsoleEvent::OperationStarted {
            run,
            operation: fields.operation_id,
            kind: fields.operation_kind.unwrap_or("operation"),
        },
        ("operation_finished", Some(run)) => ConsoleEvent::OperationFinished {
            run,
            operation: fields.operation_id,
            kind: fields.operation_kind.unwrap_or("operation"),
            activity: fields.activity.unwrap_or(WorkerActivity::Internal),
            outcome: fields.outcome.unwrap_or("succeeded"),
            code: fields.error_code,
            duration_ms: fields.duration_ms.unwrap_or(0),
        },
        _ => match level {
            Level::Debug => ConsoleEvent::Detail { text },
            Level::Info => ConsoleEvent::Note { text },
            Level::Warn => ConsoleEvent::Warning { text },
        },
    }
}

fn debug_enabled() -> bool {
    std::env::var(output::WORKER_LOG_ENV).is_ok_and(|level| level.eq_ignore_ascii_case("debug"))
}

#[derive(Serialize)]
struct LogLine<'a> {
    ts: String,
    level: &'static str,
    event: &'a str,
    assignment_id: Option<Uuid>,
    operation_kind: Option<&'a str>,
    outcome: Option<&'a str>,
    duration_ms: Option<u128>,
    cli_version: &'static str,
}

fn json_line(level: Level, event: &str, fields: Fields<'_>) -> String {
    let line = LogLine {
        ts: Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
        level: level.name(),
        event,
        assignment_id: fields.assignment_id,
        operation_kind: fields.operation_kind,
        outcome: fields.outcome,
        duration_ms: fields.duration_ms,
        cli_version: CLI_VERSION,
    };
    serde_json::to_string(&line).unwrap_or_else(|_| format!("{{\"event\":\"{event}\"}}"))
}

#[derive(Serialize)]
struct Health {
    state: &'static str,
    last_poll_age_ms: Option<u64>,
    active_assignments: usize,
    max_assignments: usize,
    cli_version: &'static str,
    credential_expires_at: Option<String>,
}

fn health(telemetry: &Telemetry) -> (bool, Health) {
    let last = telemetry.last_poll_ms.load(Ordering::Relaxed);
    let age = (last != NO_POLL).then(|| {
        u64::try_from(telemetry.started.elapsed().as_millis())
            .unwrap_or(0)
            .saturating_sub(last)
    });
    let fresh = age.is_some_and(|age| u128::from(age) < HEALTHY_POLL_AGE.as_millis());
    let healthy = fresh && telemetry.credential_valid.load(Ordering::Relaxed);
    (
        healthy,
        Health {
            state: WorkerState::from_code(telemetry.state.load(Ordering::Relaxed)).name(),
            last_poll_age_ms: age,
            active_assignments: telemetry.active.load(Ordering::Relaxed),
            max_assignments: telemetry.capacity,
            cli_version: CLI_VERSION,
            credential_expires_at: None,
        },
    )
}

fn render_metrics(telemetry: &Telemetry) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "# HELP cloudthinker_worker_info Worker build information.\n# TYPE cloudthinker_worker_info gauge\ncloudthinker_worker_info{{version=\"{CLI_VERSION}\"}} 1"
    );
    let _ = writeln!(
        out,
        "# HELP cloudthinker_worker_active_assignments Assignments this worker serves now.\n# TYPE cloudthinker_worker_active_assignments gauge\ncloudthinker_worker_active_assignments {}",
        telemetry.active.load(Ordering::Relaxed)
    );
    let _ = writeln!(
        out,
        "# HELP cloudthinker_worker_capacity Assignments this worker may serve at once.\n# TYPE cloudthinker_worker_capacity gauge\ncloudthinker_worker_capacity {}",
        telemetry.capacity
    );
    let series = telemetry.series.lock();
    let empty = Series::default();
    let series = series.as_deref().unwrap_or(&empty);
    out.push_str(
        "# HELP cloudthinker_worker_poll_errors_total Lost sessions by reason.\n# TYPE cloudthinker_worker_poll_errors_total counter\n",
    );
    for reason in POLL_ERROR_REASONS {
        let _ = writeln!(
            out,
            "cloudthinker_worker_poll_errors_total{{reason=\"{reason}\"}} {}",
            series.poll_errors.get(reason).copied().unwrap_or(0)
        );
    }
    out.push_str(
        "# HELP cloudthinker_worker_operations_total Operations finished by kind and outcome.\n# TYPE cloudthinker_worker_operations_total counter\n",
    );
    for ((kind, outcome), count) in &series.operations {
        let _ = writeln!(
            out,
            "cloudthinker_worker_operations_total{{kind=\"{kind}\",outcome=\"{outcome}\"}} {count}"
        );
    }
    out.push_str(
        "# HELP cloudthinker_worker_operation_duration_seconds Operation duration by kind.\n# TYPE cloudthinker_worker_operation_duration_seconds histogram\n",
    );
    for (kind, histogram) in &series.durations {
        for (bound, count) in DURATION_BUCKETS.iter().zip(histogram.buckets) {
            let _ = writeln!(
                out,
                "cloudthinker_worker_operation_duration_seconds_bucket{{kind=\"{kind}\",le=\"{bound}\"}} {count}"
            );
        }
        let _ = writeln!(
            out,
            "cloudthinker_worker_operation_duration_seconds_bucket{{kind=\"{kind}\",le=\"+Inf\"}} {}\ncloudthinker_worker_operation_duration_seconds_sum{{kind=\"{kind}\"}} {}\ncloudthinker_worker_operation_duration_seconds_count{{kind=\"{kind}\"}} {}",
            histogram.count, histogram.sum, histogram.count
        );
    }
    out
}

async fn healthz(State(telemetry): State<Arc<Telemetry>>) -> Response {
    let (healthy, body) = health(&telemetry);
    let status = if healthy {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (
        status,
        [(header::CONTENT_TYPE, "application/json")],
        serde_json::to_string(&body).unwrap_or_default(),
    )
        .into_response()
}

async fn metrics(State(telemetry): State<Arc<Telemetry>>) -> Response {
    (
        [(
            header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        render_metrics(&telemetry),
    )
        .into_response()
}

fn router(telemetry: Arc<Telemetry>) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/metrics", get(metrics))
        .with_state(telemetry)
}

pub async fn bind(address: SocketAddr) -> CtResult<tokio::net::TcpListener> {
    tokio::net::TcpListener::bind(address).await.map_err(|_| {
        CtError::Usage("--health-addr could not be bound; pick a free local port".into())
    })
}

pub fn serve(listener: tokio::net::TcpListener, stop: CancellationToken) {
    if let Some(telemetry) = TELEMETRY.get() {
        spawn_server(listener, telemetry.clone(), stop);
    }
}

fn spawn_server(
    listener: tokio::net::TcpListener,
    telemetry: Arc<Telemetry>,
    stop: CancellationToken,
) {
    tokio::spawn(async move {
        let _ = axum::serve(listener, router(telemetry))
            .with_graceful_shutdown(stop.cancelled_owned())
            .await;
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    async fn get_path(address: SocketAddr, path: &str) -> (u16, String) {
        let mut stream = tokio::net::TcpStream::connect(address).await.unwrap();
        stream
            .write_all(
                format!("GET {path} HTTP/1.1\r\nHost: worker\r\nConnection: close\r\n\r\n")
                    .as_bytes(),
            )
            .await
            .unwrap();
        let mut raw = String::new();
        stream.read_to_string(&mut raw).await.unwrap();
        let status = raw
            .split_whitespace()
            .nth(1)
            .and_then(|code| code.parse().ok())
            .unwrap();
        let body = raw.split("\r\n\r\n").nth(1).unwrap_or_default().to_owned();
        (status, body)
    }

    #[tokio::test]
    async fn worker_health_and_metrics_endpoints_keep_their_contract() {
        let telemetry = Arc::new(Telemetry::new(false, 4, false));
        let listener = bind("127.0.0.1:0".parse().unwrap()).await.unwrap();
        let address = listener.local_addr().unwrap();
        let stop = CancellationToken::new();
        spawn_server(listener, telemetry.clone(), stop.clone());

        let (status, body) = get_path(address, "/healthz").await;
        assert_eq!(status, 503);
        let unhealthy: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(unhealthy["state"], "starting");
        assert!(unhealthy["last_poll_age_ms"].is_null());

        telemetry.set_state(WorkerState::Online);
        telemetry.active.store(2, Ordering::Relaxed);
        telemetry.poll_ok();
        telemetry.operation("script", "succeeded", Duration::from_millis(250));
        telemetry.operation("script", "failed", Duration::from_secs(7));
        telemetry.poll_error(&CtError::Transport("down".into()));

        let (status, body) = get_path(address, "/healthz").await;
        assert_eq!(status, 200);
        let healthy: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(healthy["state"], "online");
        assert_eq!(healthy["active_assignments"], 2);
        assert_eq!(healthy["max_assignments"], 4);
        assert_eq!(healthy["cli_version"], CLI_VERSION);
        assert!(healthy["last_poll_age_ms"].as_u64().unwrap() < 60_000);
        assert!(healthy.get("credential_expires_at").is_some());

        let (status, body) = get_path(address, "/metrics").await;
        assert_eq!(status, 200);
        for expected in [
            format!("cloudthinker_worker_info{{version=\"{CLI_VERSION}\"}} 1"),
            "cloudthinker_worker_active_assignments 2".to_owned(),
            "cloudthinker_worker_capacity 4".to_owned(),
            "cloudthinker_worker_poll_errors_total{reason=\"transport\"} 1".to_owned(),
            "cloudthinker_worker_operations_total{kind=\"script\",outcome=\"succeeded\"} 1"
                .to_owned(),
            "cloudthinker_worker_operations_total{kind=\"script\",outcome=\"failed\"} 1".to_owned(),
            "cloudthinker_worker_operation_duration_seconds_bucket{kind=\"script\",le=\"0.5\"} 1"
                .to_owned(),
            "cloudthinker_worker_operation_duration_seconds_bucket{kind=\"script\",le=\"+Inf\"} 2"
                .to_owned(),
            "cloudthinker_worker_operation_duration_seconds_count{kind=\"script\"} 2".to_owned(),
        ] {
            assert!(
                body.lines().any(|line| line == expected),
                "missing {expected} in {body}"
            );
        }

        telemetry.poll_error(&CtError::Auth("revoked".into()));
        let (status, _) = get_path(address, "/healthz").await;
        assert_eq!(status, 503);
        stop.cancel();
    }

    #[test]
    fn json_log_lines_carry_the_documented_fields_and_nothing_else() {
        let line = json_line(
            Level::Info,
            "operation_finished",
            Fields {
                assignment_id: Some(Uuid::nil()),
                operation_kind: Some("script"),
                outcome: Some("succeeded"),
                duration_ms: Some(42),
                operation_id: Some(Uuid::nil()),
                activity: Some(WorkerActivity::Command),
                error_code: Some("TIMEOUT"),
            },
        );
        let value: serde_json::Value = serde_json::from_str(&line).unwrap();
        let mut keys: Vec<_> = value.as_object().unwrap().keys().cloned().collect();
        keys.sort();
        assert_eq!(
            keys,
            [
                "assignment_id",
                "cli_version",
                "duration_ms",
                "event",
                "level",
                "operation_kind",
                "outcome",
                "ts"
            ]
        );
        assert_eq!(value["level"], "info");
        assert_eq!(value["duration_ms"], 42);
    }
}
