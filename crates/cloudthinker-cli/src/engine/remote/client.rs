use std::ffi::OsString;
use std::io::{IsTerminal, Write};
use std::path::PathBuf;
use std::process::Stdio;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use cloudthinker_client::{CtError, CtResult};
use tokio::io::AsyncReadExt;
use tokio::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command};
use tokio::signal::unix::{Signal, SignalKind, signal};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::{Instant, sleep_until, timeout};

use super::catalog::read_origin;
use super::clipboard;
use super::host::TERMINAL_ENV;
use super::keys::{KeyEvent, Scanner, existing_files, interrupts, parse_dropped_paths};
use super::paths::{REMOTE_BIN_ENV, SSH_PROGRAM_ENV, validate_name};
use super::picker::{
    self, Action, Choice, Decision, Explicit, Picker, Screen, display_folder, uptime,
};
use super::targets;
use super::tty::{RawTerminal, TtyInput, bottom_line, clear_bottom_line, window_size};
use super::wire::{
    AttachState, Catalog, Frame, Hello, MAX_UPLOAD, PROTOCOL_VERSION, Query, Size, Upload,
    UploadKind, Welcome, WireError, read_frame, write_frame,
};
use crate::engine::exit::{ExitCode, report};
use crate::engine::output::eprintln_error;

const HEARTBEAT: Duration = Duration::from_secs(15);
const FLUSH_IDLE: Duration = Duration::from_millis(50);
const FIRST_BACKOFF: Duration = Duration::from_millis(500);
const MAX_BACKOFF: Duration = Duration::from_secs(30);
const TICK: Duration = Duration::from_secs(1);
const EXIT_WAIT: Duration = Duration::from_secs(5);
const FALLBACK_SIZE: Size = Size { rows: 24, cols: 80 };
const SSH_OPTIONS: [&str; 8] = [
    "-T",
    "-C",
    "-o",
    "ConnectTimeout=10",
    "-o",
    "ServerAliveInterval=15",
    "-o",
    "ServerAliveCountMax=4",
];
const BATCH_OPTIONS: [&str; 2] = ["-o", "BatchMode=yes"];
const PROMPT_MARKERS: [&str; 4] = [
    "Permission denied",
    "Host key verification failed",
    "passphrase",
    "REMOTE HOST IDENTIFICATION HAS CHANGED",
];
const NOT_INSTALLED: i32 = 127;
const TOO_OLD: i32 = 2;
const SSH_FAILED: i32 = 255;
const STDERR_LIMIT: usize = 64 * 1024;
const CLIPBOARD_NAME: &str = "clipboard.png";
const INCOMING_QUEUE: usize = 64;
const MAX_DROP: usize = 4 * MAX_UPLOAD;
const NO_TARGET: &str =
    "Which machine? Run: cloudthinker remote <ssh-host> (a host from ~/.ssh/config, or user@host)";

#[derive(Debug, Clone)]
pub struct ClientOptions {
    pub target: Option<String>,
    pub name: Option<String>,
    pub cwd: Option<String>,
    pub agent_args: Vec<String>,
}

pub async fn run(options: ClientOptions) -> ExitCode {
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        return report(&CtError::Usage(
            "cloudthinker remote needs a terminal".into(),
        ));
    }
    let ClientOptions {
        target,
        name,
        cwd,
        agent_args,
    } = options;
    let target = match target {
        Some(target) => target,
        None => match targets::latest() {
            Ok(Some(target)) => target,
            Ok(None) => return report(&CtError::Usage(NO_TARGET.into())),
            Err(error) => return report(&error),
        },
    };
    if let Some(Err(error)) = name.as_deref().map(validate_name) {
        return report(&error);
    }
    let launch = match Launch::new(target) {
        Ok(launch) => launch,
        Err(error) => return report(&error),
    };
    let mut signals = match Signals::new() {
        Ok(signals) => signals,
        Err(error) => {
            eprintln_error(&format!("could not watch terminal signals: {error}"));
            return ExitCode::JobFailed;
        }
    };
    let home = std::env::var("HOME").ok();
    let mut client = Client {
        launch,
        name,
        cwd: cwd.map(|cwd| remote_cwd(&cwd, home.as_deref())),
        agent_args,
        plan: None,
        terminal: None,
        held: Vec::new(),
        home: None,
        folder: String::new(),
        size: window_size().unwrap_or(FALLBACK_SIZE),
        session: None,
    };
    client.run(&mut signals).await
}

struct Plan {
    hello: Hello,
    continuing: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ssh {
    Interactive,
    Batch,
}

enum Attempt {
    Done(ExitCode),
    Refused { status: Option<i32>, stderr: String },
    Lost,
}

enum Unanswered {
    Done(ExitCode),
    Ended(i32),
    Replaced,
    Left,
    Lost(LinkEnd),
}

struct Client {
    launch: Launch,
    name: Option<String>,
    cwd: Option<String>,
    agent_args: Vec<String>,
    plan: Option<Plan>,
    terminal: Option<RawTerminal>,
    held: Vec<String>,
    home: Option<String>,
    folder: String,
    size: Size,
    session: Option<u64>,
}

impl Client {
    async fn run(&mut self, signals: &mut Signals) -> ExitCode {
        say(&connecting(&self.launch.target));
        let mut offered = false;
        loop {
            match self.attempt(signals, Ssh::Interactive).await {
                Attempt::Done(code) => return code,
                Attempt::Lost => return self.reconnect(signals).await,
                Attempt::Refused {
                    status: Some(TOO_OLD),
                    ..
                } if !offered => {
                    offered = true;
                    match self.update(&mut signals.stop).await {
                        Ok(true) => say(&connecting(&self.launch.target)),
                        Ok(false) => {
                            eprintln_error(&refused_message(&self.launch.target, Some(TOO_OLD)));
                            return ExitCode::JobFailed;
                        }
                        Err(code) => return code,
                    }
                }
                Attempt::Refused { status, .. } => {
                    eprintln_error(&refused_message(&self.launch.target, status));
                    return ExitCode::JobFailed;
                }
            }
        }
    }

    async fn reconnect(&mut self, signals: &mut Signals) -> ExitCode {
        let mut backoff = FIRST_BACKOFF;
        loop {
            if let Some(code) = self.wait_out(backoff, signals).await {
                return code;
            }
            self.banner("reconnecting…").await;
            match self.attempt(signals, Ssh::Batch).await {
                Attempt::Done(code) => return code,
                Attempt::Lost => backoff = FIRST_BACKOFF,
                Attempt::Refused {
                    status: Some(SSH_FAILED),
                    stderr,
                } if needs_prompt(&stderr) => {
                    self.leave_raw();
                    write_stderr(stderr.as_bytes());
                    match self.attempt(signals, Ssh::Interactive).await {
                        Attempt::Done(code) => return code,
                        Attempt::Lost => backoff = FIRST_BACKOFF,
                        Attempt::Refused { status, .. } => {
                            eprintln_error(&refused_message(&self.launch.target, status));
                            return ExitCode::JobFailed;
                        }
                    }
                }
                Attempt::Refused { .. } => backoff = (backoff * 2).min(MAX_BACKOFF),
            }
        }
    }

    async fn wait_out(&mut self, delay: Duration, signals: &mut Signals) -> Option<ExitCode> {
        if self.terminal.is_none()
            && let Err(code) = self.enter_raw()
        {
            return Some(code);
        }
        let deadline = Instant::now() + delay;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return None;
            }
            self.banner(&format!("reconnecting in {}s", countdown(left)))
                .await;
            let terminal = self.terminal.as_ref()?;
            let wake = Instant::now() + left.min(TICK);
            let pause = tokio::select! {
                code = signals.stop.wait() => Pause::Stop(code),
                read = terminal.input.read() => match read {
                    Ok(bytes) if !bytes.is_empty() => {
                        if interrupts(&bytes) { Pause::Leave } else { Pause::Tick }
                    }
                    Ok(_) | Err(_) => Pause::Stop(ExitCode::Ok),
                },
                _ = signals.resize.recv() => Pause::Tick,
                () = sleep_until(wake) => Pause::Tick,
            };
            match pause {
                Pause::Tick => {}
                Pause::Stop(code) => {
                    self.leave_raw();
                    return Some(code);
                }
                Pause::Leave => {
                    self.leave_raw();
                    say(&detached_message(&self.folder));
                    return Some(ExitCode::Ok);
                }
            }
        }
    }

    async fn banner(&mut self, status: &str) {
        let size = self.refresh_size();
        if let Some(terminal) = &self.terminal {
            let text = format!(
                " ⚠ Connection lost · {status} · typing paused · ctrl+c leaves (agent keeps running)"
            );
            let _ = terminal.output.write_all(&bottom_line(&text, size)).await;
        }
    }

    async fn attempt(&mut self, signals: &mut Signals, ssh: Ssh) -> Attempt {
        let mut child = match self.launch.command(ssh).spawn() {
            Ok(child) => child,
            Err(error) => {
                self.leave_raw();
                eprintln_error(&format!(
                    "could not start {}: {error}",
                    self.launch.program.to_string_lossy()
                ));
                return Attempt::Done(ExitCode::JobFailed);
            }
        };
        let stderr = child
            .stderr
            .take()
            .map(|stderr| tokio::spawn(collect(stderr)));
        let Some(mut link) = Link::open(&mut child) else {
            let _ = child.kill().await;
            self.leave_raw();
            return Attempt::Done(ExitCode::JobFailed);
        };
        let outcome = self.converse(&mut link, signals).await;
        drop(link);
        let status = match outcome {
            Some(_) => None,
            None => exit_code(&mut child).await,
        };
        let _ = child.kill().await;
        match outcome {
            Some(outcome) => {
                if let Some(task) = stderr {
                    task.abort();
                }
                outcome
            }
            None => Attempt::Refused {
                status,
                stderr: match stderr {
                    Some(task) => timeout(EXIT_WAIT, task)
                        .await
                        .ok()
                        .and_then(Result::ok)
                        .unwrap_or_default(),
                    None => String::new(),
                },
            },
        }
    }

    async fn converse(&mut self, link: &mut Link, signals: &mut Signals) -> Option<Attempt> {
        if self.plan.is_none() {
            match self.choose_plan(link, signals).await {
                Ok(plan) => self.plan = Some(plan),
                Err(outcome) => return outcome,
            }
        }
        let size = self.refresh_size();
        let plan = self.plan.as_ref()?;
        link.send(Frame::Hello(Hello {
            size,
            resume: self.session,
            ..plan.hello.clone()
        }));
        let welcome = match self
            .expect(link, &mut signals.stop, |frame| match frame {
                Frame::Welcome(welcome) => Some(welcome),
                _ => None,
            })
            .await
        {
            Ok(welcome) => welcome,
            Err(why) => return self.unanswered(why),
        };
        if welcome.version != PROTOCOL_VERSION {
            self.leave_raw();
            eprintln_error(&version_message(&welcome, &self.launch.target));
            return Some(Attempt::Done(ExitCode::JobFailed));
        }
        self.folder = match &self.home {
            Some(home) => display_folder(&welcome.cwd, home),
            None => welcome.cwd.clone(),
        };
        if self.session.replace(welcome.session).is_some() {
            if let Some(terminal) = &self.terminal {
                let _ = terminal
                    .output
                    .write_all(&clear_bottom_line(self.size))
                    .await;
            }
        } else {
            let _ = targets::remember(&self.launch.target);
            let continuing = self
                .plan
                .as_ref()
                .and_then(|plan| plan.continuing.as_deref());
            say(&greeting(&welcome, &self.folder, continuing, unix_now()));
        }
        if self.terminal.is_none()
            && let Err(code) = self.enter_raw()
        {
            return Some(Attempt::Done(code));
        }
        let terminal = self.terminal.as_ref()?;
        let end = session(link, signals, terminal, &mut self.size).await;
        Some(self.finish(end))
    }

    async fn choose_plan(
        &mut self,
        link: &mut Link,
        signals: &mut Signals,
    ) -> Result<Plan, Option<Attempt>> {
        link.send(Frame::Query(Query {
            probe: laptop_probe(),
        }));
        let catalog = match self
            .expect(link, &mut signals.stop, |frame| match frame {
                Frame::Catalog(catalog) => Some(catalog),
                _ => None,
            })
            .await
        {
            Ok(catalog) => catalog,
            Err(why) => return Err(self.unanswered(why)),
        };
        let origin = std::env::current_dir()
            .ok()
            .and_then(|dir| read_origin(&dir));
        let explicit = Explicit {
            name: self.name.as_deref(),
            cwd: self.cwd.as_deref(),
        };
        let decision = match picker::choose(&catalog, origin.as_deref(), explicit) {
            Choice::Open(decision) => decision,
            Choice::Ask => match self.pick(link, signals, &catalog).await {
                Ok(Some(decision)) => decision,
                Ok(None) => return Err(Some(Attempt::Done(ExitCode::Ok))),
                Err(why) => return Err(self.unanswered(why)),
            },
        };
        self.home = Some(catalog.home);
        Ok(plan_for(decision, &self.agent_args, self.size))
    }

    async fn pick(
        &self,
        link: &mut Link,
        signals: &mut Signals,
        catalog: &Catalog,
    ) -> Result<Option<Decision>, Unanswered> {
        let terminal = match RawTerminal::prompt() {
            Ok(terminal) => terminal,
            Err(error) => {
                eprintln_error(&format!(
                    "could not switch the terminal to raw mode: {error}"
                ));
                return Err(Unanswered::Done(ExitCode::JobFailed));
            }
        };
        let mut menu = Picker::new(catalog, &self.launch.target, unix_now());
        let mut screen = Screen::default();
        let mut notes = Vec::new();
        let mut action = Action::Redraw;
        let outcome = loop {
            match action {
                Action::Nothing => {}
                Action::Redraw => {
                    let size = window_size().unwrap_or(self.size);
                    let frame = screen.draw(&menu.view(size), size.cols);
                    if terminal.output.write_all(&frame).await.is_err() {
                        break Ok(None);
                    }
                }
                Action::Complete(prefix) => {
                    if !link.send(Frame::Complete(prefix)) {
                        break Err(Unanswered::Lost(LinkEnd::Closed));
                    }
                }
                Action::Done(decision) => break Ok(Some(decision)),
                Action::Cancel => break Ok(None),
            }
            action = tokio::select! {
                code = signals.stop.wait() => break Err(Unanswered::Done(code)),
                read = terminal.input.read() => match read {
                    Ok(bytes) if !bytes.is_empty() => menu.press(&picker::decode(&bytes)),
                    Ok(_) | Err(_) => Action::Cancel,
                },
                incoming = link.recv() => match incoming {
                    Ok(Frame::Completions(entries)) => menu.completions(entries),
                    Ok(Frame::Notice(text)) => {
                        notes.push(text);
                        Action::Nothing
                    }
                    Ok(Frame::Exit(code)) => break Err(Unanswered::Done(agent_exit(code))),
                    Ok(_) => Action::Nothing,
                    Err(end) => break Err(Unanswered::Lost(end)),
                },
                _ = signals.resize.recv() => Action::Redraw,
            };
        };
        let _ = terminal.output.write_all(&screen.clear()).await;
        drop(terminal);
        for note in notes {
            say(&note);
        }
        outcome
    }

    async fn expect<T>(
        &mut self,
        link: &mut Link,
        stop: &mut StopSignals,
        wanted: impl Fn(Frame) -> Option<T>,
    ) -> Result<T, Unanswered> {
        let mut refused = false;
        loop {
            let incoming = tokio::select! {
                code = stop.wait() => return Err(Unanswered::Done(code)),
                () = typed_interrupt(self.terminal.as_ref()) => return Err(Unanswered::Left),
                incoming = link.recv() => incoming,
            };
            match incoming {
                Ok(Frame::Exit(code)) if self.session.is_some() => {
                    return Err(Unanswered::Ended(code));
                }
                Ok(Frame::Exit(code)) => return Err(Unanswered::Done(agent_exit(code))),
                Ok(Frame::Replaced) => return Err(Unanswered::Replaced),
                Ok(Frame::Notice(text)) => {
                    self.note(text);
                    refused = true;
                }
                Ok(frame) => {
                    if let Some(value) = wanted(frame) {
                        return Ok(value);
                    }
                }
                Err(_) if refused => return Err(Unanswered::Done(ExitCode::JobFailed)),
                Err(end) => return Err(Unanswered::Lost(end)),
            }
        }
    }

    fn unanswered(&mut self, why: Unanswered) -> Option<Attempt> {
        match why {
            Unanswered::Done(code) => Some(self.finish(End::Stopped(code))),
            Unanswered::Ended(code) => Some(self.finish(End::Exited(code))),
            Unanswered::Replaced => Some(self.finish(End::Replaced)),
            Unanswered::Left => Some(self.finish(End::Detached)),
            Unanswered::Lost(end) => {
                if self.terminal.is_none() {
                    end.report();
                }
                None
            }
        }
    }

    fn finish(&mut self, end: End) -> Attempt {
        let (line, code) = match end {
            End::Lost => return Attempt::Lost,
            End::Exited(code) => (Some(ended_message(&self.folder)), agent_exit(code)),
            End::Detached => (Some(detached_message(&self.folder)), ExitCode::Ok),
            End::Replaced => (
                Some("Attached from another terminal".to_string()),
                ExitCode::Ok,
            ),
            End::Stopped(code) => (None, code),
        };
        self.leave_raw();
        if let Some(line) = line {
            say(&line);
        }
        Attempt::Done(code)
    }

    async fn update(&self, stop: &mut StopSignals) -> Result<bool, ExitCode> {
        write_stderr(
            format!(
                "cloudthinker on {} is too old for remote. Update it now? [Y/n] ",
                self.launch.target
            )
            .as_bytes(),
        );
        let Ok(input) = TtyInput::new() else {
            return Ok(false);
        };
        let answer = tokio::select! {
            code = stop.wait() => {
                write_stderr(b"\n");
                return Err(code);
            }
            answer = input.read() => answer,
        };
        drop(input);
        match answer {
            Ok(bytes) if !bytes.is_empty() && accepts(&String::from_utf8_lossy(&bytes)) => {}
            Ok(_) | Err(_) => return Ok(false),
        }
        let mut command = self.launch.update_command();
        let status = tokio::select! {
            code = stop.wait() => return Err(code),
            status = command.status() => status,
        };
        Ok(status.is_ok_and(|status| status.success()))
    }

    fn enter_raw(&mut self) -> Result<(), ExitCode> {
        match RawTerminal::enter() {
            Ok(terminal) => {
                self.terminal = Some(terminal);
                Ok(())
            }
            Err(error) => {
                eprintln_error(&format!(
                    "could not switch the terminal to raw mode: {error}"
                ));
                Err(ExitCode::JobFailed)
            }
        }
    }

    fn leave_raw(&mut self) {
        self.terminal = None;
        for line in std::mem::take(&mut self.held) {
            say(&line);
        }
    }

    fn note(&mut self, text: String) {
        if self.terminal.is_some() {
            self.held.push(text);
        } else {
            say(&text);
        }
    }

    fn refresh_size(&mut self) -> Size {
        if let Some(size) = window_size() {
            self.size = size;
        }
        self.size
    }
}

enum Pause {
    Tick,
    Stop(ExitCode),
    Leave,
}

async fn typed_interrupt(terminal: Option<&RawTerminal>) {
    let Some(terminal) = terminal else {
        return std::future::pending().await;
    };
    while let Ok(bytes) = terminal.input.read().await {
        if bytes.is_empty() || interrupts(&bytes) {
            return;
        }
    }
}

async fn collect(mut stderr: ChildStderr) -> String {
    let mut bytes = Vec::new();
    let mut chunk = [0u8; 4096];
    while let Ok(read) = stderr.read(&mut chunk).await {
        if read == 0 {
            break;
        }
        if bytes.len() < STDERR_LIMIT {
            bytes.extend_from_slice(chunk.get(..read).unwrap_or_default());
        }
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

async fn exit_code(child: &mut Child) -> Option<i32> {
    timeout(EXIT_WAIT, child.wait()).await.ok()?.ok()?.code()
}

enum End {
    Exited(i32),
    Stopped(ExitCode),
    Detached,
    Replaced,
    Lost,
}

enum Step {
    Read(Vec<u8>),
    Flush,
    Frame(Frame),
    Resize,
    End(End),
}

async fn session(
    link: &mut Link,
    signals: &mut Signals,
    terminal: &RawTerminal,
    size: &mut Size,
) -> End {
    let mut keys = Keys::default();
    if let Some(current) = window_size() {
        *size = current;
    }
    link.send(Frame::Resize(*size));
    let mut heartbeat = Instant::now() + HEARTBEAT;
    let mut flush_at: Option<Instant> = None;
    loop {
        let step = tokio::select! {
            biased;
            code = signals.stop.wait() => Step::End(End::Stopped(code)),
            read = terminal.input.read() => match read {
                Ok(bytes) if !bytes.is_empty() => Step::Read(bytes),
                Ok(_) | Err(_) => Step::End(End::Stopped(ExitCode::Ok)),
            },
            incoming = link.recv() => match incoming {
                Ok(frame) => Step::Frame(frame),
                Err(_) => Step::End(End::Lost),
            },
            _ = signals.resize.recv() => Step::Resize,
            () = sleep_until(flush_at.unwrap_or(heartbeat)), if flush_at.is_some() => Step::Flush,
            () = sleep_until(heartbeat) => Step::End(End::Lost),
        };
        let sent = match step {
            Step::Read(bytes) => {
                let events = keys.scanner.feed(&bytes);
                flush_at = keys
                    .scanner
                    .has_pending()
                    .then(|| Instant::now() + FLUSH_IDLE);
                if let Err(end) = keys.send(events, link).await {
                    return end;
                }
                true
            }
            Step::Flush => {
                flush_at = None;
                let events = keys.scanner.flush();
                if let Err(end) = keys.send(events, link).await {
                    return end;
                }
                true
            }
            Step::Frame(frame) => {
                heartbeat = Instant::now() + HEARTBEAT;
                if let Some(end) = show(frame, terminal).await {
                    return end;
                }
                true
            }
            Step::Resize => match window_size() {
                Some(current) => {
                    *size = current;
                    link.send(Frame::Resize(current))
                }
                None => true,
            },
            Step::End(end) => return end,
        };
        if !sent {
            return End::Lost;
        }
    }
}

async fn show(frame: Frame, terminal: &RawTerminal) -> Option<End> {
    match frame {
        Frame::Output(bytes) => {
            terminal.track(&bytes);
            terminal
                .output
                .write_all(&bytes)
                .await
                .err()
                .map(|_| End::Stopped(ExitCode::Ok))
        }
        Frame::Notice(text) => {
            write_stderr(format!("\r\n{text}\r\n").as_bytes());
            None
        }
        Frame::Exit(code) => Some(End::Exited(code)),
        Frame::Replaced => Some(End::Replaced),
        Frame::Detach => Some(End::Detached),
        _ => None,
    }
}

#[derive(Default)]
struct Keys {
    scanner: Scanner,
    bridged_press: bool,
}

impl Keys {
    async fn send(&mut self, events: Vec<KeyEvent>, link: &Link) -> Result<(), End> {
        for event in events {
            let frames = match event {
                KeyEvent::Detach => return Err(End::Detached),
                event => self.frames_for(event).await,
            };
            for frame in frames {
                if !link.send(frame) {
                    return Err(End::Lost);
                }
            }
        }
        Ok(())
    }

    async fn frames_for(&mut self, event: KeyEvent) -> Vec<Frame> {
        match event {
            KeyEvent::Detach => Vec::new(),
            KeyEvent::Forward(bytes) => input(bytes),
            KeyEvent::PasteKey(bytes) => match clipboard::read_image().await {
                Some(png) => {
                    self.bridged_press = true;
                    vec![upload(UploadKind::Image, CLIPBOARD_NAME.into(), png)]
                }
                None => {
                    self.bridged_press = false;
                    vec![Frame::Input(bytes)]
                }
            },
            KeyEvent::PasteKeyRepeat(bytes) => {
                if self.bridged_press {
                    Vec::new()
                } else {
                    vec![Frame::Input(bytes)]
                }
            }
            KeyEvent::PasteKeyRelease(bytes) => {
                if std::mem::take(&mut self.bridged_press) {
                    Vec::new()
                } else {
                    vec![Frame::Input(bytes)]
                }
            }
            KeyEvent::Paste { text, raw } => match dropped_files(&text).await {
                Some(frames) => frames,
                None => input(raw),
            },
        }
    }
}

async fn dropped_files(text: &[u8]) -> Option<Vec<Frame>> {
    let tokens = parse_dropped_paths(std::str::from_utf8(text).ok()?)?;
    let home = std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map(PathBuf::from);
    let paths = existing_files(&tokens, home.as_deref())?;
    paths.iter().try_fold(0usize, |total, path| {
        let len = usize::try_from(std::fs::metadata(path).ok()?.len()).ok()?;
        total.checked_add(len).filter(|total| *total <= MAX_DROP)
    })?;
    let mut frames = Vec::with_capacity(paths.len());
    for path in paths {
        let bytes = tokio::fs::read(&path).await.ok()?;
        if bytes.len() > MAX_UPLOAD {
            return None;
        }
        let name = path.file_name()?.to_string_lossy().into_owned();
        frames.push(upload(UploadKind::File, name, bytes));
    }
    Some(frames)
}

fn input(bytes: Vec<u8>) -> Vec<Frame> {
    if bytes.len() <= MAX_UPLOAD {
        return vec![Frame::Input(bytes)];
    }
    bytes
        .chunks(MAX_UPLOAD)
        .map(|chunk| Frame::Input(chunk.to_vec()))
        .collect()
}

fn upload(kind: UploadKind, name: String, bytes: Vec<u8>) -> Frame {
    Frame::Upload(Upload { kind, name, bytes })
}

enum LinkEnd {
    Closed,
    Broken(WireError),
}

impl LinkEnd {
    fn report(&self) {
        if let Self::Broken(error) = self {
            eprintln_error(&error.to_string());
        }
    }
}

struct Link {
    outgoing: mpsc::UnboundedSender<Frame>,
    incoming: mpsc::Receiver<Result<Frame, LinkEnd>>,
    reader: JoinHandle<()>,
    writer: JoinHandle<()>,
}

impl Link {
    fn open(child: &mut Child) -> Option<Self> {
        let stdin = child.stdin.take()?;
        let stdout = child.stdout.take()?;
        let (outgoing, outgoing_rx) = mpsc::unbounded_channel();
        let (incoming_tx, incoming) = mpsc::channel(INCOMING_QUEUE);
        Some(Self {
            outgoing,
            incoming,
            reader: tokio::spawn(read_frames(stdout, incoming_tx)),
            writer: tokio::spawn(write_frames(stdin, outgoing_rx)),
        })
    }

    fn send(&self, frame: Frame) -> bool {
        self.outgoing.send(frame).is_ok()
    }

    async fn recv(&mut self) -> Result<Frame, LinkEnd> {
        self.incoming.recv().await.unwrap_or(Err(LinkEnd::Closed))
    }
}

impl Drop for Link {
    fn drop(&mut self) {
        self.reader.abort();
        self.writer.abort();
    }
}

async fn read_frames(mut stdout: ChildStdout, incoming: mpsc::Sender<Result<Frame, LinkEnd>>) {
    loop {
        let item = match read_frame(&mut stdout).await {
            Ok(Some(frame)) => Ok(frame),
            Ok(None) => Err(LinkEnd::Closed),
            Err(error) => Err(LinkEnd::Broken(error)),
        };
        let last = item.is_err();
        if incoming.send(item).await.is_err() || last {
            return;
        }
    }
}

async fn write_frames(mut stdin: ChildStdin, mut outgoing: mpsc::UnboundedReceiver<Frame>) {
    while let Some(frame) = outgoing.recv().await {
        match write_frame(&mut stdin, &frame).await {
            Ok(()) | Err(WireError::TooLarge(_)) => {}
            Err(_) => return,
        }
    }
}

struct StopSignals {
    interrupt: Signal,
    hangup: Signal,
    terminate: Signal,
}

impl StopSignals {
    async fn wait(&mut self) -> ExitCode {
        tokio::select! {
            _ = self.interrupt.recv() => ExitCode::Interrupted,
            _ = self.hangup.recv() => ExitCode::Ok,
            _ = self.terminate.recv() => ExitCode::Ok,
        }
    }
}

struct Signals {
    stop: StopSignals,
    resize: Signal,
}

impl Signals {
    fn new() -> std::io::Result<Self> {
        Ok(Self {
            stop: StopSignals {
                interrupt: signal(SignalKind::interrupt())?,
                hangup: signal(SignalKind::hangup())?,
                terminate: signal(SignalKind::terminate())?,
            },
            resize: signal(SignalKind::window_change())?,
        })
    }
}

struct Launch {
    target: String,
    program: OsString,
    ssh_options: bool,
    remote: String,
    update: String,
}

impl Launch {
    fn new(target: String) -> CtResult<Self> {
        validate_target(&target)?;
        let bin = std::env::var(REMOTE_BIN_ENV)
            .ok()
            .filter(|bin| !bin.is_empty());
        let remote = remote_command(bin.as_deref())?;
        let update = update_command(bin.as_deref())?;
        let shim = std::env::var_os(SSH_PROGRAM_ENV).filter(|program| !program.is_empty());
        Ok(Self {
            target,
            ssh_options: shim.is_none(),
            program: shim.unwrap_or_else(|| OsString::from("ssh")),
            remote,
            update,
        })
    }

    fn command(&self, ssh: Ssh) -> Command {
        let mut command = Command::new(&self.program);
        if self.ssh_options {
            command.args(SSH_OPTIONS);
            if ssh == Ssh::Batch {
                command.args(BATCH_OPTIONS);
            }
        }
        command
            .arg(&self.target)
            .arg(&self.remote)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(match ssh {
                Ssh::Interactive => Stdio::inherit(),
                Ssh::Batch => Stdio::piped(),
            })
            .kill_on_drop(true);
        command
    }

    fn update_command(&self) -> Command {
        let mut command = Command::new(&self.program);
        if self.ssh_options {
            command.args(SSH_OPTIONS);
        }
        command
            .arg(&self.target)
            .arg(&self.update)
            .kill_on_drop(true);
        command
    }
}

fn validate_target(target: &str) -> CtResult<()> {
    if target.is_empty() || target.starts_with('-') {
        return Err(CtError::Usage(format!(
            "{target:?} is not an SSH destination; give a host, user@host, or ssh://user@host:port"
        )));
    }
    Ok(())
}

fn find_binary(bin: Option<&str>) -> CtResult<String> {
    match bin {
        Some(path) if path.contains('\'') => Err(CtError::Usage(format!(
            "{REMOTE_BIN_ENV} must not contain a single quote"
        ))),
        Some(path) => Ok(format!("b='{path}'; [ -x \"$b\" ] || exit {NOT_INSTALLED}")),
        None => Ok(format!(
            "b=$(command -v cloudthinker || echo \"$HOME/.local/bin/cloudthinker\"); [ -x \"$b\" ] || exit {NOT_INSTALLED}"
        )),
    }
}

fn remote_command(bin: Option<&str>) -> CtResult<String> {
    let find = find_binary(bin)?;
    Ok(sh(&format!(
        "{find}; \"$b\" remote-bridge --help >/dev/null 2>&1 || exit {TOO_OLD}; exec \"$b\" remote-bridge"
    )))
}

fn update_command(bin: Option<&str>) -> CtResult<String> {
    let find = find_binary(bin)?;
    Ok(sh(&format!("{find}; exec \"$b\" update")))
}

fn sh(script: &str) -> String {
    format!("sh -c {}", single_quote(script))
}

fn single_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

fn plan_for(decision: Decision, agent_args: &[String], size: Size) -> Plan {
    let (name, cwd, session) = match decision {
        Decision::Attach { name } => (name, None, None),
        Decision::Start { name, cwd, session } => (name, Some(cwd), session),
    };
    let mut args = Vec::with_capacity(agent_args.len() + 2);
    let continuing = session.map(|session| {
        args.push("--session".to_string());
        args.push(session.file);
        session.title
    });
    args.extend_from_slice(agent_args);
    Plan {
        hello: Hello {
            version: PROTOCOL_VERSION,
            size,
            name,
            cwd,
            args,
            env: forwarded_env(),
            resume: None,
        },
        continuing,
    }
}

fn laptop_probe() -> Option<String> {
    let cwd = std::env::current_dir().ok()?;
    probe_for(
        &cwd.to_string_lossy(),
        std::env::var("HOME").ok().as_deref(),
    )
}

fn probe_for(cwd: &str, home: Option<&str>) -> Option<String> {
    let probe = remote_cwd(cwd, home);
    (probe.starts_with("~/") && probe.trim_end_matches('/') != "~").then_some(probe)
}

fn remote_cwd(cwd: &str, home: Option<&str>) -> String {
    let Some(home) = home
        .map(|home| home.trim_end_matches('/'))
        .filter(|home| !home.is_empty())
    else {
        return cwd.to_string();
    };
    match cwd.strip_prefix(home) {
        Some("") => "~".into(),
        Some(rest) if rest.starts_with('/') => format!("~{rest}"),
        _ => cwd.to_string(),
    }
}

fn forwarded_env() -> Vec<(String, String)> {
    TERMINAL_ENV
        .iter()
        .filter_map(|key| {
            std::env::var(key)
                .ok()
                .map(|value| ((*key).to_string(), value))
        })
        .collect()
}

fn agent_exit(code: i32) -> ExitCode {
    match code {
        0 => ExitCode::Ok,
        130 => ExitCode::Interrupted,
        _ => ExitCode::JobFailed,
    }
}

fn needs_prompt(stderr: &str) -> bool {
    PROMPT_MARKERS.iter().any(|marker| stderr.contains(marker))
}

fn accepts(answer: &str) -> bool {
    matches!(
        answer.trim().to_ascii_lowercase().as_str(),
        "" | "y" | "yes"
    )
}

fn connecting(target: &str) -> String {
    format!("Connecting to {target}…")
}

fn greeting(welcome: &Welcome, folder: &str, continuing: Option<&str>, now: u64) -> String {
    match (welcome.state, continuing) {
        (AttachState::Attached, _) => format!(
            "Back to your agent in {folder} · {}",
            uptime(now, welcome.started_at)
        ),
        (AttachState::Started, Some("")) => {
            format!("Continuing the last session in {folder}")
        }
        (AttachState::Started, Some(title)) => format!("Continuing \"{title}\" in {folder}"),
        (AttachState::Started, None) => format!("Starting a new agent in {folder}"),
    }
}

fn detached_message(folder: &str) -> String {
    format!("Detached · the agent in {folder} keeps running · cloudthinker remote returns to it")
}

fn ended_message(folder: &str) -> String {
    format!("The agent in {folder} ended · cloudthinker remote continues that session")
}

fn refused_message(target: &str, code: Option<i32>) -> String {
    match code {
        Some(NOT_INSTALLED) => format!(
            "cloudthinker is not installed on {target}. Install it there: curl -fsSL https://cloudthinker.io/install.sh | sh"
        ),
        Some(TOO_OLD) => format!(
            "cloudthinker on {target} is too old for remote. Update it: ssh {target} cloudthinker update"
        ),
        Some(SSH_FAILED) => format!("ssh could not connect to {target}; see ssh's message above"),
        Some(code) => format!("the remote side exited with status {code}"),
        None => "the remote side stopped without an exit status".to_string(),
    }
}

fn version_message(welcome: &Welcome, target: &str) -> String {
    let local = env!("CARGO_PKG_VERSION");
    let remote = &welcome.cli_version;
    if welcome.version < PROTOCOL_VERSION {
        format!(
            "cloudthinker {remote} on {target} speaks remote protocol {}, and this cloudthinker {local} speaks {PROTOCOL_VERSION}. Update it: ssh {target} cloudthinker update",
            welcome.version
        )
    } else {
        format!(
            "cloudthinker {remote} on {target} speaks remote protocol {}, and this cloudthinker {local} speaks {PROTOCOL_VERSION}. Update this one: cloudthinker update",
            welcome.version
        )
    }
}

fn countdown(left: Duration) -> u64 {
    u64::try_from(left.as_millis().div_ceil(1000))
        .unwrap_or(u64::MAX)
        .max(1)
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

fn say(line: &str) {
    write_stderr(format!("{line}\n").as_bytes());
}

fn write_stderr(bytes: &[u8]) {
    let mut stderr = std::io::stderr();
    let _ = stderr.write_all(bytes);
    let _ = stderr.flush();
}

#[cfg(test)]
mod tests {
    use super::super::picker::Session;
    use super::*;

    const FIND: &str = r#"b=$(command -v cloudthinker || echo "$HOME/.local/bin/cloudthinker"); [ -x "$b" ] || exit 127"#;

    #[test]
    fn the_remote_command_runs_under_sh_and_finds_the_binary() {
        let bridge =
            r#"; "$b" remote-bridge --help >/dev/null 2>&1 || exit 2; exec "$b" remote-bridge"#;
        assert_eq!(
            remote_command(None).unwrap(),
            format!("sh -c '{FIND}{bridge}'")
        );
        assert_eq!(
            remote_command(Some("/opt/ct/cloudthinker")).unwrap(),
            format!(r#"sh -c 'b='\''/opt/ct/cloudthinker'\''; [ -x "$b" ] || exit 127{bridge}'"#)
        );
        assert!(remote_command(Some("/tmp/it's")).is_err());
    }

    #[test]
    fn the_update_runs_the_remote_cloudthinker_update_after_a_yes() {
        assert_eq!(
            update_command(None).unwrap(),
            format!(r#"sh -c '{FIND}; exec "$b" update'"#)
        );
        assert!(update_command(Some("/tmp/it's")).is_err());
        for yes in ["\n", "y\n", "Y\n", " yes \n"] {
            assert!(accepts(yes), "{yes:?}");
        }
        for no in ["n\n", "no\n", "later\n"] {
            assert!(!accepts(no), "{no:?}");
        }
    }

    #[test]
    fn a_laptop_home_folder_becomes_the_remote_home() {
        let home = Some("/Users/me");
        assert_eq!(remote_cwd("/Users/me/app", home), "~/app");
        assert_eq!(remote_cwd("/Users/me", home), "~");
        assert_eq!(remote_cwd("/Users/me/", Some("/Users/me/")), "~/");
        assert_eq!(remote_cwd("/Users/meow/app", home), "/Users/meow/app");
        assert_eq!(remote_cwd("/srv/app", home), "/srv/app");
        assert_eq!(remote_cwd("~/app", home), "~/app");
        assert_eq!(remote_cwd("app", None), "app");
    }

    #[test]
    fn only_a_folder_below_the_laptop_home_is_probed() {
        let home = Some("/Users/me");
        assert_eq!(
            probe_for("/Users/me/code/app", home).as_deref(),
            Some("~/code/app")
        );
        assert_eq!(probe_for("/Users/me", home), None);
        assert_eq!(probe_for("/Users/me/", home), None);
        assert_eq!(probe_for("/srv/app", home), None);
        assert_eq!(probe_for("/Users/me/app", None), None);
    }

    #[test]
    fn a_plan_continues_a_session_before_the_agent_args() {
        let size = Size { rows: 30, cols: 90 };
        let agent_args = vec!["--model".to_string(), "x".to_string()];
        let plan = plan_for(
            Decision::Start {
                name: "app-1a2b3c".into(),
                cwd: "/home/me/app".into(),
                session: Some(Session {
                    file: "/s/a.jsonl".into(),
                    title: "fix login bug".into(),
                }),
            },
            &agent_args,
            size,
        );
        assert_eq!(plan.hello.name, "app-1a2b3c");
        assert_eq!(plan.hello.cwd.as_deref(), Some("/home/me/app"));
        assert_eq!(plan.hello.args, ["--session", "/s/a.jsonl", "--model", "x"]);
        assert_eq!(plan.hello.size, size);
        assert_eq!(plan.continuing.as_deref(), Some("fix login bug"));
        let attach = plan_for(
            Decision::Attach {
                name: "infra-1".into(),
            },
            &[],
            size,
        );
        assert_eq!(attach.hello.name, "infra-1");
        assert_eq!(attach.hello.cwd, None);
        assert!(attach.hello.args.is_empty());
        assert_eq!(attach.continuing, None);
    }

    #[test]
    fn each_moment_prints_the_journey_line() {
        let welcome = |state| Welcome {
            version: PROTOCOL_VERSION,
            cli_version: "0.8.3".into(),
            state,
            started_at: 1_000,
            cwd: "/home/me/app".into(),
            session: 1,
        };
        let now = 1_000 + 2 * 3600 + 14 * 60;
        assert_eq!(
            greeting(&welcome(AttachState::Attached), "~/app", None, now),
            "Back to your agent in ~/app · up 2h 14m"
        );
        assert_eq!(
            greeting(&welcome(AttachState::Started), "~/app", None, now),
            "Starting a new agent in ~/app"
        );
        assert_eq!(
            greeting(
                &welcome(AttachState::Started),
                "~/app",
                Some("fix login bug"),
                now
            ),
            "Continuing \"fix login bug\" in ~/app"
        );
        assert_eq!(
            detached_message("~/app"),
            "Detached · the agent in ~/app keeps running · cloudthinker remote returns to it"
        );
        assert_eq!(
            ended_message("~/app"),
            "The agent in ~/app ended · cloudthinker remote continues that session"
        );
        assert_eq!(connecting("vm"), "Connecting to vm…");
    }

    #[test]
    fn only_a_prompting_ssh_failure_steps_out_of_the_banner() {
        assert!(needs_prompt("me@vm: Permission denied (publickey).\n"));
        assert!(needs_prompt("Host key verification failed.\n"));
        assert!(needs_prompt(
            "Enter passphrase for key '/Users/me/.ssh/id_ed25519':"
        ));
        assert!(needs_prompt(
            "@ WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED! @"
        ));
        assert!(!needs_prompt(
            "ssh: connect to host vm port 22: Network is unreachable\n"
        ));
        assert!(!needs_prompt(""));
    }

    #[tokio::test]
    async fn a_drop_over_the_total_budget_and_a_long_paste_go_as_text_in_frames_that_fit() {
        let dir = tempfile::tempdir().unwrap();
        let mut text = Vec::new();
        for index in 0..5 {
            let path = dir.path().join(format!("clip-{index}.mov"));
            std::fs::File::create(&path)
                .unwrap()
                .set_len(u64::try_from(MAX_UPLOAD).unwrap())
                .unwrap();
            text.extend_from_slice(format!("{} ", path.display()).as_bytes());
        }
        let frames = Keys::default()
            .frames_for(KeyEvent::Paste {
                text: text.clone(),
                raw: text.clone(),
            })
            .await;
        assert_eq!(frames, vec![Frame::Input(text)]);

        let long = vec![b'a'; MAX_UPLOAD + 10];
        let frames = Keys::default()
            .frames_for(KeyEvent::Paste {
                text: long.clone(),
                raw: long.clone(),
            })
            .await;
        assert_eq!(frames.len(), 2);
        let mut sent = Vec::new();
        for frame in frames {
            let Frame::Input(bytes) = frame else {
                panic!("a paste is sent as input");
            };
            sent.extend(bytes);
        }
        assert_eq!(sent, long);

        let forwarded = vec![b'a'; MAX_UPLOAD + 16 * 1024];
        let frames = Keys::default()
            .frames_for(KeyEvent::Forward(forwarded.clone()))
            .await;
        assert!(
            frames
                .iter()
                .all(|frame| matches!(frame, Frame::Input(bytes) if bytes.len() <= MAX_UPLOAD))
        );
        assert_eq!(frames.len(), 2);
    }

    #[test]
    fn a_target_that_looks_like_an_option_is_refused() {
        assert!(validate_target("-oProxyCommand=x").is_err());
        assert!(validate_target("").is_err());
        assert!(validate_target("me@10.0.0.5").is_ok());
    }

    #[test]
    fn agent_exit_codes_keep_the_cli_exit_table() {
        assert_eq!(agent_exit(0), ExitCode::Ok);
        assert_eq!(agent_exit(130), ExitCode::Interrupted);
        assert_eq!(agent_exit(1), ExitCode::JobFailed);
        assert_eq!(agent_exit(2), ExitCode::JobFailed);
        assert_eq!(agent_exit(-1), ExitCode::JobFailed);
    }

    #[test]
    fn each_refusal_before_attach_names_one_next_step() {
        assert!(
            refused_message("vm", Some(127))
                .contains("curl -fsSL https://cloudthinker.io/install.sh | sh")
        );
        assert!(refused_message("vm", Some(2)).contains("ssh vm cloudthinker update"));
        assert!(refused_message("vm", Some(255)).starts_with("ssh could not connect to vm"));
        assert_eq!(
            refused_message("vm", Some(3)),
            "the remote side exited with status 3"
        );
    }

    #[test]
    fn a_version_skew_names_both_versions_and_the_fix() {
        let old = Welcome {
            version: 0,
            cli_version: "0.8.2".into(),
            state: AttachState::Started,
            started_at: 0,
            cwd: "~".into(),
            session: 1,
        };
        let message = version_message(&old, "vm");
        assert!(message.contains("0.8.2") && message.contains(env!("CARGO_PKG_VERSION")));
        assert!(message.contains("ssh vm cloudthinker update"));
        let new = Welcome {
            version: PROTOCOL_VERSION + 1,
            cli_version: "9.0.0".into(),
            state: AttachState::Attached,
            started_at: 0,
            cwd: "~".into(),
            session: 1,
        };
        assert!(version_message(&new, "vm").contains("Update this one: cloudthinker update"));
    }

    #[test]
    fn the_banner_countdown_rounds_up_to_whole_seconds() {
        assert_eq!(countdown(FIRST_BACKOFF), 1);
        assert_eq!(countdown(Duration::from_millis(3_200)), 4);
        assert_eq!(countdown(Duration::from_secs(4)), 4);
        assert_eq!(countdown(MAX_BACKOFF), 30);
        assert_eq!(countdown(Duration::ZERO), 1);
    }
}
