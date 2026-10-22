use std::ffi::OsString;
use std::fmt;
use std::fs::{File, OpenOptions, Permissions};
use std::io::{Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use cloudthinker_client::CtError;
use cloudthinker_client::auth::worker_store::write_private;
use portable_pty::{CommandBuilder, MasterPty, PtySize};
use rustix::fs::FlockOperation;
use rustix::process::{Pid, Signal};
use tokio::io::AsyncWriteExt;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{UnixListener, UnixStream};
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio::time::{Instant, MissedTickBehavior};

use super::catalog::{Meta, expand, home_folder, tilde};
use super::paths::{HOST_PID_ENV, PASTE_MAX_AGE_SECS, READY_ENV, REMOTE_ENV, RemotePaths};
use super::wire::{
    AttachState, Frame, Hello, PROTOCOL_VERSION, Size, Upload, UploadKind, Welcome, output_frames,
    read_frame, write_frame,
};
use crate::engine::exit::ExitCode;

const HELLO_WAIT: Duration = Duration::from_secs(10);
const FIRST_ATTACH_WAIT: Duration = Duration::from_secs(30);
const PING_EVERY: Duration = Duration::from_secs(5);
const JIGGLE_WAIT: Duration = Duration::from_millis(50);
const WRITE_STALL: Duration = Duration::from_secs(15);
const FLUSH_WAIT: Duration = Duration::from_secs(2);
const DRAIN_WAIT: Duration = Duration::from_millis(200);
const LOG_CAP_BYTES: u64 = 1024 * 1024;
const PTY_READ_CHUNK: usize = 16 * 1024;
const OUTPUT_QUEUE: usize = 256;
const CLIENT_QUEUE: usize = 512;
const EVENT_QUEUE: usize = 64;
const PASTE_NAME_MAX: usize = 80;
const NO_UPDATE_ENV: &str = "CLOUDTHINKER_NO_UPDATE_CHECK";
const SSH_AGENT_ENV: &str = "SSH_AUTH_SOCK";
const PASTE_START: &[u8] = b"\x1b[200~";
const PASTE_END: &[u8] = b"\x1b[201~";
const FALLBACK_SIZE: Size = Size { rows: 24, cols: 80 };

pub const TERMINAL_ENV: [&str; 5] = ["TERM", "COLORTERM", "TERM_PROGRAM", "LC_TERMINAL", "LANG"];

pub async fn run(name: &str) -> ExitCode {
    let paths = match RemotePaths::for_name(name) {
        Ok(paths) => paths,
        Err(_) => return ExitCode::JobFailed,
    };
    let log = Log::new(&paths);
    match serve(&paths, &log).await {
        Ok(()) => ExitCode::Ok,
        Err(error) => {
            log.event(&format!("host failed: {error}"));
            ExitCode::JobFailed
        }
    }
}

pub(crate) fn try_lock(path: &Path) -> std::io::Result<Option<File>> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(path)?;
    match rustix::fs::flock(&file, FlockOperation::NonBlockingLockExclusive) {
        Ok(()) => Ok(Some(file)),
        Err(error) if error == rustix::io::Errno::WOULDBLOCK => Ok(None),
        Err(error) => Err(error.into()),
    }
}

async fn serve(paths: &RemotePaths, log: &Log) -> Result<(), HostError> {
    let _ = rustix::process::setsid();
    let mut hangup =
        signal(SignalKind::hangup()).map_err(|_| HostError::Setup("no SIGHUP handler"))?;
    let mut detach =
        signal(SignalKind::user_defined1()).map_err(|_| HostError::Setup("no SIGUSR1 handler"))?;
    paths.open_root().map_err(HostError::Paths)?;
    let Some(_lock) = try_lock(&paths.lock()).map_err(|_| HostError::Setup("lock unavailable"))?
    else {
        return Ok(());
    };
    let _ = std::fs::remove_file(paths.socket());
    let listener =
        UnixListener::bind(paths.socket()).map_err(|_| HostError::Setup("socket bind failed"))?;
    std::fs::set_permissions(paths.socket(), Permissions::from_mode(0o600))
        .map_err(|_| HostError::Setup("socket mode could not be set"))?;
    log.event("host start");
    prune_pastes(paths);
    let (events, inbox) = mpsc::channel(EVENT_QUEUE);
    let outcome = Host::new(paths, log, events)
        .serve(&listener, &mut hangup, &mut detach, inbox)
        .await;
    cleanup(paths);
    log.event("host stop");
    outcome
}

fn cleanup(paths: &RemotePaths) {
    let _ = std::fs::remove_file(paths.socket());
    let _ = std::fs::remove_file(paths.ready());
    let _ = std::fs::remove_file(paths.agent_socket());
    let _ = std::fs::remove_file(paths.meta());
}

#[derive(Debug)]
enum HostError {
    Setup(&'static str),
    Paths(CtError),
    Agent(String),
}

impl fmt::Display for HostError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Setup(reason) => f.write_str(reason),
            Self::Paths(error) => write!(f, "{error}"),
            Self::Agent(reason) => f.write_str(reason),
        }
    }
}

enum Event {
    Joined { stream: UnixStream, hello: Hello },
    Frame { id: u64, frame: Frame },
    Left { id: u64 },
}

enum Next {
    Continue,
    Started(AgentIo),
    Stop,
}

struct AgentIo {
    output: mpsc::Receiver<Vec<u8>>,
    exited: oneshot::Receiver<i32>,
}

struct Agent {
    master: Box<dyn MasterPty + Send>,
    input: std::sync::mpsc::Sender<Vec<u8>>,
    pid: Option<u32>,
    size: Size,
    started_at: u64,
    cwd: String,
}

struct Host<'a> {
    paths: &'a RemotePaths,
    log: &'a Log,
    agent: Option<Agent>,
    client: Option<Client>,
    next_id: u64,
    session: u64,
    jiggle: Option<(Instant, Size)>,
    events: mpsc::Sender<Event>,
}

impl<'a> Host<'a> {
    fn new(paths: &'a RemotePaths, log: &'a Log, events: mpsc::Sender<Event>) -> Self {
        Self {
            paths,
            log,
            agent: None,
            client: None,
            next_id: 0,
            session: 0,
            jiggle: None,
            events,
        }
    }

    async fn serve(
        mut self,
        listener: &UnixListener,
        hangup: &mut tokio::signal::unix::Signal,
        detach: &mut tokio::signal::unix::Signal,
        mut inbox: mpsc::Receiver<Event>,
    ) -> Result<(), HostError> {
        let first_attach_deadline = Instant::now() + FIRST_ATTACH_WAIT;
        let mut output: Option<mpsc::Receiver<Vec<u8>>> = None;
        let mut exited: Option<oneshot::Receiver<i32>> = None;
        let mut ping = tokio::time::interval(PING_EVERY);
        ping.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            let jiggle_at = self.jiggle.map(|(at, _)| at);
            tokio::select! {
                accepted = listener.accept() => {
                    if let Ok((stream, _)) = accepted {
                        tokio::spawn(handshake(stream, self.events.clone()));
                    }
                }
                Some(event) = inbox.recv() => match self.on_event(event) {
                    Next::Continue => {}
                    Next::Started(io) => {
                        output = Some(io.output);
                        exited = Some(io.exited);
                    }
                    Next::Stop => {
                        if let Some(client) = self.client.take() {
                            client.flush().await;
                        }
                        return Ok(());
                    }
                },
                chunk = recv_or_pending(&mut output) => match chunk {
                    Some(bytes) => self.forward(&bytes),
                    None => output = None,
                },
                code = exit_or_pending(&mut exited) => {
                    self.finish(code, output.take()).await;
                    return Ok(());
                }
                _ = ping.tick() => {
                    self.send(Frame::Ping);
                }
                () = sleep_or_pending(jiggle_at) => self.restore_size(),
                _ = hangup.recv() => {}
                _ = detach.recv() => self.detach_by_agent(),
                () = tokio::time::sleep_until(first_attach_deadline), if self.agent.is_none() => return Ok(()),
            }
        }
    }

    fn on_event(&mut self, event: Event) -> Next {
        match event {
            Event::Joined { stream, hello } => self.attach(stream, &hello),
            Event::Frame { id, frame } if self.is_current(id) => {
                self.on_frame(frame);
                Next::Continue
            }
            Event::Left { id } if self.is_current(id) => {
                self.client = None;
                self.log.event("detach");
                Next::Continue
            }
            Event::Frame { .. } | Event::Left { .. } => Next::Continue,
        }
    }

    fn detach_by_agent(&mut self) {
        let Some(client) = self.client.take() else {
            return;
        };
        client.send(Frame::Detach);
        self.log.event("detach (agent)");
        tokio::spawn(client.flush());
    }

    fn is_current(&self, id: u64) -> bool {
        self.client.as_ref().is_some_and(|client| client.id == id)
    }

    fn attach(&mut self, stream: UnixStream, hello: &Hello) -> Next {
        prune_pastes(self.paths);
        if let Some(resumed) = hello.resume {
            if self.agent.is_none() {
                self.next_id += 1;
                let client = Client::spawn(stream, self.next_id, self.events.clone());
                client.send(Frame::Exit(last_exit(self.paths)));
                self.log.event("resume after agent exit");
                self.client = Some(client);
                return Next::Stop;
            }
            if resumed != self.session {
                self.next_id += 1;
                let client = Client::spawn(stream, self.next_id, self.events.clone());
                client.send(Frame::Replaced);
                self.log.event("resume refused: replaced");
                tokio::spawn(client.flush());
                return Next::Continue;
            }
        }
        if let Some(previous) = self.client.take() {
            previous.send(Frame::Replaced);
            self.log.event("replaced");
        }
        self.next_id += 1;
        self.session = new_session(self.next_id);
        self.client = Some(Client::spawn(stream, self.next_id, self.events.clone()));
        self.log.event("attach");
        if self.agent.is_some() {
            self.reattach(hello);
            return Next::Continue;
        }
        let _ = std::fs::remove_file(self.paths.exit());
        let home = home_folder();
        let (cwd, moved) = resolve_cwd(hello.cwd.as_deref(), &home);
        match spawn_agent(self.paths, hello, &cwd, &home) {
            Ok((agent, io)) => {
                match agent.pid {
                    Some(pid) => self.log.event(&format!("agent start pid {pid}")),
                    None => self.log.event("agent start"),
                }
                if write_meta(self.paths, &cwd, agent.started_at).is_err() {
                    self.log.event("meta write failed");
                }
                let frame = welcome(AttachState::Started, &agent, self.session);
                self.agent = Some(agent);
                self.send(frame);
                if moved {
                    self.send(Frame::Notice(format!(
                        "the folder {} is not there; the agent started in {}",
                        hello.cwd.as_deref().unwrap_or_default(),
                        home.display()
                    )));
                }
                Next::Started(io)
            }
            Err(error) => {
                self.log.event("agent start failed");
                self.send(Frame::Notice(format!("the agent could not start: {error}")));
                self.send(Frame::Exit(1));
                Next::Stop
            }
        }
    }

    fn reattach(&mut self, hello: &Hello) {
        if let Some(frame) = self
            .agent
            .as_ref()
            .map(|agent| welcome(AttachState::Attached, agent, self.session))
        {
            self.send(frame);
        }
        if hello.resume.is_none() && (hello.cwd.is_some() || !hello.args.is_empty()) {
            self.send(Frame::Notice(
                "the agent is already running; --cwd and agent arguments apply only when it starts"
                    .into(),
            ));
        }
        self.resize(hello.size);
        let Some(agent) = &self.agent else {
            return;
        };
        let signalled = agent
            .pid
            .filter(|pid| ready_pid(&self.paths.ready()) == Some(*pid))
            .is_some_and(signal_redraw);
        if !signalled {
            let size = agent.size;
            let squeezed = Size {
                rows: if size.rows > 1 {
                    size.rows - 1
                } else {
                    size.rows + 1
                },
                cols: size.cols,
            };
            if let Some(pty) = pty_size(squeezed) {
                let _ = agent.master.resize(pty);
                self.jiggle = Some((Instant::now() + JIGGLE_WAIT, size));
            }
        }
    }

    fn restore_size(&mut self) {
        if let (Some((_, size)), Some(agent)) = (self.jiggle.take(), &self.agent)
            && let Some(pty) = pty_size(size)
        {
            let _ = agent.master.resize(pty);
        }
    }

    fn resize(&mut self, size: Size) {
        let Some(pty) = pty_size(size) else {
            return;
        };
        if let Some(agent) = &mut self.agent {
            agent.size = size;
            self.jiggle = None;
            let _ = agent.master.resize(pty);
        }
    }

    fn on_frame(&mut self, frame: Frame) {
        match frame {
            Frame::Input(bytes) => self.type_into_agent(bytes),
            Frame::Resize(size) => self.resize(size),
            Frame::Upload(upload) => self.upload(&upload),
            Frame::Hello(_)
            | Frame::Welcome(_)
            | Frame::Output(_)
            | Frame::Ping
            | Frame::Notice(_)
            | Frame::Exit(_)
            | Frame::Replaced
            | Frame::Query(_)
            | Frame::Catalog(_)
            | Frame::Complete(_)
            | Frame::Completions(_)
            | Frame::Detach => {}
        }
    }

    fn type_into_agent(&self, bytes: Vec<u8>) {
        if let Some(agent) = &self.agent {
            let _ = agent.input.send(bytes);
        }
    }

    fn upload(&mut self, upload: &Upload) {
        match save_upload(self.paths, upload) {
            Ok(path) => {
                self.log.event(&format!("upload {}", upload.bytes.len()));
                self.type_into_agent(bracketed_paste(&path));
            }
            Err(error) => {
                self.send(Frame::Notice(format!(
                    "the pasted file could not be saved: {error}"
                )));
            }
        }
    }

    fn forward(&mut self, bytes: &[u8]) {
        if self.client.is_none() {
            return;
        }
        for frame in output_frames(bytes) {
            if !self.send(frame) {
                return;
            }
        }
    }

    fn send(&mut self, frame: Frame) -> bool {
        let Some(client) = &self.client else {
            return false;
        };
        if client.send(frame) {
            return true;
        }
        self.client = None;
        self.log.event("detach");
        false
    }

    async fn finish(&mut self, code: i32, output: Option<mpsc::Receiver<Vec<u8>>>) {
        if let Some(mut output) = output {
            let deadline = Instant::now() + DRAIN_WAIT;
            while let Ok(Some(bytes)) = tokio::time::timeout_at(deadline, output.recv()).await {
                self.forward(&bytes);
            }
        }
        self.log.event(&format!("agent exit {code}"));
        if save_exit(self.paths, code).is_err() {
            self.log.event("exit code write failed");
        }
        self.send(Frame::Exit(code));
        if let Some(client) = self.client.take() {
            client.flush().await;
        }
        self.agent = None;
    }
}

fn welcome(state: AttachState, agent: &Agent, session: u64) -> Frame {
    Frame::Welcome(Welcome {
        version: PROTOCOL_VERSION,
        cli_version: env!("CARGO_PKG_VERSION").into(),
        state,
        started_at: agent.started_at,
        cwd: agent.cwd.clone(),
        session,
    })
}

fn new_session(id: u64) -> u64 {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_nanos());
    u64::try_from(nanos & u128::from(u64::MAX)).unwrap_or_default() ^ id
}

fn save_exit(paths: &RemotePaths, code: i32) -> Result<(), CtError> {
    let dir = paths.open_root()?;
    let file = paths.exit();
    let name = file
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| CtError::Usage("the agent name is not a file name".into()))?;
    write_private(&dir, name, code.to_string().as_bytes())
}

fn last_exit(paths: &RemotePaths) -> i32 {
    std::fs::read_to_string(paths.exit())
        .ok()
        .and_then(|text| text.trim().parse().ok())
        .unwrap_or(0)
}

fn write_meta(paths: &RemotePaths, cwd: &Path, started: u64) -> Result<(), CtError> {
    let dir = paths.open_root()?;
    let file = paths.meta();
    let name = file
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| CtError::Usage("the agent name is not a file name".into()))?;
    let meta = Meta {
        cwd: absolute_display(cwd),
        started,
    };
    write_private(&dir, name, meta.render().as_bytes())
}

fn absolute_display(path: &Path) -> String {
    std::path::absolute(path)
        .unwrap_or_else(|_| path.to_path_buf())
        .display()
        .to_string()
}

fn now_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

async fn handshake(mut stream: UnixStream, events: mpsc::Sender<Event>) {
    let first = tokio::time::timeout(HELLO_WAIT, read_frame(&mut stream)).await;
    let refusal = match first {
        Ok(Ok(Some(Frame::Hello(hello)))) if hello.version == PROTOCOL_VERSION => {
            let _ = events.send(Event::Joined { stream, hello }).await;
            return;
        }
        Ok(Ok(Some(Frame::Hello(hello)))) => version_notice(hello.version),
        Ok(Ok(Some(_))) => {
            "the remote side expected a hello first; update cloudthinker on both machines".into()
        }
        Ok(Err(error)) => format!(
            "the remote side could not read the hello ({error}); update cloudthinker on both machines"
        ),
        Ok(Ok(None)) | Err(_) => return,
    };
    let _ = tokio::time::timeout(
        FLUSH_WAIT,
        write_frame(&mut stream, &Frame::Notice(refusal)),
    )
    .await;
    let _ = stream.shutdown().await;
}

fn version_notice(client: u16) -> String {
    format!(
        "this machine's cloudthinker {} speaks remote protocol {PROTOCOL_VERSION}, and yours speaks {client}; update cloudthinker on both machines",
        env!("CARGO_PKG_VERSION")
    )
}

struct Client {
    id: u64,
    frames: mpsc::Sender<Frame>,
    reader: JoinHandle<()>,
    writer: Option<JoinHandle<()>>,
}

impl Client {
    fn spawn(stream: UnixStream, id: u64, events: mpsc::Sender<Event>) -> Self {
        let (reader, writer) = stream.into_split();
        let (frames, queue) = mpsc::channel(CLIENT_QUEUE);
        Self {
            id,
            frames,
            reader: tokio::spawn(read_client(reader, id, events.clone())),
            writer: Some(tokio::spawn(write_client(writer, queue, id, events))),
        }
    }

    fn send(&self, frame: Frame) -> bool {
        self.frames.try_send(frame).is_ok()
    }

    async fn flush(mut self) {
        let writer = self.writer.take();
        drop(self);
        if let Some(writer) = writer {
            let _ = tokio::time::timeout(FLUSH_WAIT, writer).await;
        }
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        self.reader.abort();
    }
}

async fn read_client(mut reader: OwnedReadHalf, id: u64, events: mpsc::Sender<Event>) {
    loop {
        let event = match read_frame(&mut reader).await {
            Ok(Some(frame)) => Event::Frame { id, frame },
            Ok(None) | Err(_) => Event::Left { id },
        };
        let left = matches!(event, Event::Left { .. });
        if events.send(event).await.is_err() || left {
            return;
        }
    }
}

async fn write_client(
    mut writer: OwnedWriteHalf,
    mut queue: mpsc::Receiver<Frame>,
    id: u64,
    events: mpsc::Sender<Event>,
) {
    while let Some(frame) = queue.recv().await {
        if !matches!(
            tokio::time::timeout(WRITE_STALL, write_frame(&mut writer, &frame)).await,
            Ok(Ok(()))
        ) {
            let _ = events.send(Event::Left { id }).await;
            return;
        }
    }
    let _ = writer.shutdown().await;
}

async fn recv_or_pending(receiver: &mut Option<mpsc::Receiver<Vec<u8>>>) -> Option<Vec<u8>> {
    match receiver {
        Some(receiver) => receiver.recv().await,
        None => std::future::pending().await,
    }
}

async fn exit_or_pending(receiver: &mut Option<oneshot::Receiver<i32>>) -> i32 {
    match receiver {
        Some(receiver) => receiver.await.unwrap_or(1),
        None => std::future::pending().await,
    }
}

async fn sleep_or_pending(at: Option<Instant>) {
    match at {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
}

fn spawn_agent(
    paths: &RemotePaths,
    hello: &Hello,
    cwd: &Path,
    home: &Path,
) -> Result<(Agent, AgentIo), HostError> {
    let size = if pty_size(hello.size).is_some() {
        hello.size
    } else {
        FALLBACK_SIZE
    };
    let pair = portable_pty::native_pty_system()
        .openpty(to_pty(size))
        .map_err(|error| HostError::Agent(format!("no terminal could be opened: {error}")))?;
    let program = std::env::current_exe().map_err(|error| {
        HostError::Agent(format!("the cloudthinker program was not found: {error}"))
    })?;
    let _ = std::fs::remove_file(paths.ready());
    let child = pair
        .slave
        .spawn_command(agent_command(paths, hello, &program, cwd))
        .map_err(|error| HostError::Agent(error.to_string()))?;
    drop(pair.slave);
    let reader = pair
        .master
        .try_clone_reader()
        .map_err(|error| HostError::Agent(error.to_string()))?;
    let writer = pair
        .master
        .take_writer()
        .map_err(|error| HostError::Agent(error.to_string()))?;
    let pid = child.process_id();
    let started_at = now_seconds();
    let (output_sender, output) = mpsc::channel(OUTPUT_QUEUE);
    let (input, input_queue) = std::sync::mpsc::channel();
    let (exit_sender, exited) = oneshot::channel();
    std::thread::spawn(move || drain_pty(reader, &output_sender));
    std::thread::spawn(move || feed_pty(writer, &input_queue));
    std::thread::spawn(move || wait_agent(child, exit_sender));
    Ok((
        Agent {
            master: pair.master,
            input,
            pid,
            size,
            started_at,
            cwd: tilde(cwd, home),
        },
        AgentIo { output, exited },
    ))
}

fn agent_command(paths: &RemotePaths, hello: &Hello, program: &Path, cwd: &Path) -> CommandBuilder {
    let mut command = CommandBuilder::new(program);
    command.arg("agent");
    command.args(&hello.args);
    command.cwd(cwd);
    for (key, value) in hello
        .env
        .iter()
        .filter(|(key, _)| TERMINAL_ENV.contains(&key.as_str()))
    {
        command.env(key, value);
    }
    command.env(REMOTE_ENV, "1");
    command.env(READY_ENV, paths.ready());
    command.env(NO_UPDATE_ENV, "1");
    command.env(HOST_PID_ENV, std::process::id().to_string());
    if std::env::var_os(SSH_AGENT_ENV).is_some() {
        command.env(SSH_AGENT_ENV, paths.agent_socket());
    }
    command
}

fn drain_pty(mut reader: Box<dyn Read + Send>, output: &mpsc::Sender<Vec<u8>>) {
    let mut buffer = vec![0u8; PTY_READ_CHUNK];
    loop {
        match reader.read(&mut buffer) {
            Ok(0) | Err(_) => return,
            Ok(count) => {
                if output.blocking_send(buffer[..count].to_vec()).is_err() {
                    return;
                }
            }
        }
    }
}

fn feed_pty(mut writer: Box<dyn Write + Send>, input: &std::sync::mpsc::Receiver<Vec<u8>>) {
    while let Ok(bytes) = input.recv() {
        if writer
            .write_all(&bytes)
            .and_then(|()| writer.flush())
            .is_err()
        {
            return;
        }
    }
}

fn wait_agent(mut child: Box<dyn portable_pty::Child + Send + Sync>, exited: oneshot::Sender<i32>) {
    let code = child
        .wait()
        .map_or(1, |status| i32::try_from(status.exit_code()).unwrap_or(1));
    let _ = exited.send(code);
}

fn signal_redraw(pid: u32) -> bool {
    i32::try_from(pid)
        .ok()
        .and_then(Pid::from_raw)
        .is_some_and(|pid| rustix::process::kill_process(pid, Signal::USR2).is_ok())
}

fn ready_pid(path: &Path) -> Option<u32> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| parse_pid(&text))
}

fn parse_pid(text: &str) -> Option<u32> {
    text.trim().parse().ok().filter(|pid| *pid > 0)
}

fn pty_size(size: Size) -> Option<PtySize> {
    (size.rows > 0 && size.cols > 0).then(|| to_pty(size))
}

fn to_pty(size: Size) -> PtySize {
    PtySize {
        rows: size.rows,
        cols: size.cols,
        pixel_width: 0,
        pixel_height: 0,
    }
}

fn resolve_cwd(requested: Option<&str>, home: &Path) -> (PathBuf, bool) {
    let Some(requested) = requested.filter(|requested| !requested.is_empty()) else {
        return (home.to_path_buf(), false);
    };
    let candidate = expand(requested, home);
    if candidate.is_dir() {
        (candidate, false)
    } else {
        (home.to_path_buf(), true)
    }
}

fn save_upload(paths: &RemotePaths, upload: &Upload) -> Result<PathBuf, CtError> {
    let dir = paths.open_pastes()?;
    let name = paste_name(
        now_seconds(),
        rand::random::<u16>(),
        &upload.name,
        upload.kind,
    );
    write_private(&dir, &name, &upload.bytes)?;
    let path = paths.pastes().join(name);
    Ok(std::path::absolute(&path).unwrap_or(path))
}

fn paste_name(seconds: u64, tag: u16, name: &str, kind: UploadKind) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or_default();
    let mut clean: String = base
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-') {
                character
            } else {
                '_'
            }
        })
        .take(PASTE_NAME_MAX)
        .collect();
    if clean.chars().all(|character| character == '.') {
        clean = match kind {
            UploadKind::Image => "clipboard.png".into(),
            UploadKind::File => "file".into(),
        };
    }
    format!("{seconds}-{tag:04x}-{clean}")
}

fn bracketed_paste(path: &Path) -> Vec<u8> {
    let mut bytes = PASTE_START.to_vec();
    bytes.extend_from_slice(path.as_os_str().as_encoded_bytes());
    bytes.extend_from_slice(PASTE_END);
    bytes
}

fn prune_pastes(paths: &RemotePaths) {
    let Ok(dir) = paths.open_pastes() else {
        return;
    };
    let Ok(entries) = dir.entries() else {
        return;
    };
    let now = SystemTime::now();
    for entry in entries.flatten() {
        let stale = entry
            .metadata()
            .ok()
            .filter(cap_std::fs::Metadata::is_file)
            .and_then(|metadata| metadata.modified().ok())
            .is_some_and(|modified| {
                now.duration_since(modified.into_std())
                    .is_ok_and(|age| age.as_secs() > PASTE_MAX_AGE_SECS)
            });
        if stale {
            let _ = dir.remove_file(entry.file_name());
        }
    }
}

struct Log {
    path: PathBuf,
    rotated: PathBuf,
}

impl Log {
    fn new(paths: &RemotePaths) -> Self {
        let path = paths.log();
        let mut rotated = OsString::from(path.as_os_str());
        rotated.push(".1");
        Self {
            path,
            rotated: PathBuf::from(rotated),
        }
    }

    fn event(&self, event: &str) {
        if std::fs::metadata(&self.path).is_ok_and(|metadata| metadata.len() >= LOG_CAP_BYTES) {
            let _ = std::fs::rename(&self.path, &self.rotated);
        }
        let file = OpenOptions::new()
            .append(true)
            .create(true)
            .mode(0o600)
            .open(&self.path);
        if let Ok(mut file) = file {
            let _ = writeln!(file, "{} {event}", chrono::Utc::now().to_rfc3339());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paste_names_keep_only_a_safe_basename() {
        assert_eq!(
            paste_name(7, 0xab, "../../etc/pass wd;rm", UploadKind::File),
            "7-00ab-pass_wd_rm"
        );
        assert_eq!(
            paste_name(7, 1, "C:\\Users\\me\\shot.png", UploadKind::Image),
            "7-0001-shot.png"
        );
        assert_eq!(
            paste_name(7, 1, "", UploadKind::Image),
            "7-0001-clipboard.png"
        );
        assert_eq!(paste_name(7, 1, "..", UploadKind::File), "7-0001-file");
        assert_eq!(paste_name(7, 1, "dir/", UploadKind::File), "7-0001-file");
        assert_eq!(paste_name(7, 1, "é.txt", UploadKind::File), "7-0001-_.txt");
        let long = paste_name(7, 1, &"x".repeat(300), UploadKind::File);
        assert_eq!(long.len(), "7-0001-".len() + PASTE_NAME_MAX);
    }

    #[test]
    fn the_working_folder_falls_back_to_home_when_missing() {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir(home.path().join("app")).unwrap();
        let home = home.path();
        assert_eq!(resolve_cwd(None, home), (home.to_path_buf(), false));
        assert_eq!(resolve_cwd(Some("~"), home), (home.to_path_buf(), false));
        assert_eq!(resolve_cwd(Some("~/app"), home), (home.join("app"), false));
        assert_eq!(resolve_cwd(Some("app"), home), (home.join("app"), false));
        let absolute = home.join("app").display().to_string();
        assert_eq!(
            resolve_cwd(Some(&absolute), home),
            (home.join("app"), false)
        );
        assert_eq!(
            resolve_cwd(Some("~/gone"), home),
            (home.to_path_buf(), true)
        );
    }

    #[test]
    fn only_a_positive_pid_in_the_ready_file_counts() {
        assert_eq!(parse_pid("4242\n"), Some(4242));
        assert_eq!(parse_pid("0"), None);
        assert_eq!(parse_pid("-1"), None);
        assert_eq!(parse_pid("pid"), None);
    }

    #[test]
    fn zero_sizes_never_reach_the_terminal() {
        assert!(pty_size(Size { rows: 0, cols: 80 }).is_none());
        assert!(pty_size(Size { rows: 24, cols: 0 }).is_none());
        assert_eq!(
            pty_size(Size {
                rows: 30,
                cols: 100
            })
            .map(|size| (size.rows, size.cols)),
            Some((30, 100))
        );
    }

    #[test]
    fn a_held_lock_is_reported_as_busy_not_as_an_error() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("default.lock");
        let held = try_lock(&path).unwrap();
        assert!(held.is_some());
        assert!(try_lock(&path).unwrap().is_none());
        drop(held);
        assert!(try_lock(&path).unwrap().is_some());
    }

    #[test]
    fn the_log_rotates_once_past_its_cap() {
        let temporary = tempfile::tempdir().unwrap();
        let log = Log {
            path: temporary.path().join("default.log"),
            rotated: temporary.path().join("default.log.1"),
        };
        std::fs::write(
            &log.path,
            vec![b'x'; usize::try_from(LOG_CAP_BYTES).unwrap()],
        )
        .unwrap();
        log.event("attach");
        assert_eq!(
            std::fs::metadata(&log.rotated).unwrap().len(),
            LOG_CAP_BYTES
        );
        let fresh = std::fs::read_to_string(&log.path).unwrap();
        assert!(fresh.trim_end().ends_with(" attach"));
        assert_eq!(fresh.lines().count(), 1);
    }
}
