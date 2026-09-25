use std::collections::BTreeMap;
use std::io::{ErrorKind, Read, Seek, SeekFrom, Write};
use std::os::fd::OwnedFd;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant, SystemTime};

use cap_std::fs::{Dir, DirBuilder, DirBuilderExt, OpenOptions, OpenOptionsExt};
use chrono::{DateTime, Utc};
use cloudthinker_client::auth::worker_store::private_directory;
use cloudthinker_client::worker_types as api;
use rustix::fs::FlockOperation;
use rustix::process::{Pid, Signal, kill_process_group};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::config::WorkdirIdentity;
use super::files::relative;

pub(crate) const STREAM_CAP_BYTES: u64 = 32 * 1024 * 1024;
pub(crate) const PAGE_ENCODED_BYTES: usize = 1024 * 1024;
pub(crate) const STATE_BUDGET_BYTES: u64 = 1024 * 1024 * 1024;
pub(crate) const RETENTION: Duration = Duration::from_secs(86_400);
pub(crate) const EXIT_CODE_UNKNOWN_TERMINAL: i32 = -1;
pub(crate) const CANCEL_GRACE: Duration = Duration::from_secs(30);
pub(crate) const COMMAND_FILE: &str = "cmd.sh";
pub(crate) const LOCK_FILE: &str = "task.lock";
pub(crate) const MANIFEST_FILE: &str = "manifest.json";
pub(crate) const STDOUT_FILE: &str = "stdout";
pub(crate) const STDERR_FILE: &str = "stderr";
pub(crate) const EXIT_FILE: &str = "exit";
pub(crate) const TRUNCATED_FILE: &str = "truncated";
const BG_DIR: &str = "bg";
const DEFAULT_TIMEOUT_SECS: u64 = 1800;
const START_WAIT: Duration = Duration::from_secs(2);
const START_POLL: Duration = Duration::from_millis(20);
const TAIL_POLL: Duration = Duration::from_millis(250);
const TAIL_DEADLINE_MARGIN: Duration = Duration::from_secs(5);
const PAGE_OVERHEAD_BYTES: usize = 160;
const MAX_TASK_ID_LEN: usize = 64;
const GC_GRACE: Duration = Duration::from_secs(60);
const BUS_ENV: [&str; 2] = ["XDG_RUNTIME_DIR", "DBUS_SESSION_BUS_ADDRESS"];
const SCOPE_PROBE_TIMEOUT: Duration = Duration::from_secs(5);
const UNIT_ENV: &str = "INVOCATION_ID";

#[derive(Serialize, Deserialize)]
pub(crate) struct TaskManifest {
    pub(crate) pid: u32,
    pub(crate) started_at: String,
    pub(crate) timeout_secs: u64,
}

pub(crate) struct TaskDir {
    dir: Dir,
    path: PathBuf,
}

pub(crate) enum Liveness {
    Exited(i32),
    Running(Option<Pid>),
    Vanished,
}

pub struct ShimImage {
    #[cfg(target_os = "linux")]
    image: std::fs::File,
    #[cfg(not(target_os = "linux"))]
    path: PathBuf,
    launch: Launch,
}

pub enum Launch {
    Direct,
    Scope(BTreeMap<String, String>),
    Refused(String),
}

impl ShimImage {
    pub async fn open(
        path: PathBuf,
        environment: &BTreeMap<String, String>,
    ) -> std::io::Result<Self> {
        let probe = probe_transient_scope(environment).await;
        Self::assemble(
            path,
            launch_mode(probe, std::env::var_os(UNIT_ENV).is_some()),
        )
    }

    fn assemble(path: PathBuf, launch: Launch) -> std::io::Result<Self> {
        Ok(Self {
            #[cfg(target_os = "linux")]
            image: std::fs::File::open(&path)?,
            #[cfg(not(target_os = "linux"))]
            path,
            launch,
        })
    }

    pub fn refusal(&self) -> Option<&str> {
        match &self.launch {
            Launch::Refused(reason) => Some(reason),
            Launch::Direct | Launch::Scope(_) => None,
        }
    }

    fn command(
        &self,
        environment: &BTreeMap<String, String>,
    ) -> Result<tokio::process::Command, &'static str> {
        #[cfg(target_os = "linux")]
        let (program, stdout) = (
            PathBuf::from("/proc/self/fd/1"),
            Stdio::from(
                self.image
                    .try_clone()
                    .map_err(|_| "EXECUTOR_BACKGROUND_START_FAILED")?,
            ),
        );
        #[cfg(not(target_os = "linux"))]
        let (program, stdout) = (self.path.clone(), Stdio::null());
        let mut command = match &self.launch {
            Launch::Scope(bus) => {
                let mut command = tokio::process::Command::new("systemd-run");
                command
                    .args(["--user", "--scope", "--collect", "--quiet", "--"])
                    .arg(program)
                    .env_clear()
                    .envs(environment)
                    .envs(bus);
                command
            }
            Launch::Direct => {
                let mut command = tokio::process::Command::new(program);
                command.env_clear().envs(environment);
                command
            }
            Launch::Refused(_) => return Err("EXECUTOR_BACKGROUND_START_FAILED"),
        };
        command.stdout(stdout);
        Ok(command)
    }
}

fn launch_mode(probe: Result<BTreeMap<String, String>, String>, inside_unit: bool) -> Launch {
    match probe {
        Ok(bus) => Launch::Scope(bus),
        Err(reason) if inside_unit => Launch::Refused(reason),
        Err(_) => Launch::Direct,
    }
}

async fn probe_transient_scope(
    environment: &BTreeMap<String, String>,
) -> Result<BTreeMap<String, String>, String> {
    if !cfg!(target_os = "linux") {
        return Err("transient scopes need Linux".into());
    }
    let bus: BTreeMap<String, String> = BUS_ENV
        .iter()
        .filter_map(|key| std::env::var(key).ok().map(|value| ((*key).into(), value)))
        .collect();
    let probe = tokio::process::Command::new("systemd-run")
        .args(["--user", "--scope", "--collect", "--quiet", "--", "true"])
        .env_clear()
        .envs(environment)
        .envs(&bus)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .output();
    match tokio::time::timeout(SCOPE_PROBE_TIMEOUT, probe).await {
        Ok(Ok(output)) if output.status.success() => Ok(bus),
        Ok(Ok(output)) => Err(format!(
            "systemd-run --user --scope probe failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )),
        Ok(Err(error)) => Err(format!("systemd-run could not run: {error}")),
        Err(_) => Err(format!(
            "systemd-run --user --scope probe timed out after {} s",
            SCOPE_PROBE_TIMEOUT.as_secs()
        )),
    }
}

pub(crate) async fn execute(
    identity: Arc<WorkdirIdentity>,
    shim: Arc<ShimImage>,
    op: api::BackgroundOperation,
    environment: BTreeMap<String, String>,
    deadline: DateTime<Utc>,
    cancel: CancellationToken,
) -> Result<Value, &'static str> {
    if op.credential_ref.is_some() {
        return Err("EXECUTOR_CREDENTIAL_NOT_LOCAL");
    }
    validate_task_id(&op.task_id)?;
    match op.action {
        api::BackgroundAction::Start => {
            start(identity, shim, op, environment, deadline, cancel).await
        }
        api::BackgroundAction::Tail => {
            let wait = tail_wait(op.wait_seconds, deadline);
            blocking(move || tail(&identity, &op, wait, &cancel)).await
        }
        api::BackgroundAction::Cancel => {
            blocking(move || cancel_task(&identity, &op.task_id)).await
        }
        api::BackgroundAction::Cleanup => blocking(move || cleanup(&identity, &op.task_id)).await,
    }
}

async fn blocking<F>(action: F) -> Result<Value, &'static str>
where
    F: FnOnce() -> Result<Value, &'static str> + Send + 'static,
{
    tokio::task::spawn_blocking(action)
        .await
        .map_err(|_| "EXECUTOR_BACKGROUND_INTERRUPTED")?
}

fn validate_task_id(task_id: &str) -> Result<(), &'static str> {
    if task_id.is_empty()
        || task_id.len() > MAX_TASK_ID_LEN
        || !task_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err("EXECUTOR_PATH_INVALID");
    }
    Ok(())
}

fn bg_root(identity: &WorkdirIdentity) -> Result<Dir, &'static str> {
    private_directory(&identity.state.join(BG_DIR))
        .map_err(|_| "EXECUTOR_BACKGROUND_STATE_UNAVAILABLE")
}

impl TaskDir {
    fn open(identity: &WorkdirIdentity, task_id: &str) -> Result<Option<Self>, &'static str> {
        match bg_root(identity)?.open_dir(task_id) {
            Ok(dir) => Ok(Some(Self {
                dir,
                path: identity.state.join(BG_DIR).join(task_id),
            })),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
            Err(_) => Err("EXECUTOR_BACKGROUND_STATE_UNAVAILABLE"),
        }
    }
}

async fn start(
    identity: Arc<WorkdirIdentity>,
    shim: Arc<ShimImage>,
    op: api::BackgroundOperation,
    environment: BTreeMap<String, String>,
    deadline: DateTime<Utc>,
    cancel: CancellationToken,
) -> Result<Value, &'static str> {
    let cmd = op.cmd.ok_or("EXECUTOR_BACKGROUND_START_FAILED")?;
    let timeout_secs = op
        .timeout
        .and_then(|t| u64::try_from(t).ok())
        .filter(|t| *t > 0)
        .unwrap_or(DEFAULT_TIMEOUT_SECS);
    let working_directory = op.working_directory.unwrap_or_else(|| ".".into());
    let task_id = op.task_id;
    let prepared = {
        let identity = identity.clone();
        let task_id = task_id.clone();
        tokio::task::spawn_blocking(move || prepare(&identity, &task_id, &working_directory, &cmd))
            .await
            .map_err(|_| "EXECUTOR_BACKGROUND_INTERRUPTED")?
    };
    let Some((task, cwd)) = prepared? else {
        return Ok(json!({"started": false}));
    };
    let outcome = launch(
        &task,
        cwd,
        &shim,
        &environment,
        timeout_secs,
        deadline,
        cancel,
    )
    .await;
    if outcome.is_err() {
        let _ = tokio::task::spawn_blocking(move || abort(&identity, &task_id)).await;
    }
    outcome.map(|()| json!({"started": true}))
}

fn prepare(
    identity: &WorkdirIdentity,
    task_id: &str,
    working_directory: &str,
    cmd: &str,
) -> Result<Option<(TaskDir, OwnedFd)>, &'static str> {
    let cwd = identity
        .root
        .open_dir(relative(working_directory)?)
        .map_err(|_| "TRUSTED_ROOT_REJECTED")?;
    let root = bg_root(identity)?;
    if identity.background_bytes.load(Ordering::Relaxed) > STATE_BUDGET_BYTES {
        return Err("EXECUTOR_BACKGROUND_BUDGET_EXCEEDED");
    }
    let mut builder = DirBuilder::new();
    builder.mode(0o700);
    match root.create_dir_with(task_id, &builder) {
        Ok(()) => {}
        Err(error) if error.kind() == ErrorKind::AlreadyExists => return Ok(None),
        Err(_) => return Err("EXECUTOR_BACKGROUND_START_FAILED"),
    }
    let task = TaskDir::open(identity, task_id)?.ok_or("EXECUTOR_BACKGROUND_START_FAILED")?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true).mode(0o600);
    let written = task
        .dir
        .open_with(COMMAND_FILE, &options)
        .and_then(|mut file| {
            file.write_all(cmd.as_bytes())
                .and_then(|()| file.sync_all())
        });
    if written.is_err() {
        let _ = root.remove_dir_all(task_id);
        return Err("EXECUTOR_BACKGROUND_START_FAILED");
    }
    Ok(Some((task, OwnedFd::from(cwd))))
}

async fn launch(
    task: &TaskDir,
    cwd: OwnedFd,
    shim: &ShimImage,
    environment: &BTreeMap<String, String>,
    timeout_secs: u64,
    deadline: DateTime<Utc>,
    cancel: CancellationToken,
) -> Result<(), &'static str> {
    let mut child = shim
        .command(environment)?
        .args(["worker", "bg-shim", "--task-dir"])
        .arg(&task.path)
        .arg("--timeout-secs")
        .arg(timeout_secs.to_string())
        .stdin(Stdio::from(cwd))
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| "EXECUTOR_BACKGROUND_START_FAILED")?;
    let wait = (deadline - Utc::now())
        .to_std()
        .unwrap_or_default()
        .min(START_WAIT);
    let started = tokio::time::Instant::now();
    loop {
        if task.dir.exists(MANIFEST_FILE) {
            return Ok(());
        }
        let exited = child.try_wait().ok().flatten().is_some();
        if exited || cancel.is_cancelled() || started.elapsed() >= wait {
            return if task.dir.exists(MANIFEST_FILE) {
                Ok(())
            } else {
                Err("EXECUTOR_BACKGROUND_START_FAILED")
            };
        }
        tokio::time::sleep(START_POLL).await;
    }
}

fn abort(identity: &WorkdirIdentity, task_id: &str) {
    kill_supervisor(identity, task_id);
    if let Ok(root) = bg_root(identity) {
        let _ = root.remove_dir_all(task_id);
    }
}

fn kill_supervisor(identity: &WorkdirIdentity, task_id: &str) {
    if let Ok(Some(task)) = TaskDir::open(identity, task_id)
        && let Some(pid) = supervisor_pid(&task)
    {
        let _ = kill_process_group(pid, Signal::KILL);
    }
}

pub(crate) fn liveness(task: &TaskDir) -> Liveness {
    if let Some(code) = exit_code(task) {
        return Liveness::Exited(code);
    }
    let Ok(lock) = task.dir.open(LOCK_FILE) else {
        return Liveness::Vanished;
    };
    if rustix::fs::flock(&lock, FlockOperation::NonBlockingLockExclusive).is_err() {
        return Liveness::Running(manifest_pid(task));
    }
    exit_code(task).map_or(Liveness::Vanished, Liveness::Exited)
}

fn supervisor_pid(task: &TaskDir) -> Option<Pid> {
    let lock = task.dir.open(LOCK_FILE).ok()?;
    rustix::fs::flock(&lock, FlockOperation::NonBlockingLockExclusive)
        .is_err()
        .then(|| manifest_pid(task))
        .flatten()
}

fn exit_code(task: &TaskDir) -> Option<i32> {
    task.dir
        .read_to_string(EXIT_FILE)
        .ok()
        .map(|text| text.trim().parse().unwrap_or(EXIT_CODE_UNKNOWN_TERMINAL))
}

fn manifest_pid(task: &TaskDir) -> Option<Pid> {
    let bytes = task.dir.read(MANIFEST_FILE).ok()?;
    let manifest: TaskManifest = serde_json::from_slice(&bytes).ok()?;
    Pid::from_raw(i32::try_from(manifest.pid).ok()?)
}

fn tail_wait(wait_seconds: Option<i64>, deadline: DateTime<Utc>) -> Duration {
    let requested = wait_seconds
        .and_then(|seconds| u64::try_from(seconds).ok())
        .map_or(Duration::ZERO, Duration::from_secs);
    let until_deadline = (deadline - Utc::now())
        .to_std()
        .unwrap_or_default()
        .saturating_sub(TAIL_DEADLINE_MARGIN);
    requested.min(until_deadline)
}

fn tail(
    identity: &WorkdirIdentity,
    op: &api::BackgroundOperation,
    wait: Duration,
    cancel: &CancellationToken,
) -> Result<Value, &'static str> {
    let task = TaskDir::open(identity, &op.task_id)?.ok_or("EXECUTOR_BACKGROUND_TASK_UNKNOWN")?;
    let offset = offset_of(op.offset)?;
    let stderr_offset = offset_of(op.stderr_offset)?;
    let started = Instant::now();
    while quiet(&task, offset, stderr_offset) && !cancel.is_cancelled() {
        let Some(remaining) = wait.checked_sub(started.elapsed()).filter(|r| !r.is_zero()) else {
            break;
        };
        std::thread::sleep(remaining.min(TAIL_POLL));
    }
    page_of(&task, offset, stderr_offset)
}

fn quiet(task: &TaskDir, offset: u64, stderr_offset: u64) -> bool {
    matches!(liveness(task), Liveness::Running(_))
        && stream_len(task, STDOUT_FILE) <= offset
        && stream_len(task, STDERR_FILE) <= stderr_offset
}

fn stream_len(task: &TaskDir, name: &str) -> u64 {
    task.dir.metadata(name).map_or(0, |metadata| metadata.len())
}

fn page_of(task: &TaskDir, offset: u64, stderr_offset: u64) -> Result<Value, &'static str> {
    let terminal = match liveness(task) {
        Liveness::Running(_) => None,
        Liveness::Exited(code) => Some(code),
        Liveness::Vanished => Some(EXIT_CODE_UNKNOWN_TERMINAL),
    };
    let marker = task
        .dir
        .exists(TRUNCATED_FILE)
        .then(|| format!("[output truncated at {} MiB]\n", STREAM_CAP_BYTES >> 20));
    let budget = PAGE_ENCODED_BYTES - PAGE_OVERHEAD_BYTES;
    let hold_partial = terminal.is_none();
    let (stdout, new_offset, stdout_complete) =
        page(task, STDOUT_FILE, offset, budget, hold_partial)?;
    let (mut stderr, new_stderr_offset, stderr_complete) = page(
        task,
        STDERR_FILE,
        stderr_offset,
        budget
            .saturating_sub(encoded_len(&stdout))
            .saturating_sub(marker.as_ref().map_or(0, String::len)),
        hold_partial,
    )?;
    let exit_code = terminal.filter(|_| stdout_complete && stderr_complete);
    if let (Some(_), Some(marker)) = (exit_code, marker) {
        stderr.push_str(&marker);
    }
    Ok(json!({
        "stdout_text": stdout,
        "stderr_text": stderr,
        "new_offset": new_offset,
        "new_stderr_offset": new_stderr_offset,
        "exit_code": exit_code,
    }))
}

fn offset_of(value: Option<i64>) -> Result<u64, &'static str> {
    value.map_or(Ok(0), |value| {
        u64::try_from(value).map_err(|_| "EXECUTOR_BACKGROUND_OFFSET_INVALID")
    })
}

fn page(
    task: &TaskDir,
    name: &str,
    offset: u64,
    budget: usize,
    hold_partial: bool,
) -> Result<(String, u64, bool), &'static str> {
    let mut file = match task.dir.open(name) {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::NotFound => {
            return Ok((String::new(), offset, true));
        }
        Err(_) => return Err("EXECUTOR_BACKGROUND_STATE_UNAVAILABLE"),
    };
    file.seek(SeekFrom::Start(offset))
        .map_err(|_| "EXECUTOR_BACKGROUND_STATE_UNAVAILABLE")?;
    let mut raw = Vec::new();
    file.take(budget as u64 + 4)
        .read_to_end(&mut raw)
        .map_err(|_| "EXECUTOR_BACKGROUND_STATE_UNAVAILABLE")?;
    let eof = raw.len() <= budget;
    let cut = if !eof {
        boundary(&raw, budget)
    } else if hold_partial {
        raw.len() - incomplete_tail(&raw)
    } else {
        raw.len()
    };
    let (text, consumed) = fit(&raw[..cut], budget);
    Ok((text, offset + consumed as u64, eof && consumed == raw.len()))
}

fn fit(raw: &[u8], budget: usize) -> (String, usize) {
    let mut len = raw.len();
    loop {
        let text = String::from_utf8_lossy(&raw[..len]);
        let encoded = encoded_len(&text);
        if encoded <= budget || len == 0 {
            return (text.into_owned(), len);
        }
        let shrunk =
            usize::try_from(u128::from(len as u64) * budget as u128 / encoded as u128).unwrap_or(0);
        len = boundary(raw, shrunk.min(len - 1));
    }
}

fn incomplete_tail(raw: &[u8]) -> usize {
    for back in 1..=raw.len().min(3) {
        let lead = raw[raw.len() - back];
        if lead & 0xC0 == 0x80 {
            continue;
        }
        let need = match lead {
            0xC0..=0xDF => 2,
            0xE0..=0xEF => 3,
            0xF0..=0xF7 => 4,
            _ => return 0,
        };
        return if need > back { back } else { 0 };
    }
    0
}

fn boundary(raw: &[u8], cut: usize) -> usize {
    let mut len = cut;
    while len > 0 && cut - len < 3 && raw.get(len).is_some_and(|byte| byte & 0xC0 == 0x80) {
        len -= 1;
    }
    if len == 0 { cut } else { len }
}

fn encoded_len(text: &str) -> usize {
    serde_json::to_string(text).map_or(usize::MAX, |s| s.len())
}

fn cancel_task(identity: &WorkdirIdentity, task_id: &str) -> Result<Value, &'static str> {
    let signaled = match TaskDir::open(identity, task_id)? {
        Some(task) => match liveness(&task) {
            Liveness::Running(Some(pid)) => kill_process_group(pid, Signal::TERM).is_ok(),
            _ => false,
        },
        None => false,
    };
    Ok(json!({"signaled": signaled}))
}

pub(crate) fn gc(identity: &WorkdirIdentity, now: SystemTime) {
    collect(identity, now, RETENTION, STATE_BUDGET_BYTES);
}

struct Scanned {
    task_id: String,
    age: Duration,
    bytes: u64,
    running: bool,
    pid: Option<Pid>,
}

fn collect(identity: &WorkdirIdentity, now: SystemTime, retention: Duration, budget: u64) {
    let Ok(root) = bg_root(identity) else {
        return;
    };
    let mut tasks = scan(identity, &root, now);
    tasks.sort_by_key(|task| std::cmp::Reverse(task.age));
    let mut total: u64 = tasks.iter().map(|task| task.bytes).sum();
    for task in tasks {
        let expired = task.age > retention;
        let evictable = !task.running && task.age >= GC_GRACE && total > budget;
        if !expired && !evictable {
            continue;
        }
        if let Some(pid) = task.pid {
            let _ = kill_process_group(pid, Signal::KILL);
        }
        if root.remove_dir_all(&task.task_id).is_ok() {
            total -= task.bytes;
        }
    }
    identity.background_bytes.store(total, Ordering::Relaxed);
}

fn scan(identity: &WorkdirIdentity, root: &Dir, now: SystemTime) -> Vec<Scanned> {
    let Ok(entries) = root.entries() else {
        return Vec::new();
    };
    entries
        .filter_map(|entry| {
            let task_id = entry.ok()?.file_name().into_string().ok()?;
            let task = TaskDir::open(identity, &task_id).ok()??;
            Some(Scanned {
                age: age(&task, now),
                bytes: dir_bytes(&task.dir),
                running: matches!(liveness(&task), Liveness::Running(_)),
                pid: supervisor_pid(&task),
                task_id,
            })
        })
        .collect()
}

fn age(task: &TaskDir, now: SystemTime) -> Duration {
    let started = task
        .dir
        .read(MANIFEST_FILE)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<TaskManifest>(&bytes).ok())
        .and_then(|manifest| DateTime::parse_from_rfc3339(&manifest.started_at).ok())
        .map(SystemTime::from)
        .or_else(|| {
            task.dir
                .dir_metadata()
                .ok()?
                .modified()
                .ok()
                .map(cap_std::time::SystemTime::into_std)
        });
    started
        .and_then(|started| now.duration_since(started).ok())
        .unwrap_or_default()
}

fn dir_bytes(dir: &Dir) -> u64 {
    dir.entries().map_or(0, |entries| {
        entries
            .filter_map(|entry| entry.ok()?.metadata().ok())
            .map(|metadata| metadata.len())
            .sum()
    })
}

fn cleanup(identity: &WorkdirIdentity, task_id: &str) -> Result<Value, &'static str> {
    kill_supervisor(identity, task_id);
    match bg_root(identity)?.remove_dir_all(task_id) {
        Ok(()) => Ok(json!({"cleaned": true})),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(json!({"cleaned": true})),
        Err(_) => Err("EXECUTOR_BACKGROUND_CLEANUP_FAILED"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    struct Fixture {
        _temporary: tempfile::TempDir,
        work: PathBuf,
        state: PathBuf,
        outpost: Uuid,
        identity: Arc<WorkdirIdentity>,
        shim: Arc<ShimImage>,
    }

    fn shim_binary() -> PathBuf {
        let mut shim = std::env::current_exe().unwrap();
        shim.pop();
        shim.pop();
        shim.push("cloudthinker");
        assert!(
            shim.is_file(),
            "run `cargo test` for the whole crate so the cloudthinker binary exists"
        );
        shim
    }

    async fn fixture() -> Fixture {
        let temporary = tempfile::tempdir().unwrap();
        let work = temporary.path().join("project");
        std::fs::create_dir(&work).unwrap();
        let state = temporary.path().join("state");
        private_directory(&state).unwrap();
        let outpost = Uuid::new_v4();
        let identity = Arc::new(WorkdirIdentity::open(&work, &state, outpost).unwrap());
        let shim = Arc::new(
            ShimImage::open(shim_binary(), &BTreeMap::new())
                .await
                .unwrap(),
        );
        Fixture {
            _temporary: temporary,
            work,
            state,
            outpost,
            identity,
            shim,
        }
    }

    fn fake_task(fixture: &Fixture, task_id: &str, started_at: DateTime<Utc>, bytes: u64) {
        let root = bg_root(&fixture.identity).unwrap();
        root.create_dir(task_id).unwrap();
        let dir = root.open_dir(task_id).unwrap();
        let manifest = TaskManifest {
            pid: 1,
            started_at: started_at.to_rfc3339(),
            timeout_secs: 60,
        };
        dir.write(MANIFEST_FILE, serde_json::to_vec(&manifest).unwrap())
            .unwrap();
        dir.write(EXIT_FILE, b"0").unwrap();
        dir.create(STDOUT_FILE).unwrap().set_len(bytes).unwrap();
    }

    fn rewrite_started_at(task: &TaskDir, started_at: DateTime<Utc>) {
        let bytes = task.dir.read(MANIFEST_FILE).unwrap();
        let mut manifest: TaskManifest = serde_json::from_slice(&bytes).unwrap();
        manifest.started_at = started_at.to_rfc3339();
        task.dir
            .write(MANIFEST_FILE, serde_json::to_vec(&manifest).unwrap())
            .unwrap();
    }

    async fn wait_for<F: Fn() -> bool>(condition: F) {
        for _ in 0..100 {
            if condition() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("condition not reached within 10 s");
    }

    fn operation(action: &str, task_id: &str, extra: Value) -> api::BackgroundOperation {
        let mut value = json!({"kind": "background", "action": action, "task_id": task_id});
        for (key, item) in extra.as_object().unwrap() {
            value[key] = item.clone();
        }
        serde_json::from_value(value).unwrap()
    }

    impl Fixture {
        async fn run(&self, op: api::BackgroundOperation) -> Result<Value, &'static str> {
            self.run_with(
                op,
                Utc::now() + chrono::Duration::seconds(60),
                CancellationToken::new(),
            )
            .await
        }

        async fn run_with(
            &self,
            op: api::BackgroundOperation,
            deadline: DateTime<Utc>,
            cancel: CancellationToken,
        ) -> Result<Value, &'static str> {
            execute(
                self.identity.clone(),
                self.shim.clone(),
                op,
                BTreeMap::new(),
                deadline,
                cancel,
            )
            .await
        }

        async fn timed_tail(
            &self,
            task_id: &str,
            wait_seconds: i64,
            deadline: DateTime<Utc>,
            cancel: CancellationToken,
        ) -> (Value, Duration) {
            let started = Instant::now();
            let page = self
                .run_with(
                    operation(
                        "tail",
                        task_id,
                        json!({"offset": 0, "stderr_offset": 0, "wait_seconds": wait_seconds}),
                    ),
                    deadline,
                    cancel,
                )
                .await
                .unwrap();
            (page, started.elapsed())
        }

        async fn start(&self, task_id: &str, cmd: &str) -> Value {
            self.run(operation("start", task_id, json!({"cmd": cmd})))
                .await
                .unwrap()
        }

        async fn tail(&self, task_id: &str, offset: u64, stderr_offset: u64) -> Value {
            self.run(operation(
                "tail",
                task_id,
                json!({"offset": offset, "stderr_offset": stderr_offset}),
            ))
            .await
            .unwrap()
        }

        async fn drain(&self, task_id: &str) -> (String, String, i32, Vec<Value>) {
            let (mut stdout, mut stderr) = (String::new(), String::new());
            let (mut offset, mut stderr_offset) = (0, 0);
            let mut pages = Vec::new();
            for _ in 0..600 {
                let page = self.tail(task_id, offset, stderr_offset).await;
                stdout.push_str(page["stdout_text"].as_str().unwrap());
                stderr.push_str(page["stderr_text"].as_str().unwrap());
                offset = page["new_offset"].as_u64().unwrap();
                stderr_offset = page["new_stderr_offset"].as_u64().unwrap();
                let exit = page["exit_code"].as_i64();
                pages.push(page);
                if let Some(code) = exit {
                    return (stdout, stderr, code as i32, pages);
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            panic!("task {task_id} never reached a terminal state");
        }

        fn task(&self, task_id: &str) -> Option<TaskDir> {
            TaskDir::open(&self.identity, task_id).unwrap()
        }

        async fn restart_worker(self) -> Self {
            let Fixture {
                _temporary,
                work,
                state,
                outpost,
                identity,
                shim,
            } = self;
            assert_eq!(Arc::strong_count(&identity), 1);
            drop(identity);
            drop(shim);
            let identity = Arc::new(WorkdirIdentity::open(&work, &state, outpost).unwrap());
            let shim = Arc::new(
                ShimImage::open(shim_binary(), &BTreeMap::new())
                    .await
                    .unwrap(),
            );
            Fixture {
                _temporary,
                work,
                state,
                outpost,
                identity,
                shim,
            }
        }
    }

    #[tokio::test]
    async fn ca_bg_01_02_09_start_is_idempotent_and_cleanup_converges() {
        let fixture = fixture().await;
        assert_eq!(
            fixture.start("t1", "sleep 30").await,
            json!({"started": true})
        );
        let task = fixture.task("t1").unwrap();
        assert!(task.dir.exists(MANIFEST_FILE));
        let pid = manifest_pid(&task).unwrap();
        assert!(matches!(liveness(&task), Liveness::Running(Some(p)) if p == pid));
        assert_eq!(
            fixture.start("t1", "sleep 30").await,
            json!({"started": false})
        );
        assert_eq!(manifest_pid(&task), Some(pid));
        let page = fixture.tail("t1", 0, 0).await;
        assert_eq!(page["exit_code"], Value::Null);
        assert_eq!(
            fixture.run(operation("cleanup", "t1", json!({}))).await,
            Ok(json!({"cleaned": true}))
        );
        assert!(fixture.task("t1").is_none());
        assert_eq!(
            fixture.run(operation("cleanup", "t1", json!({}))).await,
            Ok(json!({"cleaned": true}))
        );
        assert_eq!(
            fixture.run(operation("cancel", "t1", json!({}))).await,
            Ok(json!({"signaled": false}))
        );
        assert_eq!(
            fixture.run(operation("tail", "t1", json!({}))).await,
            Err("EXECUTOR_BACKGROUND_TASK_UNKNOWN")
        );
    }

    #[tokio::test]
    async fn ca_bg_03_17_verbatim_command_and_sliced_tail() {
        let fixture = fixture().await;
        let cmd = "printf 'a\"b\\n'\necho \"it's\" é\necho err 1>&2\nexit 3";
        assert_eq!(fixture.start("t2", cmd).await, json!({"started": true}));
        let (stdout, stderr, code, _) = fixture.drain("t2").await;
        assert_eq!(stdout, "a\"b\nit's é\n");
        assert_eq!(stderr, "err\n");
        assert_eq!(code, 3);
        let page = fixture.tail("t2", 4, 1).await;
        assert_eq!(page["stdout_text"], "it's é\n");
        assert_eq!(page["stderr_text"], "rr\n");
        assert_eq!(page["new_offset"], stdout.len() as u64);
        assert_eq!(page["new_stderr_offset"], 4);
        assert_eq!(page["exit_code"], 3);
        assert_eq!(
            fixture
                .task("t2")
                .unwrap()
                .dir
                .read_to_string(COMMAND_FILE)
                .unwrap(),
            cmd
        );
    }

    #[tokio::test]
    async fn ca_bg_07_cancel_terminates_the_group() {
        let fixture = fixture().await;
        fixture
            .start("t3", "trap 'exit 143' TERM; sleep 60 & wait")
            .await;
        assert_eq!(
            fixture.run(operation("cancel", "t3", json!({}))).await,
            Ok(json!({"signaled": true}))
        );
        let (_, _, code, _) = fixture.drain("t3").await;
        assert_eq!(code, 143);
        assert_eq!(
            fixture.run(operation("cancel", "t3", json!({}))).await,
            Ok(json!({"signaled": false}))
        );
    }

    #[tokio::test]
    async fn ca_bg_11_shim_enforces_its_own_timeout() {
        let fixture = fixture().await;
        fixture
            .run(operation(
                "start",
                "t4",
                json!({"cmd": "sleep 60", "timeout": 1}),
            ))
            .await
            .unwrap();
        let (_, _, code, _) = fixture.drain("t4").await;
        assert_eq!(code, 143);
    }

    #[tokio::test]
    async fn ca_bg_18_pages_stay_under_the_encoded_cap_and_offsets_sum_to_the_bytes() {
        let fixture = fixture().await;
        fixture
            .start(
                "t5",
                "head -c 300000 /dev/urandom; head -c 300000 /dev/urandom >&2",
            )
            .await;
        let (_, _, code, pages) = fixture.drain("t5").await;
        assert_eq!(code, 0);
        assert!(pages.len() > 1);
        for page in &pages {
            assert!(serde_json::to_vec(page).unwrap().len() <= PAGE_ENCODED_BYTES);
        }
        let last = pages.last().unwrap();
        assert_eq!(last["new_offset"], 300000);
        assert_eq!(last["new_stderr_offset"], 300000);
    }

    #[tokio::test]
    async fn ca_bg_19_daemon_grandchild_does_not_delay_the_exit_code_and_dies_at_cleanup() {
        let fixture = fixture().await;
        fixture
            .start("t6", "sleep 300 & echo $! > daemon.pid; echo up; exit 0")
            .await;
        let (stdout, _, code, _) = fixture.drain("t6").await;
        assert_eq!(stdout, "up\n");
        assert_eq!(code, 0);
        let task = fixture.task("t6").unwrap();
        let shim = supervisor_pid(&task).unwrap();
        let daemon: i32 = std::fs::read_to_string(fixture.work.join("daemon.pid"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let daemon = Pid::from_raw(daemon).unwrap();
        assert!(rustix::process::test_kill_process(daemon).is_ok());
        assert_eq!(
            fixture.run(operation("cleanup", "t6", json!({}))).await,
            Ok(json!({"cleaned": true}))
        );
        assert!(fixture.task("t6").is_none());
        wait_for(|| {
            rustix::process::test_kill_process_group(shim).is_err()
                && rustix::process::test_kill_process(daemon).is_err()
        })
        .await;
    }

    #[tokio::test]
    async fn ca_bg_13_14_credential_ref_escaping_working_directory_and_bad_task_id_are_rejected() {
        let fixture = fixture().await;
        assert_eq!(
            fixture
                .run(operation(
                    "start",
                    "t7",
                    json!({"cmd": "true", "working_directory": "../x"}),
                ))
                .await,
            Err("TRUSTED_ROOT_REJECTED")
        );
        assert!(fixture.task("t7").is_none());
        assert_eq!(
            fixture
                .run(operation("start", "../t7", json!({"cmd": "true"})))
                .await,
            Err("EXECUTOR_PATH_INVALID")
        );
        assert_eq!(
            fixture
                .run(operation(
                    "start",
                    "t8",
                    json!({"cmd": "true", "credential_ref": Uuid::new_v4()}),
                ))
                .await,
            Err("EXECUTOR_CREDENTIAL_NOT_LOCAL")
        );
    }

    #[tokio::test]
    async fn ca_bg_20_unspawnable_shim_removes_the_task_dir() {
        let mut fixture = fixture().await;
        let empty = fixture._temporary.path().join("empty");
        std::fs::write(&empty, "").unwrap();
        fixture.shim = Arc::new(ShimImage::open(empty, &BTreeMap::new()).await.unwrap());
        assert_eq!(
            fixture
                .run(operation("start", "t9", json!({"cmd": "true"})))
                .await,
            Err("EXECUTOR_BACKGROUND_START_FAILED")
        );
        assert!(fixture.task("t9").is_none());
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn shim_image_survives_replacing_the_binary_on_disk() {
        let mut fixture = fixture().await;
        let copy = fixture._temporary.path().join("cloudthinker-copy");
        std::fs::copy(shim_binary(), &copy).unwrap();
        fixture.shim = Arc::new(
            ShimImage::open(copy.clone(), &BTreeMap::new())
                .await
                .unwrap(),
        );
        let replacement = fixture._temporary.path().join("cloudthinker-new");
        std::fs::write(&replacement, "").unwrap();
        std::fs::rename(&replacement, &copy).unwrap();
        assert_eq!(
            fixture.start("t10", "echo alive").await,
            json!({"started": true})
        );
        let (stdout, _, code, _) = fixture.drain("t10").await;
        assert_eq!((stdout.as_str(), code), ("alive\n", 0));
    }

    #[tokio::test]
    async fn ca_bg_04_05_worker_restart_leaves_the_job_running() {
        let fixture = fixture().await;
        fixture.start("t11", "echo first; sleep 5; echo done").await;
        wait_for(|| {
            fixture
                .task("t11")
                .unwrap()
                .dir
                .read_to_string(STDOUT_FILE)
                .is_ok_and(|text| text == "first\n")
        })
        .await;
        let page = fixture.tail("t11", 0, 0).await;
        assert_eq!(page["stdout_text"], "first\n");
        assert_eq!(page["exit_code"], Value::Null);
        let offset = page["new_offset"].as_u64().unwrap();
        let fixture = fixture.restart_worker().await;
        assert!(matches!(
            liveness(&fixture.task("t11").unwrap()),
            Liveness::Running(Some(_))
        ));
        let page = fixture.tail("t11", offset, 0).await;
        assert_eq!(page["new_offset"], offset);
        let (stdout, _, code, _) = fixture.drain("t11").await;
        assert_eq!((stdout.as_str(), code), ("first\ndone\n", 0));
    }

    #[tokio::test]
    async fn ca_bg_06_lost_supervisor_without_an_exit_file_is_unknown_terminal() {
        let fixture = fixture().await;
        fixture.start("t12", "sleep 30").await;
        let pid = manifest_pid(&fixture.task("t12").unwrap()).unwrap();
        kill_process_group(pid, Signal::KILL).unwrap();
        wait_for(|| matches!(liveness(&fixture.task("t12").unwrap()), Liveness::Vanished)).await;
        let page = fixture.tail("t12", 0, 0).await;
        assert_eq!(page["exit_code"], EXIT_CODE_UNKNOWN_TERMINAL);
        assert_eq!(
            fixture.run(operation("cleanup", "t12", json!({}))).await,
            Ok(json!({"cleaned": true}))
        );
    }

    #[tokio::test]
    async fn ca_bg_10_capped_stream_carries_one_truncation_marker() {
        let fixture = fixture().await;
        fixture.start("t13", "yes | head -c 40M").await;
        let (stdout, stderr, code, pages) = fixture.drain("t13").await;
        assert_eq!(code, 0);
        assert_eq!(stdout.len() as u64, STREAM_CAP_BYTES);
        let task = fixture.task("t13").unwrap();
        assert!(task.dir.exists(TRUNCATED_FILE));
        assert!(task.dir.metadata(STDOUT_FILE).unwrap().len() <= STREAM_CAP_BYTES);
        assert_eq!(stderr, "[output truncated at 32 MiB]\n");
        assert_eq!(pages.last().unwrap()["stderr_text"], stderr);
        assert_eq!(pages.last().unwrap()["new_stderr_offset"], 0);
        assert_eq!(
            pages
                .iter()
                .filter(|page| page["stderr_text"].as_str().unwrap().contains("truncated"))
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn ca_bg_16_gc_removes_expired_and_over_budget_dirs_but_not_young_running_ones() {
        let fixture = fixture().await;
        let now = Utc::now();
        fake_task(&fixture, "expired", now - chrono::Duration::hours(25), 10);
        fake_task(&fixture, "older", now - chrono::Duration::hours(3), 100);
        fake_task(&fixture, "newer", now - chrono::Duration::hours(2), 50);
        fixture.start("expired-running", "sleep 60").await;
        let expired_pid = manifest_pid(&fixture.task("expired-running").unwrap()).unwrap();
        rewrite_started_at(
            &fixture.task("expired-running").unwrap(),
            now - chrono::Duration::hours(25),
        );
        fixture.start("young-running", "sleep 60").await;
        collect(&fixture.identity, SystemTime::now(), RETENTION, u64::MAX);
        assert!(fixture.task("expired").is_none());
        assert!(fixture.task("expired-running").is_none());
        assert!(fixture.task("older").is_some());
        assert!(fixture.task("newer").is_some());
        wait_for(|| rustix::process::test_kill_process_group(expired_pid).is_err()).await;
        let total = fixture.identity.background_bytes.load(Ordering::Relaxed);
        collect(&fixture.identity, SystemTime::now(), RETENTION, total - 1);
        assert!(fixture.task("older").is_none());
        assert!(fixture.task("newer").is_some());
        collect(&fixture.identity, SystemTime::now(), RETENTION, 0);
        assert!(fixture.task("newer").is_none());
        let young = fixture.task("young-running").unwrap();
        assert!(matches!(liveness(&young), Liveness::Running(Some(_))));
        assert_eq!(
            fixture
                .run(operation("cleanup", "young-running", json!({})))
                .await,
            Ok(json!({"cleaned": true}))
        );
    }

    #[tokio::test]
    async fn ca_bg_16_start_over_budget_is_refused() {
        let fixture = fixture().await;
        fake_task(&fixture, "huge", Utc::now(), STATE_BUDGET_BYTES + 1);
        gc(&fixture.identity, SystemTime::now());
        assert_eq!(
            fixture
                .run(operation("start", "t14", json!({"cmd": "true"})))
                .await,
            Err("EXECUTOR_BACKGROUND_BUDGET_EXCEEDED")
        );
        assert!(fixture.task("t14").is_none());
    }

    #[tokio::test]
    async fn ca_bg_16_admission_reads_the_measurement_gc_published() {
        let fixture = fixture().await;
        fake_task(&fixture, "huge", Utc::now(), STATE_BUDGET_BYTES + 1);
        assert_eq!(fixture.identity.background_bytes.load(Ordering::Relaxed), 0);
        collect(
            &fixture.identity,
            SystemTime::now(),
            RETENTION,
            STATE_BUDGET_BYTES,
        );
        assert!(
            fixture.identity.background_bytes.load(Ordering::Relaxed) > STATE_BUDGET_BYTES,
            "gc did not publish the over-budget measurement"
        );
        assert_eq!(
            fixture
                .run(operation("start", "over", json!({"cmd": "true"})))
                .await,
            Err("EXECUTOR_BACKGROUND_BUDGET_EXCEEDED")
        );
        rewrite_started_at(
            &fixture.task("huge").unwrap(),
            Utc::now() - chrono::Duration::hours(25),
        );
        gc(&fixture.identity, SystemTime::now());
        assert_eq!(fixture.identity.background_bytes.load(Ordering::Relaxed), 0);
        assert_eq!(
            fixture.start("under", "true").await,
            json!({"started": true})
        );
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn ca_bg_05_transient_scope_moves_the_job_out_of_the_worker_cgroup() {
        let fixture = fixture().await;
        if !matches!(fixture.shim.launch, Launch::Scope(_)) {
            eprintln!("skipped: systemd-run --user --scope is unavailable here");
            return;
        }
        assert_eq!(
            fixture.start("t15", "sleep 30").await,
            json!({"started": true})
        );
        let pid = manifest_pid(&fixture.task("t15").unwrap()).unwrap();
        let job =
            std::fs::read_to_string(format!("/proc/{}/cgroup", pid.as_raw_nonzero())).unwrap();
        let worker = std::fs::read_to_string("/proc/self/cgroup").unwrap();
        assert_ne!(job, worker);
        assert!(job.contains(".scope"));
        assert_eq!(
            fixture.run(operation("cleanup", "t15", json!({}))).await,
            Ok(json!({"cleaned": true}))
        );
    }

    #[tokio::test]
    async fn ca_bg_14_symlink_swapped_after_start_cannot_redirect_the_job() {
        let fixture = fixture().await;
        let real = fixture.work.join("real");
        let other = fixture.work.join("other");
        std::fs::create_dir(&real).unwrap();
        std::fs::create_dir(&other).unwrap();
        let link = fixture.work.join("link");
        std::os::unix::fs::symlink("real", &link).unwrap();
        fixture
            .run(operation(
                "start",
                "t16",
                json!({"cmd": "sleep 1; pwd -P", "working_directory": "link"}),
            ))
            .await
            .unwrap();
        std::fs::remove_file(&link).unwrap();
        std::os::unix::fs::symlink("other", &link).unwrap();
        let (stdout, _, code, _) = fixture.drain("t16").await;
        assert_eq!(code, 0);
        assert_eq!(
            stdout.trim(),
            real.canonicalize().unwrap().to_string_lossy()
        );
    }

    #[tokio::test]
    async fn refused_launch_fails_every_start_and_leaves_no_dir() {
        let mut fixture = fixture().await;
        fixture.shim = Arc::new(
            ShimImage::assemble(shim_binary(), Launch::Refused("no user manager".into())).unwrap(),
        );
        assert_eq!(fixture.shim.refusal(), Some("no user manager"));
        assert_eq!(
            fixture
                .run(operation("start", "t17", json!({"cmd": "true"})))
                .await,
            Err("EXECUTOR_BACKGROUND_START_FAILED")
        );
        assert!(fixture.task("t17").is_none());
    }

    #[test]
    fn launch_mode_refuses_only_inside_a_unit() {
        assert!(matches!(
            launch_mode(Err("probe failed".into()), true),
            Launch::Refused(reason) if reason == "probe failed"
        ));
        assert!(matches!(
            launch_mode(Err("probe failed".into()), false),
            Launch::Direct
        ));
        assert!(matches!(
            launch_mode(Ok(BTreeMap::new()), true),
            Launch::Scope(_)
        ));
    }

    #[tokio::test]
    async fn negative_offsets_are_refused() {
        let fixture = fixture().await;
        fixture.start("t18", "true").await;
        assert_eq!(
            fixture
                .run(operation("tail", "t18", json!({"offset": -1})))
                .await,
            Err("EXECUTOR_BACKGROUND_OFFSET_INVALID")
        );
        assert_eq!(
            fixture
                .run(operation("tail", "t18", json!({"stderr_offset": -5})))
                .await,
            Err("EXECUTOR_BACKGROUND_OFFSET_INVALID")
        );
        assert_eq!(offset_of(None), Ok(0));
        assert_eq!(offset_of(Some(7)), Ok(7));
    }

    #[tokio::test]
    async fn a_running_tail_holds_back_an_incomplete_utf8_sequence() {
        let fixture = fixture().await;
        bg_root(&fixture.identity)
            .unwrap()
            .create_dir("t19")
            .unwrap();
        let task = fixture.task("t19").unwrap();
        task.dir.write(STDOUT_FILE, b"a\xC3").unwrap();
        assert_eq!(
            page(&task, STDOUT_FILE, 0, 1000, true).unwrap(),
            ("a".into(), 1, false)
        );
        task.dir.write(STDOUT_FILE, b"a\xC3\xA9").unwrap();
        assert_eq!(
            page(&task, STDOUT_FILE, 1, 1000, true).unwrap(),
            ("\u{e9}".into(), 3, true)
        );
        task.dir.write(STDOUT_FILE, b"a\xC3").unwrap();
        assert_eq!(
            page(&task, STDOUT_FILE, 0, 1000, false).unwrap(),
            ("a\u{fffd}".into(), 2, true)
        );
        assert_eq!(incomplete_tail(b"abc"), 0);
        assert_eq!(incomplete_tail(b"\xE2\x82"), 2);
        assert_eq!(incomplete_tail(b"\xF0\x9F\x98"), 3);
        assert_eq!(incomplete_tail(b"\xC3\xA9"), 0);
        assert_eq!(incomplete_tail(b"\xA9"), 0);
    }

    fn far_deadline() -> DateTime<Utc> {
        Utc::now() + chrono::Duration::seconds(60)
    }

    #[tokio::test]
    async fn a_waiting_tail_returns_as_soon_as_bytes_arrive() {
        let fixture = fixture().await;
        fixture.start("w1", "sleep 1; echo hi; sleep 30").await;
        let (page, elapsed) = fixture
            .timed_tail("w1", 3, far_deadline(), CancellationToken::new())
            .await;
        assert_eq!(page["stdout_text"], "hi\n");
        assert_eq!(page["exit_code"], Value::Null);
        assert!(elapsed < Duration::from_millis(2000), "took {elapsed:?}");
    }

    #[tokio::test]
    async fn a_waiting_tail_on_a_silent_job_returns_empty_after_the_wait() {
        let fixture = fixture().await;
        fixture.start("w2", "sleep 30").await;
        let (page, elapsed) = fixture
            .timed_tail("w2", 2, far_deadline(), CancellationToken::new())
            .await;
        assert_eq!(page["stdout_text"], "");
        assert_eq!(page["stderr_text"], "");
        assert_eq!(page["exit_code"], Value::Null);
        assert!(elapsed >= Duration::from_secs(2), "took {elapsed:?}");
        assert!(elapsed < Duration::from_millis(3000), "took {elapsed:?}");
    }

    #[tokio::test]
    async fn a_waiting_tail_returns_as_soon_as_the_job_exits() {
        let fixture = fixture().await;
        fixture.start("w3", "sleep 1; exit 7").await;
        let (page, elapsed) = fixture
            .timed_tail("w3", 5, far_deadline(), CancellationToken::new())
            .await;
        assert_eq!(page["exit_code"], 7);
        assert!(elapsed < Duration::from_millis(2000), "took {elapsed:?}");
    }

    #[tokio::test]
    async fn a_cancelled_token_ends_the_wait_early() {
        let fixture = fixture().await;
        fixture.start("w4", "sleep 30").await;
        let cancel = CancellationToken::new();
        let trigger = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(300)).await;
            trigger.cancel();
        });
        let (page, elapsed) = fixture.timed_tail("w4", 10, far_deadline(), cancel).await;
        assert_eq!(page["exit_code"], Value::Null);
        assert!(elapsed < Duration::from_millis(1500), "took {elapsed:?}");
    }

    #[tokio::test]
    async fn a_non_empty_page_is_never_delayed_by_the_wait() {
        let fixture = fixture().await;
        fixture.start("w5", "echo now; sleep 30").await;
        wait_for(|| {
            fixture
                .task("w5")
                .unwrap()
                .dir
                .read_to_string(STDOUT_FILE)
                .is_ok_and(|text| text == "now\n")
        })
        .await;
        let (page, elapsed) = fixture
            .timed_tail("w5", 20, far_deadline(), CancellationToken::new())
            .await;
        assert_eq!(page["stdout_text"], "now\n");
        assert!(elapsed < Duration::from_millis(500), "took {elapsed:?}");
    }

    #[tokio::test]
    async fn the_wait_stops_a_margin_before_the_envelope_deadline() {
        let fixture = fixture().await;
        fixture.start("w6", "sleep 30").await;
        let deadline = Utc::now() + chrono::Duration::seconds(6);
        let (page, elapsed) = fixture
            .timed_tail("w6", 20, deadline, CancellationToken::new())
            .await;
        assert_eq!(page["exit_code"], Value::Null);
        assert!(elapsed >= Duration::from_millis(800), "took {elapsed:?}");
        assert!(elapsed < Duration::from_millis(2500), "took {elapsed:?}");
    }

    #[test]
    fn tail_wait_is_bounded_by_the_request_and_the_deadline_margin() {
        assert_eq!(tail_wait(None, far_deadline()), Duration::ZERO);
        assert_eq!(tail_wait(Some(-3), far_deadline()), Duration::ZERO);
        assert_eq!(tail_wait(Some(20), far_deadline()), Duration::from_secs(20));
        assert_eq!(
            tail_wait(Some(20), Utc::now() + chrono::Duration::seconds(3)),
            Duration::ZERO
        );
        assert_eq!(
            tail_wait(Some(20), Utc::now() - chrono::Duration::seconds(3)),
            Duration::ZERO
        );
    }

    #[test]
    fn fit_shrinks_to_the_encoded_budget_on_a_char_boundary() {
        let raw: Vec<u8> = "é\0".repeat(1000).into_bytes();
        let (text, consumed) = fit(&raw, 1000);
        assert!(encoded_len(&text) <= 1000);
        assert!(consumed > 0 && std::str::from_utf8(&raw[..consumed]).is_ok());
        assert_eq!(fit(b"", 10), (String::new(), 0));
    }
}
