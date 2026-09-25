use std::io::Write;
use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use cap_std::fs::{Dir, OpenOptions, OpenOptionsExt};
use cloudthinker_client::auth::worker_store::write_private;
use cloudthinker_client::{CtError, CtResult};
use rustix::fs::FlockOperation;
use rustix::process::{Signal, fchdir, getpgrp, getpid, kill_process_group, setsid};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::{Child, Command};
use tokio::signal::unix::{Signal as SignalStream, SignalKind, signal};
use tokio::task::JoinHandle;

use super::worker::BgShimArgs;
use crate::worker::background::{
    CANCEL_GRACE, COMMAND_FILE, EXIT_FILE, LOCK_FILE, MANIFEST_FILE, STDERR_FILE, STDOUT_FILE,
    STREAM_CAP_BYTES, TRUNCATED_FILE, TaskManifest,
};

const DRAIN_IDLE: Duration = Duration::from_millis(500);
const DRAIN_CEILING: Duration = CANCEL_GRACE;
const STRIPPED_ENV: [&str; 2] = ["XDG_RUNTIME_DIR", "DBUS_SESSION_BUS_ADDRESS"];
const READ_CHUNK: usize = 65536;
const SPAWN_FAILED_EXIT: i32 = 127;
const KILLED_EXIT: i32 = 128 + 9;

enum Outcome {
    Exited(i32),
    Unresponsive,
}

pub async fn run(args: BgShimArgs) -> CtResult<()> {
    let mut term = signal(SignalKind::terminate()).map_err(|_| shim_error("no SIGTERM handler"))?;
    setsid().map_err(|_| shim_error("no new session"))?;
    let dir = Dir::open_ambient_dir(&args.task_dir, cap_std::ambient_authority())
        .map_err(|_| shim_error("task directory unreachable"))?;
    let _lock = hold_lock(&dir)?;
    write_manifest(&dir, args.timeout_secs)?;
    fchdir(std::io::stdin()).map_err(|_| shim_error("working directory unreachable"))?;
    let recorder = Arc::new(Recorder::new());
    let mut child = match spawn_child(&args.task_dir) {
        Ok(child) => child,
        Err(_) => return write_exit(&dir, SPAWN_FAILED_EXIT),
    };
    let stdout = child.stdout.take().map(|reader| {
        tokio::spawn(copy(
            reader,
            StreamWriter::open(&dir, STDOUT_FILE),
            recorder.clone(),
        ))
    });
    let stderr = child.stderr.take().map(|reader| {
        tokio::spawn(copy(
            reader,
            StreamWriter::open(&dir, STDERR_FILE),
            recorder.clone(),
        ))
    });
    let mut streams: Vec<JoinHandle<()>> = stdout.into_iter().chain(stderr).collect();
    match wait_with_timeout(
        &mut child,
        Duration::from_secs(args.timeout_secs),
        &mut term,
    )
    .await
    {
        Outcome::Exited(code) => {
            let _ = tokio::time::timeout(DRAIN_CEILING, drain(&mut streams, &recorder)).await;
            recorder.recording.store(false, Ordering::SeqCst);
            write_exit(&dir, code)?;
            tokio::select! {
                _ = join_all(&mut streams) => {}
                _ = term.recv() => {}
            }
            Ok(())
        }
        Outcome::Unresponsive => {
            recorder.recording.store(false, Ordering::SeqCst);
            let written = write_exit(&dir, KILLED_EXIT);
            let _ = kill_process_group(getpgrp(), Signal::KILL);
            written
        }
    }
}

async fn drain(streams: &mut Vec<JoinHandle<()>>, recorder: &Recorder) {
    let mut seen = recorder.progress.load(Ordering::SeqCst);
    while tokio::time::timeout(DRAIN_IDLE, join_all(streams))
        .await
        .is_err()
    {
        let progress = recorder.progress.load(Ordering::SeqCst);
        if progress == seen {
            return;
        }
        seen = progress;
    }
}

async fn join_all(streams: &mut Vec<JoinHandle<()>>) {
    while let Some(stream) = streams.last_mut() {
        let _ = stream.await;
        streams.pop();
    }
}

fn hold_lock(dir: &Dir) -> CtResult<cap_std::fs::File> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).mode(0o600);
    let file = dir
        .open_with(LOCK_FILE, &options)
        .map_err(|_| shim_error("task lock unavailable"))?;
    rustix::fs::flock(&file, FlockOperation::LockExclusive)
        .map_err(|_| shim_error("task lock already held"))?;
    Ok(file)
}

fn write_manifest(dir: &Dir, timeout_secs: u64) -> CtResult<()> {
    let manifest = TaskManifest {
        pid: u32::try_from(getpid().as_raw_nonzero().get())
            .map_err(|_| shim_error("supervisor pid out of range"))?,
        started_at: chrono::Utc::now().to_rfc3339(),
        timeout_secs,
    };
    write_private(
        dir,
        MANIFEST_FILE,
        &serde_json::to_vec(&manifest).map_err(|_| shim_error("manifest could not be encoded"))?,
    )
}

fn spawn_child(task_dir: &Path) -> std::io::Result<Child> {
    let mut command = Command::new("/bin/bash");
    for key in STRIPPED_ENV {
        command.env_remove(key);
    }
    command
        .arg(task_dir.join(COMMAND_FILE))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
}

async fn wait_with_timeout(
    child: &mut Child,
    timeout: Duration,
    term: &mut SignalStream,
) -> Outcome {
    tokio::select! {
        status = child.wait() => return Outcome::Exited(exit_code(status)),
        _ = term.recv() => {}
        _ = tokio::time::sleep(timeout) => {}
    }
    let _ = kill_process_group(getpgrp(), Signal::TERM);
    tokio::select! {
        status = child.wait() => Outcome::Exited(exit_code(status)),
        _ = tokio::time::sleep(CANCEL_GRACE) => Outcome::Unresponsive,
    }
}

fn exit_code(status: std::io::Result<std::process::ExitStatus>) -> i32 {
    match status {
        Ok(status) => status
            .code()
            .unwrap_or_else(|| 128 + status.signal().unwrap_or(0)),
        Err(_) => -1,
    }
}

fn write_exit(dir: &Dir, code: i32) -> CtResult<()> {
    write_private(dir, EXIT_FILE, code.to_string().as_bytes())
}

struct Recorder {
    recording: AtomicBool,
    progress: AtomicU64,
}

impl Recorder {
    fn new() -> Self {
        Self {
            recording: AtomicBool::new(true),
            progress: AtomicU64::new(0),
        }
    }
}

struct StreamWriter {
    file: Option<cap_std::fs::File>,
    dir: Option<Dir>,
    written: u64,
}

impl StreamWriter {
    fn open(dir: &Dir, name: &str) -> Self {
        let mut options = OpenOptions::new();
        options.write(true).create(true).truncate(true).mode(0o600);
        Self {
            file: dir.open_with(name, &options).ok(),
            dir: dir.try_clone().ok(),
            written: 0,
        }
    }

    fn append(&mut self, bytes: &[u8]) {
        let Some(file) = self.file.as_mut() else {
            return;
        };
        let room =
            usize::try_from(STREAM_CAP_BYTES.saturating_sub(self.written)).unwrap_or(usize::MAX);
        let take = bytes.len().min(room);
        if file.write_all(&bytes[..take]).is_err() {
            self.seal();
            return;
        }
        self.written += take as u64;
        if take < bytes.len() {
            self.seal();
        }
    }

    fn seal(&mut self) {
        self.file = None;
        if let Some(dir) = self.dir.take() {
            let _ = dir.create(TRUNCATED_FILE);
        }
    }
}

async fn copy(
    mut reader: impl AsyncRead + Unpin,
    mut writer: StreamWriter,
    recorder: Arc<Recorder>,
) {
    let mut buffer = vec![0; READ_CHUNK];
    loop {
        match reader.read(&mut buffer).await {
            Ok(0) | Err(_) => return,
            Ok(count) => {
                recorder.progress.fetch_add(1, Ordering::SeqCst);
                if recorder.recording.load(Ordering::SeqCst) {
                    writer.append(&buffer[..count]);
                }
            }
        }
    }
}

fn shim_error(reason: &str) -> CtError {
    CtError::Store(format!(
        "background task supervisor could not start: {reason}"
    ))
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn ca_bg_10_writer_seals_on_a_write_error_and_keeps_the_exit_path_open() {
        let temporary = tempfile::tempdir().unwrap();
        let dir = Dir::open_ambient_dir(temporary.path(), cap_std::ambient_authority()).unwrap();
        let full = std::fs::OpenOptions::new()
            .write(true)
            .open("/dev/full")
            .unwrap();
        let mut writer = StreamWriter {
            file: Some(cap_std::fs::File::from_std(full)),
            dir: dir.try_clone().ok(),
            written: 0,
        };
        writer.append(b"lost");
        assert!(writer.file.is_none());
        assert!(dir.exists(TRUNCATED_FILE));
        writer.append(b"ignored");
        assert_eq!(writer.written, 0);
        write_exit(&dir, 0).unwrap();
        assert_eq!(dir.read_to_string(EXIT_FILE).unwrap(), "0");
    }
}
