use std::collections::BTreeMap;
use std::fmt;
use std::io::ErrorKind;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::os::unix::fs::FileTypeExt;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use cloudthinker_client::CtError;
use rustix::fs::OFlags;
use tokio::io::AsyncWriteExt;
use tokio::net::UnixStream;
use tokio::net::unix::pipe;
use tokio::time::Instant;

use super::catalog;
use super::host::try_lock;
use super::paths::RemotePaths;
use super::wire::{Frame, Hello, read_frame, write_frame};
use crate::engine::exit::ExitCode;
use crate::engine::output;
use crate::worker::background::probe_transient_scope;

const LINGER_DIR: &str = "/var/lib/systemd/linger";
const CONNECT_WAIT: Duration = Duration::from_secs(5);
const CONNECT_RETRY: Duration = Duration::from_millis(100);

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
    let paths = RemotePaths::for_name(&hello.name)?;
    paths.open_root()?;
    if let Some(forwarded) = std::env::var_os("SSH_AUTH_SOCK") {
        point_agent_socket(Path::new(&forwarded), &paths.agent_socket());
    }
    let mut stream = connect(&paths).await?;
    write_frame(&mut stream, &Frame::Hello(hello))
        .await
        .map_err(|error| bridge_error(&format!("could not reach the agent host: {error}")))?;
    relay(stream, stdin, stdout).await
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

async fn connect(paths: &RemotePaths) -> Result<UnixStream, BridgeError> {
    let mut deadline = Instant::now() + CONNECT_WAIT;
    let mut spawned = false;
    loop {
        match UnixStream::connect(paths.socket()).await {
            Ok(stream) => return Ok(stream),
            Err(error)
                if matches!(
                    error.kind(),
                    ErrorKind::NotFound | ErrorKind::ConnectionRefused
                ) =>
            {
                if !spawned && clear_stale_socket(paths)? {
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
    let _ = std::fs::remove_file(paths.socket());
    drop(lock);
    Ok(true)
}

async fn spawn_host(name: &str) -> Result<(), BridgeError> {
    let program = std::env::current_exe().map_err(|error| {
        bridge_error(&format!("could not find the cloudthinker program: {error}"))
    })?;
    let environment: BTreeMap<String, String> = std::env::vars_os()
        .filter_map(|(key, value)| Some((key.into_string().ok()?, value.into_string().ok()?)))
        .collect();
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

fn lingering(environment: &BTreeMap<String, String>) -> bool {
    ["USER", "LOGNAME"]
        .iter()
        .find_map(|key| environment.get(*key))
        .is_some_and(|user| Path::new(LINGER_DIR).join(user).exists())
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
}
