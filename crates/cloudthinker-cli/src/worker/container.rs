use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use cloudthinker_client::auth::worker_store::{private_directory, write_private};
use cloudthinker_client::worker_types as api;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio_util::sync::CancellationToken;
use tokio_util::task::AbortOnDropHandle;
use uuid::Uuid;

use super::shell::{TailBuffer, tail};
use super::telemetry::{self, Fields, Level};

pub const UNAVAILABLE_CODE: &str = "EXECUTOR_ISOLATION_UNAVAILABLE";
pub const UNAVAILABLE_MESSAGE: &str = "This conversation must run its commands in a container, and this Worker cannot start one. Install Docker, check with `cloudthinker worker doctor`, then start the worker again.";
pub const RUNTIME: &str = "docker";
pub const PROXY_IMAGE_ENV: &str = "CLOUDTHINKER_WORKER_PROXY_IMAGE";
pub const SANDBOX_IMAGE_ENV: &str = "CLOUDTHINKER_WORKER_SANDBOX_IMAGE";
pub const IDLE_LIMIT: Duration = Duration::from_secs(15 * 60);
const PROXY_PORT: u16 = 3128;
const PROXY_HOST: &str = "cloudthinker-egress";
const PROXY_CONFIG_DIR: &str = "/etc/cloudthinker";
const ALLOWLIST_FILE: &str = "egress-allowlist";
const LABEL_KEY: &str = "cloudthinker.worker";
const STATE_DIR: &str = "containers";
const SESSION_HOME: &str = "/home/worker";
const CANCEL_GRACE: Duration = Duration::from_secs(5);
const ADMIN_TIMEOUT: Duration = Duration::from_secs(300);
const DETECT_TIMEOUT: Duration = Duration::from_secs(10);
const SIGNAL_TIMEOUT: Duration = Duration::from_secs(10);
const PROXY_SETTLE: Duration = Duration::from_millis(300);
const SHELL_WRAPPER: &str =
    r#"printf '%s' "$$" > "$1" && script=$(cat) && exec bash -c "$script" </dev/null"#;
const KILL_WRAPPER: &str = r#"pgid=$(cat "$2" 2>/dev/null) && kill -s "$1" -- "-$pgid""#;
const PROCESS_COUNT: &str = "n=0; for p in /proc/[0-9]*; do n=$((n+1)); done; echo \"$n\"";
const HOST_ONLY_ENV: [&str; 5] = ["PATH", "HOME", "USER", "TMPDIR", "SHELL"];
const CLIENT_ENV: [&str; 11] = [
    "PATH",
    "HOME",
    "DOCKER_HOST",
    "DOCKER_CONFIG",
    "DOCKER_CONTEXT",
    "DOCKER_CERT_PATH",
    "DOCKER_TLS_VERIFY",
    "CONTAINER_HOST",
    "CONTAINERS_CONF",
    "XDG_RUNTIME_DIR",
    "XDG_CONFIG_HOME",
];

#[derive(Debug, Clone)]
pub struct Runtime {
    pub program: PathBuf,
    pub version: String,
}

pub enum Detection {
    Ready(Runtime),
    Unavailable(String),
}

impl Runtime {
    pub fn detect() -> Detection {
        let Some(program) = find_on_path(RUNTIME) else {
            return Detection::Unavailable(format!("{RUNTIME} is not installed"));
        };
        match probe(&program, &["version", "--format", "{{.Server.Version}}"]) {
            Some(server) => Detection::Ready(Runtime {
                version: format!("{RUNTIME} {}", server.trim()),
                program,
            }),
            None => Detection::Unavailable(format!("{RUNTIME} cannot reach its daemon")),
        }
    }

    pub async fn detect_async() -> Detection {
        tokio::task::spawn_blocking(Self::detect)
            .await
            .unwrap_or_else(|_| Detection::Unavailable("runtime detection was interrupted".into()))
    }
}

fn probe(program: &Path, argv: &[&str]) -> Option<String> {
    let mut child = std::process::Command::new(program)
        .args(argv)
        .env_clear()
        .envs(client_env())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + DETECT_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => break,
            Ok(Some(_)) | Err(_) => return None,
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
        }
    }
    let mut out = String::new();
    std::io::Read::read_to_string(&mut child.stdout.take()?, &mut out).ok()?;
    Some(out).filter(|text| !text.trim().is_empty())
}

fn find_on_path(name: &str) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join(name))
        .find(|candidate| {
            std::fs::metadata(candidate)
                .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
        })
}

pub fn client_env() -> BTreeMap<String, String> {
    std::env::vars()
        .filter(|(key, _)| CLIENT_ENV.contains(&key.as_str()))
        .collect()
}

pub fn forwarded_env(environment: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    environment
        .iter()
        .filter(|(key, _)| {
            !HOST_ONLY_ENV.contains(&key.as_str()) && !CLIENT_ENV.contains(&key.as_str())
        })
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

pub fn default_proxy_image() -> String {
    image_from_env(
        PROXY_IMAGE_ENV,
        "ghcr.io/cloudthinker-ai/cloudthinker-worker",
    )
}

pub fn default_sandbox_image() -> String {
    image_from_env(
        SANDBOX_IMAGE_ENV,
        "ghcr.io/cloudthinker-ai/cloudthinker-worker-sandbox",
    )
}

fn image_from_env(key: &str, repository: &str) -> String {
    std::env::var(key)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| format!("{repository}:{}", env!("CARGO_PKG_VERSION")))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Names {
    pub network: String,
    pub proxy: String,
    pub session: String,
}

impl Names {
    pub fn of(owner: &Path) -> Self {
        let digest = Sha256::digest(owner.as_os_str().as_encoded_bytes());
        let short: String = digest.iter().take(6).map(|b| format!("{b:02x}")).collect();
        Self {
            network: format!("ct-{short}-net"),
            proxy: format!("ct-{short}-egress"),
            session: format!("ct-{short}-session"),
        }
    }

    fn key(&self) -> &str {
        &self.session
    }
}

pub fn label(installation: Uuid, workdir: Uuid) -> String {
    format!("{LABEL_KEY}={installation}.{workdir}")
}

pub fn network_create_argv(label: &str, network: &str) -> Vec<String> {
    strings(&[
        "network",
        "create",
        "--internal",
        "--opt",
        "com.docker.network.bridge.inhibit_ipv4=true",
        "--label",
        label,
        network,
    ])
}

pub fn network_connect_argv(network: &str, container: &str) -> Vec<String> {
    strings(&["network", "connect", network, container])
}

pub fn proxy_run_argv(
    names: &Names,
    label: &str,
    user: &str,
    allowlist_dir: &Path,
    image: &str,
) -> Vec<String> {
    let mount = format!("{}:{PROXY_CONFIG_DIR}:ro", allowlist_dir.display());
    let listen = format!("0.0.0.0:{PROXY_PORT}");
    let allowlist = format!("{PROXY_CONFIG_DIR}/{ALLOWLIST_FILE}");
    strings(&[
        "run",
        "-d",
        "--name",
        &names.proxy,
        "--label",
        label,
        "--user",
        user,
        "--cap-drop",
        "ALL",
        "--security-opt",
        "no-new-privileges",
        "--read-only",
        "-v",
        &mount,
        "--entrypoint",
        "cloudthinker",
        image,
        "worker",
        "egress-proxy",
        "--listen",
        &listen,
        "--allowlist-file",
        &allowlist,
    ])
}

pub fn proxy_address_argv(names: &Names) -> Vec<String> {
    let format = format!(
        "{{{{(index .NetworkSettings.Networks \"{}\").IPAddress}}}}",
        names.network
    );
    strings(&["inspect", "-f", &format, &names.proxy])
}

pub fn running_argv(container: &str) -> Vec<String> {
    strings(&["inspect", "-f", "{{.State.Running}}", container])
}

pub struct SessionSpec<'a> {
    pub names: &'a Names,
    pub label: &'a str,
    pub user: &'a str,
    pub proxy_address: &'a str,
    pub folder: &'a Path,
    pub extra_rw: &'a [&'a Path],
    pub read_only: &'a [&'a Path],
    pub image: &'a str,
}

pub fn session_run_argv(spec: &SessionSpec<'_>) -> Vec<String> {
    let proxy = format!("http://{PROXY_HOST}:{PROXY_PORT}");
    let folder = spec.folder.display().to_string();
    let mut argv = strings(&[
        "run",
        "-d",
        "--name",
        &spec.names.session,
        "--label",
        spec.label,
        "--network",
        &spec.names.network,
        "--dns",
        "127.0.0.1",
        "--add-host",
        &format!("{PROXY_HOST}:{}", spec.proxy_address),
        "--user",
        spec.user,
        "--cap-drop",
        "ALL",
        "--security-opt",
        "no-new-privileges",
        "--read-only",
        "--tmpfs",
        "/tmp:rw,nosuid,size=1g",
        "--tmpfs",
        &format!("{SESSION_HOME}:rw,nosuid"),
        "--pids-limit",
        "512",
        "--memory",
        "4g",
        "--cpus",
        "2",
        "-e",
        &format!("HOME={SESSION_HOME}"),
    ]);
    for key in ["HTTP_PROXY", "HTTPS_PROXY", "http_proxy", "https_proxy"] {
        argv.push("-e".into());
        argv.push(format!("{key}={proxy}"));
    }
    argv.push("-v".into());
    argv.push(format!("{folder}:{folder}:rw"));
    for path in spec.extra_rw {
        let path = path.display();
        argv.push("-v".into());
        argv.push(format!("{path}:{path}:rw"));
    }
    for path in spec.read_only {
        let path = path.display();
        argv.push("-v".into());
        argv.push(format!("{path}:{path}:ro"));
    }
    argv.extend(strings(&["-w", &folder, spec.image, "sleep", "infinity"]));
    argv
}

pub fn shell_argv(session: &str, cwd: &Path, env_keys: &[String], pgid_file: &str) -> Vec<String> {
    let mut argv = strings(&["exec", "-i", "-w", &cwd.display().to_string()]);
    for key in env_keys {
        argv.push("-e".into());
        argv.push(key.clone());
    }
    argv.extend(strings(&[
        session,
        "setsid",
        "-w",
        "bash",
        "-c",
        SHELL_WRAPPER,
        "cloudthinker",
        pgid_file,
    ]));
    argv
}

pub fn kill_argv(session: &str, pgid_file: &str, signal: &str) -> Vec<String> {
    strings(&[
        "exec",
        session,
        "bash",
        "-c",
        KILL_WRAPPER,
        "cloudthinker",
        signal,
        pgid_file,
    ])
}

pub fn busy_argv(session: &str) -> Vec<String> {
    strings(&["exec", session, "bash", "-c", PROCESS_COUNT])
}

pub fn remove_argv(containers: &[&str]) -> Vec<String> {
    let mut argv = strings(&["rm", "-f"]);
    argv.extend(containers.iter().map(|name| (*name).to_owned()));
    argv
}

pub fn network_remove_argv(network: &str) -> Vec<String> {
    strings(&["network", "rm", network])
}

pub fn labelled_containers_argv(label: &str) -> Vec<String> {
    strings(&["ps", "-aq", "--filter", &format!("label={label}")])
}

pub fn labelled_networks_argv(label: &str) -> Vec<String> {
    strings(&["network", "ls", "-q", "--filter", &format!("label={label}")])
}

pub fn image_present_argv(image: &str) -> Vec<String> {
    strings(&["image", "inspect", image])
}

pub fn pgid_file(id: &str) -> String {
    format!("/tmp/.cloudthinker-{id}.pgid")
}

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

pub fn render_allowlist(entries: &[String]) -> String {
    let mut text = String::new();
    for entry in entries {
        let entry = entry.trim();
        if entry.is_empty() || entry.contains(char::is_whitespace) {
            continue;
        }
        text.push_str(entry);
        text.push('\n');
    }
    text
}

pub fn write_allowlist(dir: &Path, entries: &[String]) -> Result<(), &'static str> {
    let dir = private_directory(dir).map_err(|_| UNAVAILABLE_CODE)?;
    write_private(&dir, ALLOWLIST_FILE, render_allowlist(entries).as_bytes())
        .map_err(|_| UNAVAILABLE_CODE)
}

#[derive(Clone)]
pub struct Target {
    pub program: PathBuf,
    pub session: String,
}

struct Session {
    names: Names,
    image: String,
    allowlist_dir: PathBuf,
    allowlist: Vec<String>,
    last_used: Instant,
}

pub struct Containers {
    runtime: Option<Runtime>,
    label: String,
    state: PathBuf,
    user: String,
    sessions: tokio::sync::Mutex<HashMap<String, Session>>,
    assignments: Mutex<HashMap<Uuid, String>>,
}

impl Containers {
    pub fn new(runtime: Option<Runtime>, installation: Uuid, workdir: Uuid, state: &Path) -> Self {
        let user = format!(
            "{}:{}",
            rustix::process::getuid().as_raw(),
            rustix::process::getgid().as_raw()
        );
        Self {
            runtime,
            label: label(installation, workdir),
            state: state.join(STATE_DIR),
            user,
            sessions: tokio::sync::Mutex::new(HashMap::new()),
            assignments: Mutex::new(HashMap::new()),
        }
    }

    pub fn disabled() -> Self {
        Self::new(None, Uuid::nil(), Uuid::nil(), Path::new("/nonexistent"))
    }

    pub fn is_ready(&self) -> bool {
        self.runtime.is_some()
    }

    pub async fn ensure(
        &self,
        assignment: Uuid,
        policy: &api::WorkerIsolationPolicy,
        folder: &Path,
        served: &Path,
        bundles: &Path,
    ) -> Result<Target, &'static str> {
        let runtime = self.runtime.as_ref().ok_or(UNAVAILABLE_CODE)?;
        let names = Names::of(folder);
        let image = policy
            .image
            .clone()
            .filter(|image| !image.trim().is_empty())
            .unwrap_or_else(default_sandbox_image);
        self.assignments
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(assignment, names.key().to_owned());
        let allowlist_dir = self.state.join(&names.session);
        let mut sessions = self.sessions.lock().await;
        let reusable = match sessions.get_mut(names.key()) {
            Some(session) => {
                if session.allowlist != policy.egress_allowlist {
                    rewrite_allowlist(&session.allowlist_dir, &policy.egress_allowlist).await?;
                    session.allowlist = policy.egress_allowlist.clone();
                }
                session.image == image
            }
            None => false,
        };
        if reusable && self.running(runtime, &names.session).await {
            if let Some(session) = sessions.get_mut(names.key()) {
                session.last_used = Instant::now();
            }
            return Ok(self.target(runtime, &names));
        }
        if let Some(stale) = sessions.remove(names.key()) {
            self.teardown(runtime, &stale.names).await;
        }
        rewrite_allowlist(&allowlist_dir, &policy.egress_allowlist).await?;
        let mut extra_rw = Vec::new();
        if served != folder {
            extra_rw.push(served);
        }
        let read_only = [bundles];
        let session = (&names, allowlist_dir.as_path(), image.as_str());
        let mounts = (extra_rw.as_slice(), read_only.as_slice());
        let mut started = self.start(runtime, session, folder, mounts).await;
        if started.is_err() {
            started = self.start(runtime, session, folder, mounts).await;
        }
        if let Err(reason) = started {
            telemetry::emit(
                Level::Warn,
                "container_start_failed",
                Fields {
                    assignment_id: Some(assignment),
                    error_code: Some(UNAVAILABLE_CODE),
                    ..Fields::default()
                },
                &format!("Could not start the session container: {reason}."),
            );
            self.teardown(runtime, &names).await;
            return Err(UNAVAILABLE_CODE);
        }
        sessions.insert(
            names.key().to_owned(),
            Session {
                names: names.clone(),
                image,
                allowlist_dir,
                allowlist: policy.egress_allowlist.clone(),
                last_used: Instant::now(),
            },
        );
        Ok(self.target(runtime, &names))
    }

    pub async fn refresh_allowlist(&self, assignment: Uuid, policy: &api::WorkerIsolationPolicy) {
        let key = self
            .assignments
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&assignment)
            .cloned();
        let Some(key) = key else {
            return;
        };
        let mut sessions = self.sessions.lock().await;
        if let Some(session) = sessions.get_mut(&key)
            && session.allowlist != policy.egress_allowlist
            && rewrite_allowlist(&session.allowlist_dir, &policy.egress_allowlist)
                .await
                .is_ok()
        {
            session.allowlist = policy.egress_allowlist.clone();
        }
    }

    pub fn forget_assignment(&self, assignment: Uuid) {
        self.assignments
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&assignment);
    }

    pub async fn reap_idle(&self) {
        let Some(runtime) = self.runtime.as_ref() else {
            return;
        };
        let mut sessions = self.sessions.lock().await;
        let idle: Vec<String> = sessions
            .iter()
            .filter(|(_, session)| session.last_used.elapsed() >= IDLE_LIMIT)
            .map(|(key, _)| key.clone())
            .collect();
        for key in idle {
            if busy(runtime, &key).await {
                if let Some(session) = sessions.get_mut(&key) {
                    session.last_used = Instant::now();
                }
                continue;
            }
            if let Some(session) = sessions.remove(&key) {
                self.teardown(runtime, &session.names).await;
            }
        }
    }

    pub async fn shutdown(&self) {
        let Some(runtime) = self.runtime.as_ref() else {
            return;
        };
        let mut sessions = self.sessions.lock().await;
        for (_, session) in sessions.drain() {
            self.teardown(runtime, &session.names).await;
        }
        drop(sessions);
        self.remove_labelled().await;
    }

    pub async fn remove_labelled(&self) {
        let Some(runtime) = self.runtime.as_ref() else {
            return;
        };
        if let Ok(ids) = admin(runtime, &labelled_containers_argv(&self.label)).await {
            let ids: Vec<&str> = ids.split_whitespace().collect();
            if !ids.is_empty() {
                let _ = admin(runtime, &remove_argv(&ids)).await;
            }
        }
        if let Ok(ids) = admin(runtime, &labelled_networks_argv(&self.label)).await {
            for id in ids.split_whitespace() {
                let _ = admin(runtime, &network_remove_argv(id)).await;
            }
        }
    }

    fn target(&self, runtime: &Runtime, names: &Names) -> Target {
        Target {
            program: runtime.program.clone(),
            session: names.session.clone(),
        }
    }

    async fn running(&self, runtime: &Runtime, container: &str) -> bool {
        admin(runtime, &running_argv(container))
            .await
            .is_ok_and(|state| state.trim() == "true")
    }

    async fn start(
        &self,
        runtime: &Runtime,
        session: (&Names, &Path, &str),
        folder: &Path,
        mounts: (&[&Path], &[&Path]),
    ) -> Result<(), String> {
        let (names, allowlist_dir, image) = session;
        let (extra_rw, read_only) = mounts;
        self.teardown(runtime, names).await;
        admin(runtime, &network_create_argv(&self.label, &names.network)).await?;
        let proxy_image = default_proxy_image();
        admin(
            runtime,
            &proxy_run_argv(names, &self.label, &self.user, allowlist_dir, &proxy_image),
        )
        .await?;
        admin(runtime, &network_connect_argv(&names.network, &names.proxy)).await?;
        tokio::time::sleep(PROXY_SETTLE).await;
        if !self.running(runtime, &names.proxy).await {
            return Err("the egress proxy container stopped right after it started".into());
        }
        let address = admin(runtime, &proxy_address_argv(names)).await?;
        let address = address.trim();
        if address.parse::<std::net::IpAddr>().is_err() {
            return Err("the egress proxy has no address on the session network".into());
        }
        admin(
            runtime,
            &session_run_argv(&SessionSpec {
                names,
                label: &self.label,
                user: &self.user,
                proxy_address: address,
                folder,
                extra_rw,
                read_only,
                image,
            }),
        )
        .await?;
        if self.running(runtime, &names.session).await {
            Ok(())
        } else {
            Err("the session container stopped right after it started".into())
        }
    }

    async fn teardown(&self, runtime: &Runtime, names: &Names) {
        let _ = admin(
            runtime,
            &remove_argv(&[names.session.as_str(), names.proxy.as_str()]),
        )
        .await;
        let _ = admin(runtime, &network_remove_argv(&names.network)).await;
    }
}

async fn rewrite_allowlist(dir: &Path, entries: &[String]) -> Result<(), &'static str> {
    let dir = dir.to_path_buf();
    let entries = entries.to_vec();
    tokio::task::spawn_blocking(move || write_allowlist(&dir, &entries))
        .await
        .map_err(|_| UNAVAILABLE_CODE)?
}

async fn busy(runtime: &Runtime, session: &str) -> bool {
    admin(runtime, &busy_argv(session))
        .await
        .ok()
        .and_then(|count| count.trim().parse::<u32>().ok())
        .is_some_and(|count| count > 2)
}

async fn admin(runtime: &Runtime, argv: &[String]) -> Result<String, String> {
    let mut command = Command::new(&runtime.program);
    command
        .args(argv)
        .env_clear()
        .envs(client_env())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let step = format!(
        "{RUNTIME} {}",
        argv.iter().take(2).cloned().collect::<Vec<_>>().join(" ")
    );
    let output = match tokio::time::timeout(ADMIN_TIMEOUT, command.output()).await {
        Ok(Ok(output)) => output,
        Ok(Err(error)) => return Err(format!("{step} failed: {error}")),
        Err(_) => return Err(format!("{step} timed out")),
    };
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let lines: Vec<&str> = stderr
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .collect();
        let detail = lines
            .iter()
            .rev()
            .find(|line| line.to_ascii_lowercase().contains("error"))
            .or(lines.last())
            .map_or("no output", |line| line.trim_start_matches("docker: "));
        Err(format!("{step} failed: {detail}"))
    }
}

pub async fn signal_group(program: &Path, session: &str, pgid_file: &str, signal: &str) -> bool {
    let mut command = Command::new(program);
    command
        .args(kill_argv(session, pgid_file, signal))
        .env_clear()
        .envs(client_env())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    matches!(
        tokio::time::timeout(SIGNAL_TIMEOUT, command.status()).await,
        Ok(Ok(status)) if status.success()
    )
}

pub async fn run_script(
    target: &Target,
    cwd: &Path,
    script: &str,
    environment: &BTreeMap<String, String>,
    deadline: DateTime<Utc>,
    timeout: Option<f64>,
    cancel: CancellationToken,
) -> Result<Value, &'static str> {
    let started = Instant::now();
    let duration = (deadline - Utc::now()).to_std().map_err(|_| "TIMEOUT")?;
    let duration = timeout
        .filter(|t| t.is_finite() && *t > 0.0)
        .map(|t| duration.min(Duration::from_secs_f64(t.min(3600.0))))
        .unwrap_or(duration);
    let forwarded = forwarded_env(environment);
    let keys: Vec<String> = forwarded.keys().cloned().collect();
    let pgid = pgid_file(&Uuid::new_v4().to_string());
    let mut child = Command::new(&target.program)
        .args(shell_argv(&target.session, cwd, &keys, &pgid))
        .env_clear()
        .envs(client_env())
        .envs(&forwarded)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|_| UNAVAILABLE_CODE)?;
    let mut stdin = child.stdin.take().ok_or("SHELL_UNAVAILABLE")?;
    let stdout = child.stdout.take().ok_or("SHELL_UNAVAILABLE")?;
    let stderr = child.stderr.take().ok_or("SHELL_UNAVAILABLE")?;
    let buffers = [TailBuffer::default(), TailBuffer::default()];
    let mut stdout_task = AbortOnDropHandle::new(tokio::spawn(tail(stdout, buffers[0].clone())));
    let mut stderr_task = AbortOnDropHandle::new(tokio::spawn(tail(stderr, buffers[1].clone())));
    if stdin.write_all(script.as_bytes()).await.is_err() {
        return Err("SHELL_UNAVAILABLE");
    }
    drop(stdin);
    let (status, reason) = tokio::select! {
        status = child.wait() => (status.ok(), None),
        _ = cancel.cancelled() => (None, Some("CANCELLED")),
        _ = tokio::time::sleep(duration) => (None, Some("TIMEOUT")),
    };
    let mut incomplete = false;
    let status = if reason.is_some() {
        signal_group(&target.program, &target.session, &pgid, "TERM").await;
        match tokio::time::timeout(CANCEL_GRACE, child.wait()).await {
            Ok(Ok(status)) => Some(status),
            _ => {
                if !signal_group(&target.program, &target.session, &pgid, "KILL").await {
                    incomplete = true;
                }
                let _ = child.start_kill();
                match tokio::time::timeout(CANCEL_GRACE, child.wait()).await {
                    Ok(Ok(status)) => Some(status),
                    _ => {
                        incomplete = true;
                        None
                    }
                }
            }
        }
    } else {
        status
    };
    let mut text = Vec::new();
    for (handle, buffer) in [&mut stdout_task, &mut stderr_task]
        .into_iter()
        .zip(&buffers)
    {
        if !matches!(
            tokio::time::timeout(CANCEL_GRACE, &mut *handle).await,
            Ok(Ok(Ok(())))
        ) {
            handle.abort();
            incomplete = true;
        }
        text.push(buffer.text());
    }
    Ok(
        json!({"result":{"return_code":status.and_then(|s|s.code()).unwrap_or(-1),"stdout":text[0],"stderr":text[1],"duration":started.elapsed().as_secs_f64(),"error_code":reason,"cancel_incomplete":incomplete}}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names() -> Names {
        Names::of(Path::new("/srv/project"))
    }

    #[test]
    fn the_cleanup_label_belongs_to_one_served_folder_not_the_whole_machine() {
        let machine = Uuid::new_v4();
        let first = label(machine, Uuid::new_v4());
        let second = label(machine, Uuid::new_v4());
        assert_ne!(first, second);
        assert!(first.starts_with(&format!("{LABEL_KEY}={machine}.")));
        assert_ne!(
            labelled_containers_argv(&first),
            labelled_containers_argv(&second)
        );
    }

    #[test]
    fn names_are_stable_short_and_scoped_to_the_folder() {
        let first = names();
        assert_eq!(first, Names::of(Path::new("/srv/project")));
        assert_ne!(first, Names::of(Path::new("/srv/other")));
        assert!(first.network.starts_with("ct-") && first.network.ends_with("-net"));
        assert!(first.proxy.ends_with("-egress"));
        assert!(first.session.ends_with("-session"));
        assert_eq!(first.session.len(), "ct-".len() + 12 + "-session".len());
    }

    #[test]
    fn the_session_container_has_no_dns_no_privileges_and_only_the_proxy_as_a_way_out() {
        let names = names();
        let argv = session_run_argv(&SessionSpec {
            names: &names,
            label: "cloudthinker.worker=1",
            user: "1000:1000",
            proxy_address: "10.89.0.2",
            folder: Path::new("/srv/project"),
            extra_rw: &[],
            read_only: &[Path::new("/state/bundles")],
            image: "sandbox:1",
        });
        let joined = argv.join(" ");
        for expected in [
            format!("--network {}", names.network),
            "--dns 127.0.0.1".into(),
            "--add-host cloudthinker-egress:10.89.0.2".into(),
            "--user 1000:1000".into(),
            "--cap-drop ALL".into(),
            "--security-opt no-new-privileges".into(),
            "--read-only".into(),
            "--tmpfs /tmp:rw,nosuid,size=1g".into(),
            "--tmpfs /home/worker:rw,nosuid".into(),
            "-e HOME=/home/worker".into(),
            "-e HTTPS_PROXY=http://cloudthinker-egress:3128".into(),
            "-e https_proxy=http://cloudthinker-egress:3128".into(),
            "--pids-limit 512 --memory 4g --cpus 2".into(),
            "-v /srv/project:/srv/project:rw".into(),
            "-v /state/bundles:/state/bundles:ro".into(),
            "-w /srv/project sandbox:1 sleep infinity".into(),
        ] {
            assert!(joined.contains(&expected), "missing {expected} in {joined}");
        }
        assert!(!joined.contains("--privileged"));
    }

    #[test]
    fn the_proxy_reads_a_mounted_folder_so_an_atomic_rewrite_reaches_it() {
        let names = names();
        let argv = proxy_run_argv(
            &names,
            "cloudthinker.worker=1",
            "1000:1000",
            Path::new("/state/containers/x"),
            "worker:1",
        );
        let joined = argv.join(" ");
        assert!(joined.contains("-v /state/containers/x:/etc/cloudthinker:ro"));
        assert!(joined.contains("--cap-drop ALL --security-opt no-new-privileges --read-only"));
        assert!(joined.ends_with(
            "--entrypoint cloudthinker worker:1 worker egress-proxy --listen 0.0.0.0:3128 --allowlist-file /etc/cloudthinker/egress-allowlist"
        ));
        assert!(!joined.contains("--network"));
        assert_eq!(
            network_create_argv("l", &names.network),
            [
                "network",
                "create",
                "--internal",
                "--opt",
                "com.docker.network.bridge.inhibit_ipv4=true",
                "--label",
                "l",
                names.network.as_str()
            ]
        );
    }

    #[test]
    fn a_command_names_its_environment_without_values_and_records_its_process_group() {
        let argv = shell_argv(
            "ct-abc-session",
            Path::new("/srv/project/src"),
            &["LANG".into(), "TOKEN".into()],
            "/tmp/.cloudthinker-1.pgid",
        );
        assert_eq!(
            argv,
            [
                "exec",
                "-i",
                "-w",
                "/srv/project/src",
                "-e",
                "LANG",
                "-e",
                "TOKEN",
                "ct-abc-session",
                "setsid",
                "-w",
                "bash",
                "-c",
                SHELL_WRAPPER,
                "cloudthinker",
                "/tmp/.cloudthinker-1.pgid",
            ]
        );
        assert_eq!(
            kill_argv("ct-abc-session", "/tmp/.cloudthinker-1.pgid", "TERM"),
            [
                "exec",
                "ct-abc-session",
                "bash",
                "-c",
                KILL_WRAPPER,
                "cloudthinker",
                "TERM",
                "/tmp/.cloudthinker-1.pgid",
            ]
        );
    }

    #[test]
    fn host_paths_and_runtime_settings_never_enter_the_container() {
        let environment = BTreeMap::from([
            ("PATH".to_owned(), "/host/bin".to_owned()),
            ("HOME".to_owned(), "/home/me".to_owned()),
            (
                "DOCKER_HOST".to_owned(),
                "unix:///run/docker.sock".to_owned(),
            ),
            ("LANG".to_owned(), "C.UTF-8".to_owned()),
        ]);
        let forwarded = forwarded_env(&environment);
        assert_eq!(forwarded.keys().collect::<Vec<_>>(), ["LANG"]);
    }

    #[test]
    fn an_allowlist_rewrite_replaces_the_file_and_an_empty_list_blocks_everything() {
        let temporary = tempfile::tempdir().unwrap();
        let dir = temporary.path().join("allow");
        write_allowlist(&dir, &["api.github.com".into(), "*.pypi.org".into()]).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.join(ALLOWLIST_FILE)).unwrap(),
            "api.github.com\n*.pypi.org\n"
        );
        write_allowlist(&dir, &[" bad entry ".into()]).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.join(ALLOWLIST_FILE)).unwrap(),
            ""
        );
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(leftovers, [std::ffi::OsString::from(ALLOWLIST_FILE)]);
    }
}
