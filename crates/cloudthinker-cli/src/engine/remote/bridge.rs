use std::collections::BTreeMap;
use std::fmt;
use std::io::ErrorKind;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::os::unix::fs::FileTypeExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, SystemTime};

use cloudthinker_client::CtError;
use cloudthinker_client::auth::worker_store::write_private;
use rustix::fs::OFlags;
use tokio::io::AsyncWriteExt;
use tokio::net::UnixStream;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf, pipe};
use tokio::sync::mpsc;
use tokio::time::{Instant, MissedTickBehavior};

use super::catalog::{self, expand, home_folder, tilde};
use super::host::{Log, TERMINAL_ENV, last_exit, try_lock};
use super::link::{
    FromHost, LINK_VERSION, Open, Ready, Start, ToHost, read_from_host, write_to_host,
};
use super::paths::{PASTE_MAX_AGE_SECS, RemotePaths};
use super::unix_now;
use super::wire::{
    AttachState, Frame, Hello, PROTOCOL_VERSION, Upload, UploadKind, Welcome, output_frames,
    read_frame, write_frame,
};
use crate::engine::exit::ExitCode;
use crate::engine::output;
use crate::worker::background::probe_transient_scope;

const LINGER_DIR: &str = "/var/lib/systemd/linger";
const CONNECT_WAIT: Duration = Duration::from_secs(5);
const CONNECT_RETRY: Duration = Duration::from_millis(100);
const PING_EVERY: Duration = Duration::from_secs(5);
const WRITE_STALL: Duration = Duration::from_secs(15);
const LOGIND_WAIT: Duration = Duration::from_secs(2);
const FRAME_QUEUE: usize = 64;
const PASTE_NAME_MAX: usize = 80;
const PASTE_START: &[u8] = b"\x1b[200~";
const PASTE_END: &[u8] = b"\x1b[201~";

pub async fn run(_name: &str) -> ExitCode {
    match bridge().await {
        Ok(()) => ExitCode::Ok,
        Err(error) => {
            output::eprintln_error(&error.to_string());
            ExitCode::JobFailed
        }
    }
}

async fn bridge() -> Result<(), BridgeError> {
    let mut stdin = nonblocking(std::io::stdin().as_fd())
        .and_then(pipe::Receiver::from_owned_fd_unchecked)
        .map_err(|error| bridge_error(&format!("could not read the ssh connection: {error}")))?;
    let mut stdout = nonblocking(std::io::stdout().as_fd())
        .and_then(pipe::Sender::from_owned_fd_unchecked)
        .map_err(|error| bridge_error(&format!("could not write the ssh connection: {error}")))?;
    let Some(hello) = answer_until_hello(&mut stdin, &mut stdout).await? else {
        return Ok(());
    };
    if hello.version != PROTOCOL_VERSION {
        let _ = send(&mut stdout, &Frame::Notice(version_notice(hello.version))).await;
        return Ok(());
    }
    let paths = RemotePaths::for_name(&hello.name)?;
    paths.open_root()?;
    if let Some(forwarded) = std::env::var_os("SSH_AUTH_SOCK") {
        point_agent_socket(Path::new(&forwarded), &paths.agent_socket());
    }
    if let Ok(mut legacy) = UnixStream::connect(paths.socket()).await {
        write_frame(&mut legacy, &Frame::Hello(hello))
            .await
            .map_err(|error| bridge_error(&format!("could not reach the agent host: {error}")))?;
        return relay(legacy, stdin, stdout).await;
    }
    let home = home_folder();
    let launch = hello.resume.is_none().then(|| launch(&hello, &home));
    let Some(mut link) = connect(&paths, launch.is_some()).await? else {
        let _ = send(&mut stdout, &Frame::Exit(last_exit(&paths))).await;
        return Ok(());
    };
    let open = Open {
        version: LINK_VERSION,
        size: hello.size,
        resume: hello.resume,
        start: launch.as_ref().map(|launch| launch.start.clone()),
    };
    write_to_host(&mut link, &ToHost::Open(open))
        .await
        .map_err(|error| bridge_error(&format!("could not reach the agent host: {error}")))?;
    Session {
        paths: &paths,
        log: Log::new(&paths),
        home,
        hello: &hello,
        moved: launch.is_some_and(|launch| launch.moved),
        stdout,
    }
    .run(link, stdin)
    .await
}

struct Launch {
    start: Start,
    moved: bool,
}

fn launch(hello: &Hello, home: &Path) -> Launch {
    let (cwd, moved) = resolve_cwd(hello.cwd.as_deref(), home);
    let cwd = std::path::absolute(&cwd).unwrap_or(cwd);
    Launch {
        start: Start {
            cwd: cwd.display().to_string(),
            args: hello.args.clone(),
            env: hello
                .env
                .iter()
                .filter(|(key, _)| TERMINAL_ENV.contains(&key.as_str()))
                .cloned()
                .collect(),
        },
        moved,
    }
}

struct Session<'a> {
    paths: &'a RemotePaths,
    log: Log,
    home: PathBuf,
    hello: &'a Hello,
    moved: bool,
    stdout: pipe::Sender,
}

impl Session<'_> {
    async fn run(mut self, link: UnixStream, stdin: pipe::Receiver) -> Result<(), BridgeError> {
        let (from_host, mut to_host) = link.into_split();
        let (laptop_frames, mut laptop) = mpsc::channel(FRAME_QUEUE);
        let (host_frames, mut host) = mpsc::channel(FRAME_QUEUE);
        let laptop_reader = tokio::spawn(read_laptop(stdin, laptop_frames));
        let host_reader = tokio::spawn(read_host(from_host, host_frames));
        let mut ping = tokio::time::interval_at(Instant::now() + PING_EVERY, PING_EVERY);
        ping.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            let keep_going = tokio::select! {
                frame = laptop.recv() => match frame {
                    Some(frame) => self.on_laptop(frame, &mut to_host).await,
                    None => false,
                },
                frame = host.recv() => match frame {
                    Some(frame) => self.on_host(frame).await,
                    None => false,
                },
                _ = ping.tick() => send(&mut self.stdout, &Frame::Ping).await,
            };
            if !keep_going {
                break;
            }
        }
        laptop_reader.abort();
        host_reader.abort();
        let _ = to_host.shutdown().await;
        let _ = self.stdout.flush().await;
        Ok(())
    }

    async fn on_laptop(&mut self, frame: Frame, to_host: &mut OwnedWriteHalf) -> bool {
        let message = match frame {
            Frame::Input(bytes) => ToHost::Input(bytes),
            Frame::Resize(size) => ToHost::Resize(size),
            Frame::Upload(upload) => match save_upload(self.paths, &upload) {
                Ok(path) => {
                    self.log.event(&format!("upload {}", upload.bytes.len()));
                    ToHost::Input(bracketed_paste(&path))
                }
                Err(error) => {
                    return send(
                        &mut self.stdout,
                        &Frame::Notice(format!("the pasted file could not be saved: {error}")),
                    )
                    .await;
                }
            },
            _ => return true,
        };
        write_to_host(to_host, &message).await.is_ok()
    }

    async fn on_host(&mut self, frame: FromHost) -> bool {
        match frame {
            FromHost::Ready(ready) => self.welcome(ready).await,
            FromHost::Output(bytes) => {
                for frame in output_frames(&bytes) {
                    if !send(&mut self.stdout, &frame).await {
                        return false;
                    }
                }
                true
            }
            FromHost::Exit(code) => {
                let _ = send(&mut self.stdout, &Frame::Exit(code)).await;
                false
            }
            FromHost::Replaced => {
                let _ = send(&mut self.stdout, &Frame::Replaced).await;
                false
            }
            FromHost::Detach => {
                let _ = send(&mut self.stdout, &Frame::Detach).await;
                false
            }
            FromHost::Failed(reason) => send(&mut self.stdout, &Frame::Notice(reason)).await,
        }
    }

    async fn welcome(&mut self, ready: Ready) -> bool {
        prune_pastes(self.paths);
        let frame = Frame::Welcome(Welcome {
            version: PROTOCOL_VERSION,
            cli_version: env!("CARGO_PKG_VERSION").into(),
            state: ready.state,
            started_at: ready.started_at,
            cwd: tilde(Path::new(&ready.cwd), &self.home),
            session: ready.session,
        });
        if !send(&mut self.stdout, &frame).await {
            return false;
        }
        let mut notices = Vec::new();
        match ready.state {
            AttachState::Started => {
                if self.moved {
                    notices.push(format!(
                        "the folder {} is not there; the agent started in {}",
                        self.hello.cwd.as_deref().unwrap_or_default(),
                        self.home.display()
                    ));
                }
                if stops_at_logout().await {
                    notices.push(
                        "this machine stops the agent when you log out; run `loginctl enable-linger` (or ask an admin) to keep it running"
                            .into(),
                    );
                }
            }
            AttachState::Attached => {
                if self.hello.resume.is_none()
                    && (self.hello.cwd.is_some() || !self.hello.args.is_empty())
                {
                    notices.push(
                        "the agent is already running; --cwd and agent arguments apply only when it starts"
                            .into(),
                    );
                }
            }
        }
        for notice in notices {
            if !send(&mut self.stdout, &Frame::Notice(notice)).await {
                return false;
            }
        }
        true
    }
}

async fn read_laptop(mut stdin: pipe::Receiver, frames: mpsc::Sender<Frame>) {
    while let Ok(Some(frame)) = read_frame(&mut stdin).await {
        if frames.send(frame).await.is_err() {
            return;
        }
    }
}

async fn read_host(mut from_host: OwnedReadHalf, frames: mpsc::Sender<FromHost>) {
    while let Ok(Some(frame)) = read_from_host(&mut from_host).await {
        if frames.send(frame).await.is_err() {
            return;
        }
    }
}

async fn send(stdout: &mut pipe::Sender, frame: &Frame) -> bool {
    matches!(
        tokio::time::timeout(WRITE_STALL, write_frame(stdout, frame)).await,
        Ok(Ok(()))
    )
}

fn version_notice(client: u16) -> String {
    format!(
        "this machine's cloudthinker {} speaks remote protocol {PROTOCOL_VERSION}, and yours speaks {client}; update cloudthinker on both machines",
        env!("CARGO_PKG_VERSION")
    )
}

async fn answer_until_hello(
    stdin: &mut pipe::Receiver,
    stdout: &mut pipe::Sender,
) -> Result<Option<Hello>, BridgeError> {
    let home = catalog::home_folder();
    loop {
        let frame = read_frame(stdin).await.map_err(|error| {
            bridge_error(&format!("could not read the laptop's request: {error}"))
        })?;
        let reply = match frame {
            None => return Ok(None),
            Some(Frame::Hello(hello)) => return Ok(Some(hello)),
            Some(Frame::Query(query)) => Frame::Catalog(catalog::build(&query)),
            Some(Frame::Complete(typed)) => Frame::Completions(catalog::complete(&typed, &home)),
            Some(_) => {
                return Err(bridge_error(
                    "the laptop sent an unexpected first message; update cloudthinker on both machines",
                ));
            }
        };
        write_frame(stdout, &reply)
            .await
            .map_err(|error| bridge_error(&format!("could not answer the laptop: {error}")))?;
    }
}

fn point_agent_socket(forwarded: &Path, link: &Path) {
    let is_socket =
        std::fs::metadata(forwarded).is_ok_and(|metadata| metadata.file_type().is_socket());
    if !is_socket {
        return;
    }
    let mut temporary = link.as_os_str().to_owned();
    temporary.push(format!(".{}.tmp", uuid::Uuid::new_v4()));
    let temporary = Path::new(&temporary);
    if std::os::unix::fs::symlink(forwarded, temporary).is_err() {
        return;
    }
    if std::fs::rename(temporary, link).is_err() {
        let _ = std::fs::remove_file(temporary);
    }
}

async fn connect(paths: &RemotePaths, may_start: bool) -> Result<Option<UnixStream>, BridgeError> {
    let mut deadline = Instant::now() + CONNECT_WAIT;
    let mut spawned = false;
    loop {
        match UnixStream::connect(paths.link()).await {
            Ok(stream) => return Ok(Some(stream)),
            Err(error)
                if matches!(
                    error.kind(),
                    ErrorKind::NotFound | ErrorKind::ConnectionRefused
                ) =>
            {
                if !spawned && clear_stale_socket(paths)? {
                    if !may_start {
                        return Ok(None);
                    }
                    spawn_host(paths.name()).await?;
                    spawned = true;
                    deadline = Instant::now() + CONNECT_WAIT;
                }
            }
            Err(error) => {
                return Err(bridge_error(&format!(
                    "could not reach the agent host: {error}"
                )));
            }
        }
        if Instant::now() >= deadline {
            return Err(bridge_error(&format!(
                "the agent host did not start within {} s; see {}",
                CONNECT_WAIT.as_secs(),
                paths.log().display()
            )));
        }
        tokio::time::sleep(CONNECT_RETRY).await;
    }
}

fn clear_stale_socket(paths: &RemotePaths) -> Result<bool, BridgeError> {
    let lock = try_lock(&paths.lock())
        .map_err(|error| bridge_error(&format!("could not open the agent host lock: {error}")))?;
    let Some(lock) = lock else {
        return Ok(false);
    };
    let _ = std::fs::remove_file(paths.link());
    let _ = std::fs::remove_file(paths.socket());
    drop(lock);
    Ok(true)
}

async fn spawn_host(name: &str) -> Result<(), BridgeError> {
    let program = std::env::current_exe().map_err(|error| {
        bridge_error(&format!("could not find the cloudthinker program: {error}"))
    })?;
    let environment = environment();
    let mut command =
        if lingering(&environment) && probe_transient_scope(&environment).await.is_ok() {
            let mut command = tokio::process::Command::new("systemd-run");
            command
                .args(["--user", "--scope", "--collect", "--quiet", "--"])
                .arg(program);
            command
        } else {
            tokio::process::Command::new(program)
        };
    command
        .args(["remote-host", "--name", name])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(drop)
        .map_err(|error| bridge_error(&format!("could not start the agent host: {error}")))
}

fn environment() -> BTreeMap<String, String> {
    std::env::vars_os()
        .filter_map(|(key, value)| Some((key.into_string().ok()?, value.into_string().ok()?)))
        .collect()
}

fn lingering(environment: &BTreeMap<String, String>) -> bool {
    ["USER", "LOGNAME"]
        .iter()
        .find_map(|key| environment.get(*key))
        .is_some_and(|user| Path::new(LINGER_DIR).join(user).exists())
}

async fn stops_at_logout() -> bool {
    if lingering(&environment()) {
        return false;
    }
    let query = tokio::process::Command::new("busctl")
        .args([
            "get-property",
            "org.freedesktop.login1",
            "/org/freedesktop/login1",
            "org.freedesktop.login1.Manager",
            "KillUserProcesses",
        ])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .output();
    matches!(
        tokio::time::timeout(LOGIND_WAIT, query).await,
        Ok(Ok(found)) if found.status.success() && found.stdout.trim_ascii() == b"b true"
    )
}

async fn relay(
    stream: UnixStream,
    mut stdin: pipe::Receiver,
    mut stdout: pipe::Sender,
) -> Result<(), BridgeError> {
    let (mut from_host, mut to_host) = stream.into_split();
    let upstream = async {
        let _ = tokio::io::copy(&mut stdin, &mut to_host).await;
        let _ = to_host.shutdown().await;
        std::future::pending::<()>().await;
    };
    let downstream = async {
        let _ = tokio::io::copy(&mut from_host, &mut stdout).await;
        let _ = stdout.flush().await;
    };
    tokio::select! {
        () = upstream => {}
        () = downstream => {}
    }
    Ok(())
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
    let name = paste_name(unix_now(), rand::random::<u16>(), &upload.name, upload.kind);
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

fn nonblocking(fd: BorrowedFd<'_>) -> std::io::Result<OwnedFd> {
    let owned = fd.try_clone_to_owned()?;
    let flags = rustix::fs::fcntl_getfl(&owned)?;
    rustix::fs::fcntl_setfl(&owned, flags | OFlags::NONBLOCK)?;
    Ok(owned)
}

#[derive(Debug)]
struct BridgeError(String);

impl fmt::Display for BridgeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<CtError> for BridgeError {
    fn from(error: CtError) -> Self {
        Self(error.to_string())
    }
}

fn bridge_error(reason: &str) -> BridgeError {
    BridgeError(format!("cloudthinker remote-bridge: {reason}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_agent_socket_link_follows_the_newest_forwarded_socket() {
        let temporary = tempfile::tempdir().unwrap();
        let first = temporary.path().join("first.sock");
        let second = temporary.path().join("second.sock");
        let _first = std::os::unix::net::UnixListener::bind(&first).unwrap();
        let _second = std::os::unix::net::UnixListener::bind(&second).unwrap();
        let link = temporary.path().join("default.agent.sock");
        point_agent_socket(&first, &link);
        assert_eq!(std::fs::read_link(&link).unwrap(), first);
        point_agent_socket(&second, &link);
        assert_eq!(std::fs::read_link(&link).unwrap(), second);
        point_agent_socket(&temporary.path().join("missing.sock"), &link);
        assert_eq!(std::fs::read_link(&link).unwrap(), second);
        assert_eq!(std::fs::read_dir(temporary.path()).unwrap().count(), 3);
    }

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
}
