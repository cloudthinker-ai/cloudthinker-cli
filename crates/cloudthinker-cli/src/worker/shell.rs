use std::collections::BTreeMap;
use std::process::Stdio;
use std::time::Duration;

use cap_std::fs::Dir;
use chrono::{DateTime, Utc};
use cloudthinker_client::worker_types as api;
use rustix::process::{Pid, Signal, kill_process_group};
use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;
use tokio_util::sync::CancellationToken;
use tokio_util::task::AbortOnDropHandle;

use super::files::relative;

const TAIL_BYTES: usize = 65536;

struct ProcessGroup(Pid);

impl Drop for ProcessGroup {
    fn drop(&mut self) {
        let _ = kill_process_group(self.0, Signal::KILL);
    }
}

pub fn environment(
    extra: &[String],
    inherit: bool,
) -> Result<BTreeMap<String, String>, &'static str> {
    if extra.iter().any(|key| {
        key.is_empty()
            || !key
                .bytes()
                .enumerate()
                .all(|(i, b)| b.is_ascii_alphabetic() || b == b'_' || (i > 0 && b.is_ascii_digit()))
    }) {
        return Err("INVALID_ENVIRONMENT_KEY");
    }
    let mut keys = vec!["PATH", "HOME", "LANG", "TERM", "USER", "TMPDIR"];
    keys.extend(extra.iter().map(String::as_str));
    let mut env: BTreeMap<_, _> = std::env::vars()
        .filter(|(key, _)| inherit || keys.contains(&key.as_str()))
        .collect();
    env.retain(|key, _| {
        !key.starts_with("CLOUDTHINKER_")
            && !key.starts_with("HERDR_")
            && key != "BASH_ENV"
            && key != "ENV"
    });
    Ok(env)
}

#[cfg(target_os = "linux")]
fn working_directory(dir: &Dir) -> Result<std::path::PathBuf, &'static str> {
    use std::os::fd::AsRawFd;
    Ok(format!("/proc/self/fd/{}", dir.as_raw_fd()).into())
}

#[cfg(target_vendor = "apple")]
fn working_directory(dir: &Dir) -> Result<std::path::PathBuf, &'static str> {
    use std::os::unix::ffi::OsStringExt;
    let path = rustix::fs::getpath(dir).map_err(|_| "TRUSTED_ROOT_REJECTED")?;
    Ok(std::ffi::OsString::from_vec(path.into_bytes()).into())
}

#[cfg(not(any(target_os = "linux", target_vendor = "apple")))]
fn working_directory(dir: &Dir) -> Result<std::path::PathBuf, &'static str> {
    use std::os::fd::AsRawFd;
    Ok(format!("/dev/fd/{}", dir.as_raw_fd()).into())
}

pub async fn execute(
    dir: &Dir,
    script: &api::ScriptRun,
    environment: &BTreeMap<String, String>,
    deadline: DateTime<Utc>,
    cancel: CancellationToken,
) -> Result<Value, &'static str> {
    if script.credential_ref.is_some() {
        return Err("EXECUTOR_CREDENTIAL_NOT_LOCAL");
    }
    let cwd = dir
        .open_dir(relative(
            script.working_directory.as_deref().unwrap_or("."),
        )?)
        .map_err(|_| "TRUSTED_ROOT_REJECTED")?;
    let cwd_path = working_directory(&cwd)?;
    let started = std::time::Instant::now();
    let duration = (deadline - Utc::now()).to_std().map_err(|_| "TIMEOUT")?;
    let duration = script
        .timeout
        .filter(|t| t.is_finite() && *t > 0.0)
        .map(|t| duration.min(Duration::from_secs_f64(t.min(3600.0))))
        .unwrap_or(duration);
    let mut child = Command::new("/bin/bash")
        .arg("-c")
        .arg(&script.script)
        .current_dir(cwd_path)
        .env_clear()
        .envs(environment)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .kill_on_drop(true)
        .spawn()
        .map_err(|_| "SHELL_UNAVAILABLE")?;
    let pid = child
        .id()
        .and_then(|id| i32::try_from(id).ok())
        .and_then(Pid::from_raw)
        .ok_or("SHELL_UNAVAILABLE")?;
    let group = ProcessGroup(pid);
    let stdout = child.stdout.take().ok_or("SHELL_UNAVAILABLE")?;
    let stderr = child.stderr.take().ok_or("SHELL_UNAVAILABLE")?;
    let mut stdout_task = AbortOnDropHandle::new(tokio::spawn(tail(stdout)));
    let mut stderr_task = AbortOnDropHandle::new(tokio::spawn(tail(stderr)));
    let (status, reason) = tokio::select! {
        status = child.wait() => (status.ok(),None),
        _ = cancel.cancelled() => (None,Some("CANCELLED")),
        _ = tokio::time::sleep(duration) => (None,Some("TIMEOUT")),
    };
    let mut incomplete = false;
    let status = if reason.is_some() {
        if kill_process_group(group.0, Signal::KILL).is_err() {
            incomplete = true;
        }
        match tokio::time::timeout(Duration::from_secs(5), child.wait()).await {
            Ok(Ok(status)) => Some(status),
            _ => {
                incomplete = true;
                None
            }
        }
    } else {
        status
    };
    drop(group);
    let mut text = Vec::new();
    for handle in [&mut stdout_task, &mut stderr_task] {
        match tokio::time::timeout(Duration::from_secs(5), &mut *handle).await {
            Ok(Ok(Ok(output))) => text.push(output),
            _ => {
                handle.abort();
                incomplete = true;
                text.push(String::new());
            }
        }
    }
    Ok(
        json!({"result":{"return_code":status.and_then(|s|s.code()).unwrap_or(-1),"stdout":text[0],"stderr":text[1],"duration":started.elapsed().as_secs_f64(),"error_code":reason,"cancel_incomplete":incomplete}}),
    )
}

async fn tail(mut reader: impl AsyncRead + Unpin) -> Result<String, std::io::Error> {
    let mut tail = Vec::with_capacity(TAIL_BYTES * 2);
    let mut buffer = [0; 8192];
    loop {
        let count = reader.read(&mut buffer).await?;
        if count == 0 {
            break;
        }
        tail.extend_from_slice(&buffer[..count]);
        if tail.len() > TAIL_BYTES * 2 {
            tail.drain(..tail.len() - TAIL_BYTES);
        }
    }
    if tail.len() > TAIL_BYTES {
        tail.drain(..tail.len() - TAIL_BYTES);
    }
    Ok(String::from_utf8_lossy(&tail).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn ca_wo_10_cwd_bounded_output_and_deadline() {
        let root = tempfile::tempdir().unwrap();
        let dir = Dir::open_ambient_dir(root.path(), cap_std::ambient_authority()).unwrap();
        let script: api::ScriptRun = serde_json::from_value(json!({"kind":"script","script":"printf persisted > state; head -c 100000 /dev/zero | tr '\\0' x; printf TAIL"})).unwrap();
        let result = execute(
            &dir,
            &script,
            &BTreeMap::new(),
            Utc::now() + chrono::Duration::seconds(5),
            CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(root.path().join("state")).unwrap(),
            "persisted"
        );
        assert_eq!(result["result"]["stdout"].as_str().unwrap().len(), 65536);
        assert!(
            result["result"]["stdout"]
                .as_str()
                .unwrap()
                .ends_with("TAIL")
        );
        let script: api::ScriptRun =
            serde_json::from_value(json!({"kind":"script","script":"sleep 30 & wait"})).unwrap();
        let result = execute(
            &dir,
            &script,
            &BTreeMap::new(),
            Utc::now() + chrono::Duration::milliseconds(100),
            CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(result["result"]["error_code"], "TIMEOUT");
    }

    #[tokio::test]
    async fn ca_wo_11_cancellation_kills_the_group_and_37_records_cancel_incomplete_false() {
        let root = tempfile::tempdir().unwrap();
        let dir = Dir::open_ambient_dir(root.path(), cap_std::ambient_authority()).unwrap();
        let script: api::ScriptRun = serde_json::from_value(
            json!({"kind":"script","script":"printf running > marker; sleep 30 & wait"}),
        )
        .unwrap();
        let cancel = CancellationToken::new();
        let stopping = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(200)).await;
            stopping.cancel();
        });
        let started = std::time::Instant::now();
        let result = execute(
            &dir,
            &script,
            &BTreeMap::new(),
            Utc::now() + chrono::Duration::seconds(60),
            cancel,
        )
        .await
        .unwrap();
        assert!(started.elapsed() < Duration::from_secs(10));
        assert_eq!(result["result"]["error_code"], "CANCELLED");
        assert_eq!(result["result"]["cancel_incomplete"], false);
        assert_eq!(result["result"]["return_code"], -1);
        assert!(root.path().join("marker").is_file());
    }

    #[tokio::test]
    async fn a_credential_reference_is_never_resolved_locally() {
        let root = tempfile::tempdir().unwrap();
        let dir = Dir::open_ambient_dir(root.path(), cap_std::ambient_authority()).unwrap();
        let script: api::ScriptRun = serde_json::from_value(json!({
            "kind": "script",
            "script": "true",
            "credential_ref": uuid::Uuid::new_v4(),
        }))
        .unwrap();
        assert_eq!(
            execute(
                &dir,
                &script,
                &BTreeMap::new(),
                Utc::now() + chrono::Duration::seconds(5),
                CancellationToken::new(),
            )
            .await,
            Err("EXECUTOR_CREDENTIAL_NOT_LOCAL")
        );
    }

    #[test]
    fn environment_rejects_bad_keys_and_strips_worker_owned_ones() {
        for key in ["", "1BAD", "BAD-KEY", "BAD KEY"] {
            assert_eq!(
                environment(&[key.to_owned()], false),
                Err("INVALID_ENVIRONMENT_KEY")
            );
        }
        let allowlisted = environment(&["EXTRA_KEY".into()], false).unwrap();
        let inherited = environment(&[], true).unwrap();
        assert!(allowlisted.len() <= 6);
        assert!(allowlisted.keys().all(|key| inherited.contains_key(key)));
        for env in [&allowlisted, &inherited] {
            assert!(!env.keys().any(|key| key.starts_with("CLOUDTHINKER_")));
            assert!(!env.keys().any(|key| key.starts_with("HERDR_")));
            assert!(!env.contains_key("BASH_ENV"));
            assert!(!env.contains_key("ENV"));
        }
    }
}
