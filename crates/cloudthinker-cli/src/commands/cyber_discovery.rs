use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::time::Duration;

use cloudthinker_client::{
    CtClient, CtError, CyberDiscoveryArtifacts, CyberDiscoveryHealth, CyberDiscoveryManifest,
    CyberExecutionHost, CyberRunResult, cyber_discovery_artifacts, cyber_discovery_auth_value,
    cyber_discovery_identity_manifest, cyber_discovery_redact, wait_for_run_stop,
};
use fs2::FileExt;
use serde::Serialize;
use uuid::Uuid;

use crate::engine::{exit, exit::ExitCode, output};

pub(super) const RUNTIME_BYTES: &[u8] = include_bytes!("../../runtime/cyber-discovery.pyz");
const MAX_CONTEXT_ENTRIES: usize = 20_000;
const MAX_CONTEXT_DEPTH: usize = 64;
const MAX_CONTEXT_FILE_BYTES: u64 = 10 * 1024 * 1024;
const MAX_CONTEXT_TOTAL_BYTES: u64 = 100 * 1024 * 1024;
const PROCESS_OUTPUT_LIMIT: usize = 16 * 1024;

struct RunLock(File);

impl Drop for RunLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.0);
    }
}

struct RunRoots {
    root: PathBuf,
    runtime: PathBuf,
    _lock: RunLock,
}

struct RuntimeOutput {
    status: ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

#[derive(Serialize)]
struct DiscoveryOutput<'a> {
    run_id: Uuid,
    health: &'a CyberDiscoveryHealth,
    artifacts: String,
    context_limits: Vec<String>,
}

#[derive(Serialize)]
struct LocalWorkspaceManifest {
    version: u8,
    run_id: Uuid,
    app_id: Uuid,
    conversation_id: Uuid,
    workspace_root: String,
    evidence_root: String,
    workflow_root: String,
}

pub(super) fn prepare_local_workspace(
    run_id: Uuid,
    app_id: Uuid,
    conversation_id: Uuid,
) -> Result<cloudthinker_client::LocalCyberWorkspace, String> {
    let run_root = state_root()?.join(run_id.to_string());
    prepare_local_workspace_at(&run_root, run_id, app_id, conversation_id)
}

fn prepare_local_workspace_at(
    run_root: &Path,
    run_id: Uuid,
    app_id: Uuid,
    conversation_id: Uuid,
) -> Result<cloudthinker_client::LocalCyberWorkspace, String> {
    create_private_dir(run_root)?;
    let workspace = run_root.join("workspace");
    let evidence = workspace.join("evidence");
    let workflow = workspace.join("workflow");
    create_private_dir(&workspace)?;
    create_private_dir(&evidence)?;
    create_private_dir(&workflow)?;
    let manifest = workspace.join("manifest.json");
    let paths = cloudthinker_client::LocalCyberWorkspace {
        workspace_root: workspace.display().to_string(),
        evidence_root: evidence.display().to_string(),
        workflow_root: workflow.display().to_string(),
        manifest: manifest.display().to_string(),
    };
    let value = LocalWorkspaceManifest {
        version: 1,
        run_id,
        app_id,
        conversation_id,
        workspace_root: paths.workspace_root.clone(),
        evidence_root: paths.evidence_root.clone(),
        workflow_root: paths.workflow_root.clone(),
    };
    let bytes = serde_json::to_vec_pretty(&value)
        .map_err(|error| format!("cannot encode local Cyber workspace manifest: {error}"))?;
    write_private_file(&manifest, &bytes)?;
    Ok(paths)
}

pub async fn discover(
    base_url: &str,
    workspace: Option<&str>,
    run_id: Uuid,
    max_urls: u64,
    timeout: u64,
    json: bool,
) -> ExitCode {
    if !(1..=20_000).contains(&max_urls) || !(1..=720).contains(&timeout) {
        output::eprintln_error("discovery bounds are max-urls 1..20000 and timeout 1..720 seconds");
        return ExitCode::Usage;
    }
    if !super::cyber_doctor::discovery_python_ready() {
        output::eprintln_error(
            "local discovery requires Python 3.10 or newer; see `cloudthinker cyber doctor`",
        );
        return ExitCode::JobFailed;
    }
    let client = match super::build_client(base_url, workspace) {
        Ok(client) => client,
        Err(error) => return exit::report(&error),
    };
    let run = match client.cyber_get_run(run_id).await {
        Ok(run) => run,
        Err(error) => return exit::report(&error),
    };
    if run.execution_host != CyberExecutionHost::Local || run.result != CyberRunResult::Running {
        output::eprintln_error("discovery requires a RUNNING local Cyber run");
        return ExitCode::Usage;
    }
    let Some(conversation_id) = run.conversation_id else {
        output::eprintln_error(
            "bind this local run to the current CloudThinker session before discovery",
        );
        return ExitCode::Usage;
    };
    let brief = match client.cyber_bind_run(run_id, conversation_id).await {
        Ok(brief) => brief,
        Err(error) => return exit::report(&error),
    };
    if brief.run_id != run_id
        || brief.execution_host != CyberExecutionHost::Local
        || brief.result != CyberRunResult::Running
    {
        output::eprintln_error("CloudThinker returned a different or non-running local run brief");
        return ExitCode::JobFailed;
    }
    if brief.targets.len() != 1 {
        output::eprintln_error(
            "the embedded discovery runtime currently supports exactly one frozen run target",
        );
        return ExitCode::Usage;
    }
    let roots = match RunRoots::open(run_id) {
        Ok(roots) => roots,
        Err(error) => {
            output::eprintln_error(&error);
            return ExitCode::JobFailed;
        }
    };
    if let Err(error) = ensure_runtime(&roots.runtime) {
        output::eprintln_error(&error);
        return ExitCode::JobFailed;
    }
    let path = match super::cyber_doctor::installed_tool_directories() {
        Ok(directories) => match child_path(directories) {
            Ok(path) => path,
            Err(error) => {
                output::eprintln_error(&error);
                return ExitCode::JobFailed;
            }
        },
        Err(error) => {
            output::eprintln_error(&error);
            return ExitCode::JobFailed;
        }
    };
    let environment = match child_environment(path) {
        Ok(environment) => environment,
        Err(error) => {
            output::eprintln_error(&error.to_string());
            return ExitCode::Usage;
        }
    };
    let output_result =
        run_or_reuse(&client, &brief, &roots, &environment, max_urls, timeout).await;
    let (health, artifacts, artifacts_path, runner_succeeded) = match output_result {
        Ok(result) => result,
        Err(error) => return exit::report(&error),
    };
    let current_run = match client.cyber_get_run(run_id).await {
        Ok(run) => run,
        Err(error) => return exit::report(&error),
    };
    if current_run.result != CyberRunResult::Running {
        output::eprintln_error(
            "local Cyber run stopped before discovery evidence could be uploaded",
        );
        return ExitCode::JobFailed;
    }
    if let Err(error) = client.cyber_publish_discovery(run_id, artifacts).await {
        return exit::report(&error);
    }
    let result = DiscoveryOutput {
        run_id,
        health: &health,
        artifacts: artifacts_path.display().to_string(),
        context_limits: super::cyber_config::context_items()
            .into_iter()
            .filter(|(_, source)| source.starts_with("http://") || source.starts_with("https://"))
            .map(|(label, _)| format!("context `{label}` is a URL reference; save relevant material locally to include it"))
            .collect(),
    };
    let render = if json {
        output::emit_json(&result)
    } else {
        render_health(&result)
    };
    if let Err(error) = render {
        output::eprintln_error(&error);
        return ExitCode::JobFailed;
    }
    if !runner_succeeded || health.overall_state == "FAILED" {
        ExitCode::JobFailed
    } else {
        ExitCode::Ok
    }
}

async fn run_or_reuse(
    client: &CtClient,
    brief: &cloudthinker_client::CyberRunBrief,
    roots: &RunRoots,
    environment: &BTreeMap<OsString, OsString>,
    max_urls: u64,
    timeout: u64,
) -> Result<(CyberDiscoveryHealth, CyberDiscoveryArtifacts, PathBuf, bool), CtError> {
    let version = runtime_version(&roots.runtime, environment).await?;
    if let Some((health, artifacts, path)) =
        try_reuse_latest(brief.run_id, roots, environment).await
        && health.overall_state != "FAILED"
    {
        return Ok((health, artifacts, path, true));
    }
    let attempt_id = Uuid::new_v4();
    let attempt = roots.root.join(format!("attempt-{attempt_id}"));
    create_private_dir(&attempt).map_err(local_error)?;
    let context_dir = attempt.join("context");
    create_private_dir(&context_dir).map_err(local_error)?;
    let config = super::cyber_config::effective_values();
    let cwd = std::env::current_dir().map_err(local_error)?;
    let workspace_dir = match config.get("repo_path") {
        Some(path) => {
            let path = PathBuf::from(path);
            let path = if path.is_absolute() {
                path
            } else {
                cwd.join(path)
            };
            let canonical = path.canonicalize().map_err(|error| {
                CtError::Usage(format!(
                    "configured Cyber repo_path cannot be read at {}: {error}",
                    path.display()
                ))
            })?;
            if !canonical.is_dir() {
                return Err(CtError::Usage(format!(
                    "configured Cyber repo_path is not a directory: {}",
                    canonical.display()
                )));
            }
            canonical
        }
        None => cwd.clone(),
    };
    let (repository_roots, repository_available) = if workspace_dir.is_dir() {
        (vec![workspace_dir.display().to_string()], true)
    } else {
        (Vec::new(), false)
    };
    let mut context_budget = 0u64;
    let mut context_entries = 0usize;
    for (index, (label, source)) in super::cyber_config::context_items().iter().enumerate() {
        if source.starts_with("http://") || source.starts_with("https://") {
            eprintln!(
                "discovery context `{label}` is a URL reference; save relevant material locally to include it"
            );
            continue;
        }
        let source = PathBuf::from(source);
        let source = if source.is_absolute() {
            source
        } else {
            cwd.join(source)
        };
        if !source.exists() {
            return Err(CtError::Usage(format!(
                "Cyber context `{label}` does not exist at {}",
                source.display()
            )));
        }
        copy_context_source(
            &source,
            &context_dir.join(format!("context-{index}")),
            &mut context_entries,
            &mut context_budget,
        )
        .map_err(local_error)?;
    }
    let auth_identities = super::cyber_config::auth_identities();
    let mut identity_refs = Vec::new();
    let mut identity_env = BTreeMap::new();
    if auth_identities.len() > 20 {
        return Err(CtError::Usage(
            "local discovery supports at most 20 configured Cyber identities".into(),
        ));
    }
    for (index, (label, _source_env)) in auth_identities.iter().enumerate() {
        let child_env = format!("CT_CYBER_DISCOVERY_AUTH_{index}");
        identity_refs.push((label.clone(), child_env.clone()));
        let (name, value) = super::cyber_config::probe_auth_header(Some(label))
            .map_err(CtError::Usage)?
            .ok_or_else(|| {
                CtError::Usage(format!(
                    "Cyber identity `{label}` has no authentication header"
                ))
            })?;
        let auth = cyber_discovery_auth_value(&format!("{name}: {value}"))?;
        identity_env.insert(OsString::from(child_env), OsString::from(auth));
    }
    identity_env.insert(
        OsString::from("CYBER_IDENTITIES_JSON"),
        OsString::from(cyber_discovery_identity_manifest(&identity_refs)?),
    );
    let target = brief.targets[0].clone();
    let manifest = CyberDiscoveryManifest {
        version,
        run_id: brief.run_id,
        targets: vec![target],
        frameworks: brief.frameworks.clone(),
        mode: brief.mode.clone(),
        intensity: brief.intensity.clone(),
        context_dir: context_dir.display().to_string(),
        workspace_dir: workspace_dir.display().to_string(),
        repository_roots,
        identity_labels: identity_refs
            .iter()
            .map(|(label, _)| label.clone())
            .collect(),
        repository_available,
    };
    write_private_file(
        &attempt.join("manifest.json"),
        manifest.to_json()?.as_bytes(),
    )
    .map_err(local_error)?;
    let discovery_dir = attempt.join("discovery");
    create_private_dir(&discovery_dir).map_err(local_error)?;
    let mut run_environment = environment.clone();
    run_environment.extend(identity_env);
    let run_output = invoke_runtime_with_guard(
        &roots.runtime,
        &[
            OsString::from("run"),
            OsString::from("--manifest"),
            attempt.join("manifest.json").into_os_string(),
            OsString::from("--discovery-dir"),
            discovery_dir.clone().into_os_string(),
            OsString::from("--max-urls"),
            OsString::from(max_urls.to_string()),
            OsString::from("--timeout"),
            OsString::from(timeout.to_string()),
        ],
        &run_environment,
        Duration::from_secs(timeout.saturating_add(30)),
        Some((client, brief.run_id)),
    )
    .await?;
    let validation = validate_attempt(
        &roots.runtime,
        &attempt.join("manifest.json"),
        &discovery_dir.join("report.full.json"),
        environment,
    )
    .await;
    if validation.is_err() {
        let detail = if run_output.status.success() {
            "embedded discovery did not produce a canonically valid report"
        } else {
            "embedded discovery failed before producing a canonically valid report"
        };
        return Err(CtError::Usage(format!(
            "{detail}; attempt retained at {}: {}",
            attempt.display(),
            String::from_utf8_lossy(&run_output.stderr)
        )));
    }
    let (health, artifacts) = cyber_discovery_artifacts(brief.run_id, &discovery_dir)?;
    write_latest(&roots.root, attempt_id).map_err(local_error)?;
    Ok((
        health,
        artifacts,
        discovery_dir,
        run_output.status.success(),
    ))
}

async fn try_reuse_latest(
    run_id: Uuid,
    roots: &RunRoots,
    environment: &BTreeMap<OsString, OsString>,
) -> Option<(CyberDiscoveryHealth, CyberDiscoveryArtifacts, PathBuf)> {
    let value = fs::read_to_string(roots.root.join("latest")).ok()?;
    let attempt_id = Uuid::parse_str(value.trim()).ok()?;
    let attempt = roots.root.join(format!("attempt-{attempt_id}"));
    let manifest = attempt.join("manifest.json");
    let report = attempt.join("discovery").join("report.full.json");
    if validate_attempt(&roots.runtime, &manifest, &report, environment)
        .await
        .is_err()
    {
        return None;
    }
    let (health, artifacts) = cyber_discovery_artifacts(run_id, &attempt.join("discovery")).ok()?;
    Some((health, artifacts, attempt.join("discovery")))
}

async fn runtime_version(
    runtime: &Path,
    environment: &BTreeMap<OsString, OsString>,
) -> Result<u64, CtError> {
    let output = invoke_runtime(
        runtime,
        &[OsString::from("version")],
        environment,
        Duration::from_secs(10),
    )
    .await?;
    if !output.status.success() {
        return Err(CtError::Usage(
            "embedded discovery runtime version probe failed".into(),
        ));
    }
    std::str::from_utf8(&output.stdout)
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .ok_or_else(|| {
            CtError::Usage("embedded discovery runtime returned an invalid version".into())
        })
}

async fn validate_attempt(
    runtime: &Path,
    manifest: &Path,
    report: &Path,
    environment: &BTreeMap<OsString, OsString>,
) -> Result<(), CtError> {
    let output = invoke_runtime(
        runtime,
        &[
            OsString::from("validate"),
            OsString::from("--manifest"),
            manifest.as_os_str().to_os_string(),
            OsString::from("--report"),
            report.as_os_str().to_os_string(),
        ],
        environment,
        Duration::from_secs(30),
    )
    .await?;
    if output.status.success() {
        Ok(())
    } else {
        Err(CtError::Usage(
            "discovery report failed canonical validation".into(),
        ))
    }
}

async fn invoke_runtime(
    runtime: &Path,
    args: &[OsString],
    environment: &BTreeMap<OsString, OsString>,
    timeout: Duration,
) -> Result<RuntimeOutput, CtError> {
    invoke_runtime_with_guard(runtime, args, environment, timeout, None).await
}

async fn invoke_runtime_with_guard(
    runtime: &Path,
    args: &[OsString],
    environment: &BTreeMap<OsString, OsString>,
    timeout: Duration,
    run_guard: Option<(&CtClient, Uuid)>,
) -> Result<RuntimeOutput, CtError> {
    invoke_runtime_with_snapshot(
        runtime,
        args,
        environment,
        timeout,
        run_guard,
        process_table_snapshot,
    )
    .await
}

async fn invoke_runtime_with_snapshot<F>(
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

async fn stop_runtime_process<F>(
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
fn process_table_snapshot() -> Option<Vec<(u32, u32, char)>> {
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
fn process_table_snapshot() -> Option<Vec<(u32, u32, char)>> {
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

async fn stream_stderr<R: tokio::io::AsyncRead + Unpin>(
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

fn child_path(mut directories: Vec<PathBuf>) -> Result<OsString, String> {
    if let Some(path) = std::env::var_os("PATH") {
        directories.extend(std::env::split_paths(&path));
    }
    std::env::join_paths(directories)
        .map_err(|error| format!("could not construct discovery PATH: {error}"))
}

fn child_environment(path: OsString) -> Result<BTreeMap<OsString, OsString>, CtError> {
    let mut environment = BTreeMap::new();
    environment.insert(OsString::from("PATH"), path);
    for name in ["HOME", "LANG", "TMPDIR"] {
        if let Some(value) = std::env::var_os(name) {
            environment.insert(OsString::from(name), value);
        }
    }
    if !environment.contains_key(OsStr::new("TMPDIR")) {
        environment.insert(
            OsString::from("TMPDIR"),
            std::env::temp_dir().into_os_string(),
        );
    }
    Ok(environment)
}

fn ensure_runtime(path: &Path) -> Result<(), String> {
    if fs::read(path).ok().as_deref() == Some(RUNTIME_BYTES) {
        return Ok(());
    }
    write_private_file(path, RUNTIME_BYTES)
}

impl RunRoots {
    fn open(run_id: Uuid) -> Result<Self, String> {
        let root = state_root()?.join(run_id.to_string());
        Self::open_at(root)
    }

    fn open_at(root: PathBuf) -> Result<Self, String> {
        create_private_dir(&root)?;
        let lock_path = root.join("run.lock");
        let lock_file = open_private_file(&lock_path)?;
        lock_file
            .try_lock_exclusive()
            .map_err(|_| "another discovery command is active for this run".to_string())?;
        Ok(Self {
            runtime: root.join("cyber-discovery.pyz"),
            root,
            _lock: RunLock(lock_file),
        })
    }
}

fn state_root() -> Result<PathBuf, String> {
    let root = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .map(|path| path.join(".local/state"))
        })
        .ok_or_else(|| "could not resolve the local CloudThinker state directory".to_string())?;
    let root = root.join("cloudthinker").join("cyber").join("runs");
    create_private_dir(&root)?;
    Ok(root)
}

fn create_private_dir(path: &Path) -> Result<(), String> {
    fs::create_dir_all(path)
        .map_err(|error| format!("cannot create {}: {error}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .map_err(|error| format!("cannot secure {}: {error}", path.display()))?;
    }
    Ok(())
}

fn open_private_file(path: &Path) -> Result<File, String> {
    let mut options = OpenOptions::new();
    options.create(true).read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
        .open(path)
        .map_err(|error| format!("cannot open {}: {error}", path.display()))
}

fn write_private_file(path: &Path, content: &[u8]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        create_private_dir(parent)?;
    }
    let temporary = path.with_extension(format!("tmp-{}", Uuid::new_v4()));
    let mut file = open_private_file(&temporary)?;
    use std::io::Write;
    file.write_all(content)
        .and_then(|()| file.sync_all())
        .map_err(|error| format!("cannot write {}: {error}", temporary.display()))?;
    fs::rename(&temporary, path)
        .map_err(|error| format!("cannot install {}: {error}", path.display()))
}

fn write_latest(root: &Path, attempt_id: Uuid) -> Result<(), String> {
    write_private_file(&root.join("latest"), attempt_id.to_string().as_bytes())
}

fn copy_context_source(
    source: &Path,
    destination: &Path,
    entry_count: &mut usize,
    total_bytes: &mut u64,
) -> Result<(), String> {
    copy_context_entry(source, destination, entry_count, total_bytes, 0)
}

fn copy_context_entry(
    source: &Path,
    destination: &Path,
    entry_count: &mut usize,
    total_bytes: &mut u64,
    depth: usize,
) -> Result<(), String> {
    if depth > MAX_CONTEXT_DEPTH || *entry_count >= MAX_CONTEXT_ENTRIES {
        return Err("Cyber context exceeds the local entry-count or depth limit".into());
    }
    *entry_count += 1;
    let metadata = fs::symlink_metadata(source)
        .map_err(|error| format!("cannot inspect Cyber context {}: {error}", source.display()))?;
    if metadata.file_type().is_symlink() {
        return Err(format!(
            "Cyber context symlink is not supported: {}",
            source.display()
        ));
    }
    if metadata.is_file() {
        let bytes = metadata.len();
        if bytes > MAX_CONTEXT_FILE_BYTES
            || total_bytes.saturating_add(bytes) > MAX_CONTEXT_TOTAL_BYTES
        {
            return Err(format!(
                "Cyber context exceeds the local staging limit at {}",
                source.display()
            ));
        }
        create_private_dir(destination)?;
        let filename = source
            .file_name()
            .ok_or_else(|| format!("Cyber context file has no filename: {}", source.display()))?;
        fs::copy(source, destination.join(filename))
            .map_err(|error| format!("cannot stage Cyber context {}: {error}", source.display()))?;
        *total_bytes += bytes;
        return Ok(());
    }
    if metadata.is_dir() {
        create_private_dir(destination)?;
        for entry in fs::read_dir(source)
            .map_err(|error| format!("cannot read Cyber context {}: {error}", source.display()))?
        {
            let entry =
                entry.map_err(|error| format!("cannot read Cyber context entry: {error}"))?;
            let destination = if entry
                .file_type()
                .map_err(|error| format!("cannot inspect Cyber context entry: {error}"))?
                .is_dir()
            {
                destination.join(entry.file_name())
            } else {
                destination.to_path_buf()
            };
            copy_context_entry(
                &entry.path(),
                &destination,
                entry_count,
                total_bytes,
                depth + 1,
            )?;
        }
    }
    Ok(())
}

fn local_error(error: impl std::fmt::Display) -> CtError {
    CtError::Usage(error.to_string())
}

fn render_health(result: &DiscoveryOutput<'_>) -> Result<(), String> {
    let mut lines = vec![
        format!("run        {}", result.run_id),
        format!(
            "discovery  {} ({} candidates)",
            output::terminal_text(&result.health.overall_state),
            result.health.total_candidates
        ),
        format!("artifacts  {}", output::terminal_text(&result.artifacts)),
    ];
    for collector in &result.health.collector_results {
        lines.push(format!(
            "collector  {}  {}  {}",
            output::terminal_text(&collector.collector),
            output::terminal_text(&collector.state),
            output::terminal_text(&collector.reason)
        ));
    }
    for blind_spot in &result.health.blind_spots {
        lines.push(format!("blind spot  {}", output::terminal_text(blind_spot)));
    }
    for limit in &result.context_limits {
        lines.push(format!("context    {}", output::terminal_text(limit)));
    }
    output::print_lines(&lines)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    async fn assert_processes_terminated(pids: &[u32], description: &str) {
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        loop {
            let alive = pids.iter().find_map(|pid| {
                let status = std::process::Command::new("ps")
                    .args(["-o", "stat=", "-p", &pid.to_string()])
                    .output()
                    .unwrap();
                let process_state = String::from_utf8_lossy(&status.stdout);
                (!process_state.trim().is_empty() && !process_state.trim_start().starts_with('Z'))
                    .then_some((*pid, process_state.trim().to_owned()))
            });
            let Some((pid, process_state)) = alive else {
                return;
            };
            if std::time::Instant::now() >= deadline {
                let diagnostic = std::process::Command::new("ps")
                    .args(["-o", "stat=,wchan=", "-p", &pid.to_string()])
                    .output()
                    .unwrap();
                let proc_status = fs::read_to_string(format!("/proc/{pid}/status"))
                    .unwrap_or_else(|error| format!("unavailable: {error}"));
                let pending_signals = proc_status
                    .lines()
                    .filter(|line| line.starts_with("SigPnd:") || line.starts_with("ShdPnd:"))
                    .collect::<Vec<_>>()
                    .join("; ");
                panic!(
                    "{description} survived cancellation; pid={pid}; initial_state={process_state}; ps={}; pending_signals={pending_signals}",
                    String::from_utf8_lossy(&diagnostic.stdout).trim()
                );
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    #[cfg(unix)]
    struct DescendantCleanup(Vec<u32>);

    #[cfg(unix)]
    impl Drop for DescendantCleanup {
        fn drop(&mut self) {
            for pid in &self.0 {
                if let Ok(pid) = i32::try_from(*pid)
                    && let Some(pid) = rustix::process::Pid::from_raw(pid)
                {
                    let _ = rustix::process::kill_process(pid, rustix::process::Signal::KILL);
                }
            }
        }
    }

    #[test]
    fn child_path_preserves_pinned_and_inherited_directories() {
        let inherited = std::env::var_os("PATH").unwrap_or_default();
        let mut expected: Vec<PathBuf> = vec!["/pinned/katana".into(), "/pinned/nuclei".into()];
        expected.extend(std::env::split_paths(&inherited));
        let actual = std::env::split_paths(
            &child_path(vec!["/pinned/katana".into(), "/pinned/nuclei".into()]).unwrap(),
        )
        .collect::<Vec<_>>();
        assert_eq!(actual, expected);
    }

    #[test]
    fn child_environment_does_not_inherit_cloud_or_appsec_credentials() {
        let environment = child_environment("/pinned".into()).unwrap();
        for name in [
            "CLOUDTHINKER_TOKEN",
            "CYBER_IDENTITIES_JSON",
            "CYBER_SURFACE_ROOT",
        ] {
            assert!(!environment.contains_key(OsStr::new(name)));
        }
    }

    #[test]
    fn context_file_keeps_its_extension_and_missing_or_symlink_inputs_fail() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("openapi.yaml");
        fs::write(&source, "openapi: 3.1.0").unwrap();
        let destination = root.path().join("context-0");
        let mut count = 0;
        let mut bytes = 0;
        copy_context_source(&source, &destination, &mut count, &mut bytes).unwrap();
        assert_eq!(
            fs::read(destination.join("openapi.yaml")).unwrap(),
            b"openapi: 3.1.0"
        );
        assert!(
            copy_context_source(
                &root.path().join("missing.yaml"),
                &destination,
                &mut count,
                &mut bytes
            )
            .is_err()
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            let link = root.path().join("linked.yaml");
            symlink(&source, &link).unwrap();
            assert!(copy_context_source(&link, &destination, &mut count, &mut bytes).is_err());
        }
    }

    #[test]
    fn run_lock_excludes_a_second_process_and_releases_on_drop() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("run-state");
        let first = RunRoots::open_at(root.clone()).unwrap();
        assert!(RunRoots::open_at(root.clone()).is_err());
        drop(first);
        assert!(RunRoots::open_at(root).is_ok());
    }

    #[test]
    fn run_workspace_reopens_without_losing_partial_artifacts() {
        let directory = tempfile::tempdir().unwrap();
        let run_id = Uuid::from_u128(1);
        let app_id = Uuid::from_u128(2);
        let conversation_id = Uuid::from_u128(3);
        let run_root = directory.path().join(run_id.to_string());
        let first = prepare_local_workspace_at(&run_root, run_id, app_id, conversation_id).unwrap();
        let candidate = Path::new(&first.evidence_root)
            .join("findings/candidates/open")
            .join("candidate.md");
        let report = Path::new(&first.evidence_root)
            .join("output")
            .join("report.pdf");
        fs::create_dir_all(candidate.parent().unwrap()).unwrap();
        fs::create_dir_all(report.parent().unwrap()).unwrap();
        fs::write(&candidate, "draft finding").unwrap();
        fs::write(&report, b"partial report").unwrap();
        fs::write(
            Path::new(&first.workflow_root).join("journal.jsonl"),
            "stage=scan\n",
        )
        .unwrap();

        let resumed =
            prepare_local_workspace_at(&run_root, run_id, app_id, conversation_id).unwrap();

        assert_eq!(first.workspace_root, resumed.workspace_root);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for path in [
                run_root.as_path(),
                Path::new(&resumed.workspace_root),
                Path::new(&resumed.evidence_root),
                Path::new(&resumed.workflow_root),
            ] {
                assert_eq!(
                    fs::metadata(path).unwrap().permissions().mode() & 0o777,
                    0o700
                );
            }
        }
        assert_eq!(fs::read(candidate).unwrap(), b"draft finding");
        assert_eq!(fs::read(report).unwrap(), b"partial report");
        assert_eq!(
            fs::read(Path::new(&resumed.workflow_root).join("journal.jsonl")).unwrap(),
            b"stage=scan\n"
        );
        let manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(resumed.manifest).unwrap()).unwrap();
        assert_eq!(manifest["run_id"], run_id.to_string());
        assert_eq!(manifest["app_id"], app_id.to_string());
        assert_eq!(manifest["conversation_id"], conversation_id.to_string());
        assert_eq!(manifest["workspace_root"], resumed.workspace_root);
        assert_eq!(manifest["evidence_root"], resumed.evidence_root);
        assert_eq!(manifest["workflow_root"], resumed.workflow_root);
        assert!(manifest.get("memory_root").is_none());
    }

    #[test]
    fn a_later_run_keeps_its_workspace_separate_from_the_prior_run() {
        let directory = tempfile::tempdir().unwrap();
        let prior = prepare_local_workspace_at(
            &directory.path().join(Uuid::from_u128(1).to_string()),
            Uuid::from_u128(1),
            Uuid::from_u128(3),
            Uuid::from_u128(4),
        )
        .unwrap();
        let later = prepare_local_workspace_at(
            &directory.path().join(Uuid::from_u128(2).to_string()),
            Uuid::from_u128(2),
            Uuid::from_u128(3),
            Uuid::from_u128(5),
        )
        .unwrap();
        fs::write(Path::new(&prior.evidence_root).join("partial.txt"), "keep").unwrap();

        assert_ne!(prior.workspace_root, later.workspace_root);
        assert_eq!(
            fs::read(Path::new(&prior.evidence_root).join("partial.txt")).unwrap(),
            b"keep"
        );
    }

    #[test]
    fn context_directory_preserves_relative_layout() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        fs::create_dir_all(source.join("schemas")).unwrap();
        fs::write(source.join("openapi.yaml"), "openapi: 3.1.0").unwrap();
        fs::write(source.join("schemas/model.yaml"), "type: object").unwrap();
        let destination = root.path().join("staged");
        copy_context_source(&source, &destination, &mut 0, &mut 0).unwrap();
        assert!(destination.join("openapi.yaml").is_file());
        assert!(destination.join("schemas/model.yaml").is_file());
    }

    #[test]
    fn context_staging_bounds_empty_directories_and_depth() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        fs::create_dir(&source).unwrap();
        let destination = root.path().join("staged");
        let mut entries = 20_000;
        let mut bytes = 0;
        assert!(copy_context_source(&source, &destination, &mut entries, &mut bytes).is_err());
        let mut deepest = source.clone();
        for _ in 0..65 {
            deepest = deepest.join("nested");
        }
        fs::create_dir_all(deepest).unwrap();
        entries = 0;
        assert!(copy_context_source(&source, &destination, &mut entries, &mut bytes).is_err());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn runtime_child_stays_in_outer_group() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("runtime.py");
        let inherited_path = std::env::var_os("PATH").unwrap_or_default();
        let environment = child_environment(inherited_path).unwrap();
        fs::write(&path, "import os; print(os.getpid(), os.getpgrp())\n").unwrap();
        let output = invoke_runtime(&path, &[], &environment, Duration::from_secs(5))
            .await
            .unwrap();
        assert!(output.status.success());
        let stdout = String::from_utf8_lossy(&output.stdout);
        let mut identity = stdout.split_whitespace();
        let _process_id = identity.next().unwrap();
        let process_group = identity.next().unwrap();
        let expected_group = std::process::Command::new("ps")
            .args(["-o", "pgid=", "-p", &std::process::id().to_string()])
            .output()
            .unwrap();
        assert_eq!(
            process_group,
            String::from_utf8_lossy(&expected_group.stdout).trim()
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn runtime_timeout_returns_timeout_without_a_child_readiness_dependency() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("runtime.py");
        let inherited_path = std::env::var_os("PATH").unwrap_or_default();
        let environment = child_environment(inherited_path).unwrap();
        fs::write(&path, "import time; time.sleep(30)\n").unwrap();

        let error = invoke_runtime(&path, &[], &environment, Duration::from_millis(100))
            .await
            .err()
            .expect("runtime deadline should be reported");

        assert!(matches!(error, CtError::Timeout(_)));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cancellation_catches_descendant_created_after_snapshot() {
        let directory = tempfile::tempdir().unwrap();
        let root_path = directory.path().join("runtime.py");
        let child_path = directory.path().join("child.py");
        let child_pid_path = directory.path().join("child.pid");
        let grandchild_pid_path = directory.path().join("grandchild.pid");
        let release_path = directory.path().join("release-child");
        let inherited_path = std::env::var_os("PATH").unwrap_or_default();
        let environment = child_environment(inherited_path).unwrap();
        fs::write(
            &child_path,
            "import os, subprocess, sys, time\nwhile not os.path.exists(sys.argv[2]): time.sleep(0.001)\ngrandchild = subprocess.Popen(['sleep', '30'])\ntemporary = sys.argv[1] + '.tmp'\nwith open(temporary, 'w') as output: output.write(str(grandchild.pid))\nos.replace(temporary, sys.argv[1])\ntime.sleep(30)\n",
        )
        .unwrap();
        fs::write(
            &root_path,
            format!(
                "import os, subprocess, sys, time\nchild = subprocess.Popen([sys.executable, {:?}, {:?}, {:?}])\ntemporary = {:?} + '.tmp'\nwith open(temporary, 'w') as output: output.write(str(child.pid))\nos.replace(temporary, {:?})\ntime.sleep(30)\n",
                child_path.display().to_string(),
                grandchild_pid_path.display().to_string(),
                release_path.display().to_string(),
                child_pid_path.display().to_string(),
                child_pid_path.display().to_string()
            ),
        )
        .unwrap();

        let mut child = tokio::process::Command::new("python3")
            .arg("-I")
            .arg(&root_path)
            .env_clear()
            .envs(&environment)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let root_pid = child.id().unwrap();
        let mut cleanup = DescendantCleanup(Vec::new());
        let child_pid = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Ok(pid) = fs::read_to_string(&child_pid_path) {
                    break pid.trim().parse::<u32>().unwrap();
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("runtime child should publish its pid");
        cleanup.0.push(child_pid);
        let stale_snapshot = process_table_snapshot().expect("process table is available");
        fs::write(&release_path, "go").unwrap();
        let grandchild_pid = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Ok(pid) = fs::read_to_string(&grandchild_pid_path) {
                    break pid.trim().parse::<u32>().unwrap();
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("runtime grandchild should publish its pid");
        cleanup.0.push(grandchild_pid);

        assert!(
            stale_snapshot
                .iter()
                .all(|(pid, _, _)| *pid != grandchild_pid)
        );
        let mut first_snapshot = Some(stale_snapshot);
        let mut snapshot = || first_snapshot.take().or_else(process_table_snapshot);
        stop_runtime_process(&mut child, Some(root_pid), &mut snapshot).await;

        assert_processes_terminated(
            &[child_pid, grandchild_pid],
            "descendants created after snapshot",
        )
        .await;
    }

    #[tokio::test]
    async fn streamed_stderr_redacts_secrets_split_across_reads() {
        use tokio::io::AsyncWriteExt;
        let auth = cyber_discovery_auth_value("Authorization: Bearer ultra-secret-token").unwrap();
        let (mut writer, reader) = tokio::io::duplex(128);
        let task = tokio::spawn(stream_stderr(reader, 1024, vec![auth]));
        writer
            .write_all(b"collector saw Bearer ultra-")
            .await
            .unwrap();
        writer.write_all(b"secret-token\n").await.unwrap();
        drop(writer);
        let tail = task.await.unwrap().unwrap();
        let tail = String::from_utf8(tail).unwrap();
        assert!(!tail.contains("ultra-secret-token"));
        assert!(tail.contains("[redacted]"));
    }

    #[tokio::test]
    async fn overlong_stderr_line_is_replaced_without_exposing_prefix() {
        use tokio::io::AsyncWriteExt;
        let auth = cyber_discovery_auth_value("Authorization: Bearer super-secret-token").unwrap();
        let (mut writer, reader) = tokio::io::duplex(128);
        let task = tokio::spawn(stream_stderr(reader, 64, vec![auth]));
        writer.write_all(&[b'x'; 50]).await.unwrap();
        writer
            .write_all(b"Bearer super-secret-token\n")
            .await
            .unwrap();
        drop(writer);
        let tail = task.await.unwrap().unwrap();
        let tail = String::from_utf8(tail).unwrap();
        assert!(!tail.contains("Bearer sup"));
        assert!(!tail.contains("super-secret-token"));
        assert!(tail.contains("[discovery progress line exceeded limit]"));
    }
}
