use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::Path;
use std::process::{ExitStatus, Stdio};
use std::time::Duration;

use cloudthinker_client::{CtClient, CtError, cyber_discovery_redact, wait_for_run_stop};
use uuid::Uuid;

use crate::engine::output;

const PROCESS_OUTPUT_LIMIT: usize = 16 * 1024;

pub(super) struct RuntimeOutput {
    pub(super) status: ExitStatus,
    pub(super) stdout: Vec<u8>,
    pub(super) stderr: Vec<u8>,
}

pub(super) async fn invoke(
    runtime: &Path,
    args: &[OsString],
    environment: &BTreeMap<OsString, OsString>,
    timeout: Duration,
    run_guard: Option<(&CtClient, Uuid)>,
) -> Result<RuntimeOutput, CtError> {
    invoke_with_snapshot(
        runtime,
        args,
        environment,
        timeout,
        run_guard,
        process_table_snapshot,
    )
    .await
}

async fn invoke_with_snapshot<F>(
    runtime: &Path,
    args: &[OsString],
    environment: &BTreeMap<OsString, OsString>,
    timeout: Duration,
    run_guard: Option<(&CtClient, Uuid)>,
    mut snapshot: F,
) -> Result<RuntimeOutput, CtError>
where
    F: FnMut() -> Option<Vec<(u32, u32, char)>>,
{
    let mut command = tokio::process::Command::new("python3");
    command
        .arg("-I")
        .arg(runtime)
        .args(args)
        .env_clear()
        .envs(environment)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = command
        .spawn()
        .map_err(|_| CtError::Usage("could not start embedded discovery runtime".into()))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| CtError::Usage("embedded discovery stdout was unavailable".into()))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| CtError::Usage("embedded discovery stderr was unavailable".into()))?;
    let auth_values: Vec<String> = environment
        .iter()
        .filter(|(name, _)| {
            name.to_string_lossy()
                .starts_with("CT_CYBER_DISCOVERY_AUTH_")
        })
        .map(|(_, value)| value.to_string_lossy().into_owned())
        .collect();
    let process_id = child.id();
    let mut execution = Box::pin(async {
        tokio::try_join!(
            child.wait(),
            read_bounded(stdout, PROCESS_OUTPUT_LIMIT),
            stream_stderr(stderr, PROCESS_OUTPUT_LIMIT, auth_values),
        )
    });
    let stopped = async {
        match run_guard {
            Some((client, run_id)) => Err(wait_for_run_stop(client, run_id).await),
            None => std::future::pending::<Result<(), CtError>>().await,
        }
    };
    tokio::pin!(stopped);
    let finished = tokio::time::timeout(timeout, async {
        tokio::select! {
            result = &mut execution => result.map_err(|error| CtError::Transport(error.to_string())),
            stop = &mut stopped => match stop {
                Err(error) => Err(error),
                Ok(()) => Err(CtError::Protocol("local run status watcher exited unexpectedly".into())),
            },
        }
    })
    .await;
    let (status, stdout, stderr) = match finished {
        Ok(Ok(output)) => output,
        Ok(Err(error)) => {
            drop(execution);
            stop_runtime_process(&mut child, process_id, &mut snapshot).await;
            return Err(error);
        }
        Err(_) => {
            drop(execution);
            stop_runtime_process(&mut child, process_id, &mut snapshot).await;
            return Err(CtError::Timeout(
                "local Cyber discovery exceeded its deadline".into(),
            ));
        }
    };
    Ok(RuntimeOutput {
        status,
        stdout,
        stderr,
    })
}

pub(super) async fn stop_runtime_process<F>(
    child: &mut tokio::process::Child,
    process_id: Option<u32>,
    snapshot: &mut F,
) where
    F: FnMut() -> Option<Vec<(u32, u32, char)>>,
{
    #[cfg(unix)]
    if let (Some(root_pid), Some(root)) = (process_id, runtime_pid(process_id)) {
        let _ = rustix::process::kill_process(root, rustix::process::Signal::STOP);
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        let mut observed = std::collections::BTreeSet::new();
        let mut to_kill = std::collections::BTreeSet::new();
        let mut previous_quiescent = false;
        loop {
            let quiescent = if let Some(processes) = snapshot() {
                let descendants = process_descendant_states(root_pid, &processes);
                let mut new_descendant = false;
                for (pid, _) in &descendants {
                    new_descendant |= observed.insert(*pid);
                }
                let process_states = processes
                    .iter()
                    .map(|(pid, _, state)| (*pid, *state))
                    .collect::<BTreeMap<_, _>>();
                for pid in &observed {
                    if let Some(state) = process_states.get(pid)
                        && !process_state_is_quiescent(*state)
                        && let Some(process) = runtime_pid(Some(*pid))
                    {
                        let _ =
                            rustix::process::kill_process(process, rustix::process::Signal::STOP);
                    }
                }
                to_kill = observed
                    .iter()
                    .filter(|pid| {
                        process_states
                            .get(*pid)
                            .is_some_and(|state| !matches!(*state, 'Z' | 'X'))
                    })
                    .copied()
                    .collect();
                let root_stopped = processes
                    .iter()
                    .find(|(pid, _, _)| *pid == root_pid)
                    .map(|(_, _, state)| process_state_is_quiescent(*state))
                    .unwrap_or(true);
                let descendants_stopped = observed.iter().all(|pid| {
                    process_states
                        .get(pid)
                        .map(|state| process_state_is_quiescent(*state))
                        .unwrap_or(true)
                });
                Some(root_stopped && descendants_stopped && !new_descendant)
            } else {
                None
            };
            if quiescent == Some(true) && previous_quiescent {
                break;
            }
            previous_quiescent = quiescent == Some(true);
            if std::time::Instant::now() >= deadline {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        for pid in to_kill {
            if let Some(process) = runtime_pid(Some(pid)) {
                let _ = rustix::process::kill_process(process, rustix::process::Signal::KILL);
            }
        }
        let _ = rustix::process::kill_process(root, rustix::process::Signal::KILL);
    } else {
        let _ = child.start_kill();
    }
    #[cfg(not(unix))]
    {
        let _ = process_id;
        let _ = snapshot;
        let _ = child.start_kill();
    }
    let _ = tokio::time::timeout(Duration::from_secs(2), child.wait()).await;
}

#[cfg(unix)]
fn runtime_pid(process_id: Option<u32>) -> Option<rustix::process::Pid> {
    process_id
        .and_then(|pid| i32::try_from(pid).ok())
        .and_then(rustix::process::Pid::from_raw)
}

#[cfg(unix)]
pub(super) fn process_table_snapshot() -> Option<Vec<(u32, u32, char)>> {
    let output = match std::process::Command::new("ps")
        .args(["-axo", "pid=,ppid=,stat="])
        .output()
    {
        Ok(output) if output.status.success() => output,
        _ => return None,
    };
    let processes = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let (Some(pid), Some(parent), Some(state)) =
                (fields.next(), fields.next(), fields.next())
            else {
                return None;
            };
            let (Ok(pid), Ok(parent)) = (pid.parse::<u32>(), parent.parse::<u32>()) else {
                return None;
            };
            Some((pid, parent, state.chars().next()?))
        })
        .collect();
    Some(processes)
}

#[cfg(not(unix))]
pub(super) fn process_table_snapshot() -> Option<Vec<(u32, u32, char)>> {
    None
}

#[cfg(unix)]
fn process_descendant_states(root: u32, processes: &[(u32, u32, char)]) -> Vec<(u32, char)> {
    let mut children = BTreeMap::<u32, Vec<(u32, char)>>::new();
    for (pid, parent, state) in processes {
        children.entry(*parent).or_default().push((*pid, *state));
    }
    let mut pending = children.get(&root).cloned().unwrap_or_default();
    let mut descendants = Vec::new();
    while let Some((pid, state)) = pending.pop() {
        pending.extend(children.get(&pid).into_iter().flatten().copied());
        descendants.push((pid, state));
    }
    descendants
}

#[cfg(unix)]
fn process_state_is_quiescent(state: char) -> bool {
    matches!(state, 'T' | 't' | 'Z' | 'X')
}

async fn read_bounded<R: tokio::io::AsyncRead + Unpin>(
    mut reader: R,
    limit: usize,
) -> std::io::Result<Vec<u8>> {
    use tokio::io::AsyncReadExt;
    let mut tail = std::collections::VecDeque::with_capacity(limit);
    let mut buffer = [0u8; 4096];
    loop {
        let count = reader.read(&mut buffer).await?;
        if count == 0 {
            break;
        }
        for byte in &buffer[..count] {
            if tail.len() == limit {
                tail.pop_front();
            }
            tail.push_back(*byte);
        }
    }
    Ok(tail.into_iter().collect())
}

pub(super) async fn stream_stderr<R: tokio::io::AsyncRead + Unpin>(
    mut reader: R,
    limit: usize,
    auth_values: Vec<String>,
) -> std::io::Result<Vec<u8>> {
    use tokio::io::AsyncReadExt;
    let mut tail = std::collections::VecDeque::with_capacity(limit);
    let mut line = Vec::new();
    let mut buffer = [0u8; 4096];
    let mut overflow = false;
    loop {
        let count = reader.read(&mut buffer).await?;
        if count == 0 {
            if !line.is_empty() {
                emit_stderr_line(&line, &auth_values, &mut tail, limit, overflow);
            }
            break;
        }
        for byte in &buffer[..count] {
            if *byte == b'\n' {
                emit_stderr_line(&line, &auth_values, &mut tail, limit, overflow);
                line.clear();
                overflow = false;
            } else if line.len() < limit && !overflow {
                line.push(*byte);
            } else {
                line.clear();
                overflow = true;
            }
        }
    }
    Ok(tail.into_iter().collect())
}

fn emit_stderr_line(
    line: &[u8],
    auth_values: &[String],
    tail: &mut std::collections::VecDeque<u8>,
    limit: usize,
    overflow: bool,
) {
    let text = if overflow {
        "[discovery progress line exceeded limit]".to_string()
    } else {
        String::from_utf8_lossy(&cyber_discovery_redact(line, auth_values)).into_owned()
    };
    let safe = output::terminal_text(&text);
    eprintln!("{safe}");
    for byte in safe.bytes().chain(std::iter::once(b'\n')) {
        if tail.len() == limit {
            tail.pop_front();
        }
        tail.push_back(byte);
    }
}
