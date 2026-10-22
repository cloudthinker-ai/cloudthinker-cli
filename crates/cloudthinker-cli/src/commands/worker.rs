use std::net::SocketAddr;
use std::path::PathBuf;

use clap::{Args, Subcommand, ValueEnum};
use cloudthinker_client::auth::worker_store::{WorkerCredential, WorkerStore};
use cloudthinker_client::{CtError, CtResult, worker_api::WorkerClient, worker_types as api};
use serde::Serialize;
use uuid::Uuid;

use super::build_client;
use super::worker_service::{self, ServiceCommand};
use crate::engine::{
    exit::{self, ExitCode},
    output,
};

#[path = "worker_credentials.rs"]
mod worker_credentials;

#[cfg(unix)]
use worker_credentials::Rotator;
#[cfg(unix)]
use worker_credentials::resolve_credential;

const CLI_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Args)]
pub struct WorkerArgs {
    #[command(subcommand)]
    command: WorkerCommand,
}

#[derive(Debug, Subcommand)]
enum WorkerCommand {
    /// Create, list, or archive the outposts this workspace can run work on.
    Outpost {
        #[command(subcommand)]
        command: OutpostCommand,
    },
    /// Serve one folder on this machine as an outpost worker until interrupted.
    Start(StartArgs),
    /// Keep a worker running under systemd --user or launchd.
    Service {
        #[command(subcommand)]
        command: ServiceCommand,
    },
    /// Check that this machine can run a worker and print one fix for each problem.
    Doctor(DoctorArgs),
    /// List or clean up the git worktrees this machine made for conversations.
    Worktrees {
        #[command(subcommand)]
        command: WorktreesCommand,
    },
    /// Show one outpost's availability as the workspace sees it.
    Status {
        /// Outpost UUID or exact name.
        #[arg(long, env = "CLOUDTHINKER_OUTPOST_ID")]
        outpost: String,

        /// Print JSON on stdout instead of text.
        #[arg(long)]
        json: bool,
    },
    /// Supervise one background command for a running worker.
    #[command(hide = true)]
    BgShim(BgShimArgs),
}

#[derive(Debug, Subcommand)]
enum WorktreesCommand {
    /// List worktrees with their branch, last use, and whether they hold uncommitted changes.
    Ls {
        /// Only this outpost's worktrees: UUID or exact name.
        #[arg(long)]
        outpost: Option<String>,

        /// Print JSON on stdout instead of text.
        #[arg(long)]
        json: bool,
    },
    /// Remove worktrees unused for 14 days that have no uncommitted changes.
    Prune {
        /// Only this outpost's worktrees: UUID or exact name.
        #[arg(long)]
        outpost: Option<String>,

        /// Print JSON on stdout instead of text.
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Subcommand)]
enum OutpostCommand {
    /// Register a new outpost and store its worker credential on this machine.
    Create {
        /// Display name, unique within the scope.
        name: String,

        /// Let the whole workspace run work on it instead of you alone.
        #[arg(long)]
        shared: bool,

        /// Print JSON on stdout instead of text.
        #[arg(long)]
        json: bool,
    },
    /// List the outposts this workspace can run work on.
    Ls {
        /// Print JSON on stdout instead of text.
        #[arg(long)]
        json: bool,
    },
    /// Retire an outpost so no further work is routed to it.
    Archive {
        /// Outpost UUID or exact name.
        outpost: String,

        /// Print JSON on stdout instead of text.
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Args)]
pub struct DoctorArgs {
    /// Outpost UUID or exact name; defaults to the only outpost set up on this machine.
    #[arg(long, env = "CLOUDTHINKER_OUTPOST_ID")]
    outpost: Option<String>,

    /// The folder you serve or plan to serve.
    #[arg(long, default_value = ".")]
    workdir: PathBuf,

    /// Print JSON on stdout instead of text.
    #[arg(long)]
    json: bool,
}

#[cfg(unix)]
impl DoctorArgs {
    pub(super) fn outpost(&self) -> Option<&str> {
        self.outpost.as_deref()
    }

    pub(super) fn workdir(&self) -> &std::path::Path {
        &self.workdir
    }

    pub(super) fn json(&self) -> bool {
        self.json
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum LogFormat {
    Text,
    Json,
}

#[derive(Debug, Args)]
pub struct StartArgs {
    /// Outpost UUID or exact name; omit it only with `--register`.
    #[arg(long, env = "CLOUDTHINKER_OUTPOST_ID")]
    outpost: Option<String>,

    /// The one folder file tools are confined to; shell commands run here.
    #[arg(long)]
    workdir: PathBuf,

    /// How many assignments this worker serves at once.
    #[arg(long,default_value_t=4,value_parser=clap::value_parser!(u16).range(1..=32))]
    concurrency: u16,

    /// One-time registration reference from the install command.
    #[arg(long)]
    register: Option<String>,

    /// Short display name for the served folder.
    #[arg(long)]
    label: Option<String>,

    /// Pass this variable's value from the worker's own environment to shell commands. Give the name only, not NAME=VALUE; repeat for more.
    #[arg(long = "env", value_name = "NAME")]
    env: Vec<String>,

    /// Pass this shell's whole environment to shell commands.
    #[arg(long)]
    inherit_env: bool,

    /// Serve a replaced folder at --workdir as a new folder for this outpost.
    #[arg(long)]
    reset_folder: bool,

    /// Print worker logs on stderr as plain text or as one JSON object per line.
    #[arg(long, value_enum, default_value_t = LogFormat::Text)]
    log_format: LogFormat,

    /// Also print each file and shell step, for debugging.
    #[arg(short, long)]
    verbose: bool,

    /// Serve /healthz and /metrics on this address, for example 127.0.0.1:9464.
    #[arg(long)]
    health_addr: Option<SocketAddr>,

    /// Read the worker credential from this machine's worker store, as an installed service does.
    #[arg(long, hide = true)]
    stored_credential: bool,
}

#[derive(Debug, Args)]
pub struct BgShimArgs {
    /// The task's private state directory, holding its command file.
    #[arg(long)]
    pub task_dir: PathBuf,
    /// Stop the command after this many seconds.
    #[arg(long)]
    pub timeout_secs: u64,
}

#[derive(Serialize)]
struct OutpostOutput {
    target_id: Uuid,
    name: String,
    status: String,
}

pub async fn run(base_url: &str, workspace: Option<&str>, args: WorkerArgs) -> ExitCode {
    if let WorkerCommand::Doctor(args) = args.command {
        return doctor(base_url, args).await;
    }
    match execute(base_url, workspace, args).await {
        Ok(()) => ExitCode::Ok,
        Err(error) => exit::report(&error),
    }
}

#[cfg(unix)]
async fn doctor(base_url: &str, args: DoctorArgs) -> ExitCode {
    super::worker_doctor::run(base_url, args).await
}

#[cfg(not(unix))]
async fn doctor(_base_url: &str, _args: DoctorArgs) -> ExitCode {
    exit::report(&CtError::Usage("worker requires Linux or macOS".into()))
}

async fn execute(base_url: &str, workspace: Option<&str>, args: WorkerArgs) -> CtResult<()> {
    match args.command {
        WorkerCommand::Doctor(_) => Ok(()),
        WorkerCommand::Start(args) => start(base_url, args).await,
        WorkerCommand::BgShim(args) => bg_shim(args).await,
        WorkerCommand::Service { command } => worker_service::execute(base_url, workspace, command),
        WorkerCommand::Worktrees { command } => worktrees(base_url, command),
        WorkerCommand::Outpost {
            command: OutpostCommand::Create { name, shared, json },
        } => {
            let store = WorkerStore::open_default(base_url)?;
            let client = build_client(base_url, workspace)?;
            let created = client.create_outpost(&name, shared).await?;
            let boot =
                WorkerClient::exchange(base_url, &created.registration.reference, CLI_VERSION)
                    .await?;
            if created.target.target_id != Some(boot.target_id) {
                return Err(CtError::Protocol(
                    "worker registration target mismatch".into(),
                ));
            }
            store.save(&WorkerCredential {
                target_id: boot.target_id,
                name: created.target.name.clone(),
                token: boot.credential,
            })?;
            let human = output::outpost_created_line(&created.target.name, boot.target_id);
            let result = OutpostOutput {
                target_id: boot.target_id,
                name: created.target.name,
                status: "pending".into(),
            };
            output::emit_worker(&result, &human, json).map_err(CtError::Store)
        }
        WorkerCommand::Outpost {
            command: OutpostCommand::Ls { json },
        } => {
            let choices = build_client(base_url, workspace)?.list_outposts().await?;
            let choices: Vec<_> = choices
                .into_iter()
                .filter(|choice| choice.target_id.is_some())
                .collect();
            let human = output::outpost_list_text(&choices);
            output::emit_worker(&choices, &human, json).map_err(CtError::Store)
        }
        WorkerCommand::Outpost {
            command: OutpostCommand::Archive { outpost, json },
        } => {
            let client = build_client(base_url, workspace)?;
            let choices = client.list_outposts().await?;
            let choice = resolve(&choices, &outpost)?;
            let id = choice
                .target_id
                .ok_or_else(|| CtError::Usage("managed execution cannot be archived".into()))?;
            client.archive_outpost(id).await?;
            let human = output::outpost_archived_line(&choice.name);
            let result = OutpostOutput {
                target_id: id,
                name: choice.name.clone(),
                status: "archived".into(),
            };
            output::emit_worker(&result, &human, json).map_err(CtError::Store)
        }
        WorkerCommand::Status { outpost, json } => {
            let statuses = build_client(base_url, workspace)?
                .list_outpost_status()
                .await?;
            let choices: Vec<_> = statuses
                .iter()
                .map(|status| status.choice.clone())
                .collect();
            let target_id = resolve(&choices, &outpost)?.target_id;
            let status = statuses
                .iter()
                .find(|status| status.choice.target_id == target_id)
                .ok_or_else(|| CtError::Usage("outpost not found".into()))?;
            output::emit_worker(status, &output::outpost_status_text(status), json)
                .map_err(CtError::Store)
        }
    }
}

fn resolve<'a>(
    choices: &'a [api::ExecutorChoicePublic],
    selector: &str,
) -> CtResult<&'a api::ExecutorChoicePublic> {
    let matches: Vec<_> = choices
        .iter()
        .filter(|c| {
            c.target_id.is_some()
                && (c.name == selector || c.target_id.is_some_and(|id| id.to_string() == selector))
        })
        .collect();
    match matches.as_slice() {
        [choice] => Ok(choice),
        [] => Err(CtError::Usage("outpost not found".into())),
        _ => Err(CtError::Usage(
            "outpost name is ambiguous; use its id".into(),
        )),
    }
}

#[cfg(unix)]
fn build_registration(
    identity: &crate::worker::config::WorkdirIdentity,
    concurrency: u16,
    label: Option<&str>,
) -> CtResult<api::RegisterWorkerRequest> {
    let os = if cfg!(target_os = "macos") {
        "darwin"
    } else {
        "linux"
    };
    let mut capabilities = vec![
        api::ExecutorCapability::FilesRead,
        api::ExecutorCapability::FilesWrite,
        api::ExecutorCapability::Shell,
        api::ExecutorCapability::Artifacts,
        api::ExecutorCapability::BackgroundShell,
    ];
    if crate::worker::worktree::git_supports_worktrees() {
        capabilities.push(api::ExecutorCapability::Worktree);
    }
    Ok(api::RegisterWorkerRequest {
        worker_instance_id: Uuid::new_v4(),
        worker_installation_id: identity.installation_id,
        workdir_id: identity.workdir_id,
        protocol_version: 1
            .try_into()
            .map_err(|_| CtError::Protocol("invalid worker protocol".into()))?,
        max_assignments: u64::from(concurrency)
            .try_into()
            .map_err(|_| CtError::Usage("invalid worker concurrency".into()))?,
        os_arch: format!("{os}-{}", std::env::consts::ARCH)
            .parse()
            .map_err(|_| {
                CtError::Usage("worker requires Linux or macOS on x86_64 or aarch64".into())
            })?,
        capabilities,
        label: label.map(str::to_owned),
        repository: None,
    })
}

#[cfg(unix)]
async fn start(base_url: &str, args: StartArgs) -> CtResult<()> {
    use crate::worker::{
        background::ShimImage,
        config::WorkdirIdentity,
        executor::WorkdirExecutor,
        runtime, shell,
        telemetry::{self, Fields, Level},
    };
    use std::sync::Arc;
    use tokio_util::sync::CancellationToken;
    let json_logs = args.log_format == LogFormat::Json;
    telemetry::init(json_logs, usize::from(args.concurrency), args.verbose);
    if !args.workdir.is_dir() {
        return Err(CtError::Usage(
            "--workdir must name an existing directory".into(),
        ));
    }
    if let Some(label) = args.label.as_deref()
        && (label.contains('/') || label.contains('\\') || label.chars().any(char::is_control))
    {
        return Err(CtError::Usage(
            "--label must be a short display name, not a path".into(),
        ));
    }
    let store = WorkerStore::open_default(base_url)?;
    let (credential, rotates) = resolve_credential(base_url, &store, &args).await?;
    if args.reset_folder {
        WorkdirIdentity::forget(&args.workdir, store.state_root())?;
    }
    let identity = Arc::new(WorkdirIdentity::open(
        &args.workdir,
        store.state_root(),
        credential.target_id,
    )?);
    let environment = shell::environment(&args.env, args.inherit_env)
        .map_err(|code| CtError::Usage(code.into()))?;
    let client = WorkerClient::new(base_url, &credential.token, CLI_VERSION)?;
    let registration = build_registration(&identity, args.concurrency, args.label.as_deref())?;
    let shim = match std::env::current_exe() {
        Ok(path) => ShimImage::open(path, &environment).await,
        Err(error) => Err(error),
    }
    .map_err(|_| CtError::Store("worker executable unavailable".into()))?;
    if let Some(reason) = shim.refusal() {
        telemetry::emit(
            Level::Warn,
            "background_unavailable",
            Fields::default(),
            &format!("Background jobs are unavailable inside this service unit: {reason}"),
        );
    }
    let executor = Arc::new(WorkdirExecutor::new(
        identity,
        environment,
        vec![credential.token.clone()],
        shim,
    ));
    let rotation: Option<runtime::RotationHook> = rotates.then(|| {
        let rotator = Arc::new(Rotator {
            store,
            client: client.clone(),
            executor: executor.clone(),
            target_id: credential.target_id,
            name: credential.name.clone(),
            token: std::sync::Mutex::new(credential.token.clone()),
        });
        Arc::new(move || -> runtime::RotationFuture {
            let rotator = rotator.clone();
            Box::pin(async move { rotator.rotate().await })
        }) as runtime::RotationHook
    });
    let signals = runtime::Signals::default();
    let handler = signals.clone();
    let draining = executor.clone();
    let listen = |kind| {
        tokio::signal::unix::signal(kind)
            .map_err(|_| CtError::Protocol("worker signal handler unavailable".into()))
    };
    let mut term = listen(tokio::signal::unix::SignalKind::terminate())?;
    let mut interrupt = listen(tokio::signal::unix::SignalKind::interrupt())?;
    tokio::spawn(async move {
        tokio::select! {_=interrupt.recv()=>{},_=term.recv()=>{}}
        telemetry::stopping(draining.running());
        handler.drain.cancel();
        tokio::select! {_=interrupt.recv()=>{},_=term.recv()=>{}}
        telemetry::emit(
            Level::Info,
            "worker_cancelling",
            Fields::default(),
            "Cancelling running operations.",
        );
        handler.abort.cancel();
    });
    let health_stop = CancellationToken::new();
    if let Some(address) = args.health_addr {
        let listener = telemetry::bind(address).await?;
        if !address.ip().is_loopback() {
            telemetry::emit(
                Level::Warn,
                "health_exposed",
                Fields::default(),
                "--health-addr is not a loopback address; /healthz and /metrics have no authentication.",
            );
        }
        telemetry::serve(listener, health_stop.clone());
    }
    if json_logs {
        telemetry::emit(Level::Info, "worker_serving", Fields::default(), "");
    } else {
        let name = if credential.name == worker_credentials::UNNAMED_OUTPOST {
            output::short_id(credential.target_id)
        } else {
            credential.name.clone()
        };
        output::print_worker_header(&output::WorkerHeader {
            name: &name,
            folder: &executor.identity.path,
            label: args.label.as_deref(),
            server: base_url,
            concurrency: args.concurrency,
            version: CLI_VERSION,
        });
    }
    let outcome = runtime::run(
        client,
        executor,
        registration,
        signals,
        credential.target_id,
        rotation,
    )
    .await;
    health_stop.cancel();
    if outcome.is_ok() {
        telemetry::stopped();
    }
    outcome
}

#[cfg(not(unix))]
async fn start(_base_url: &str, _args: StartArgs) -> CtResult<()> {
    Err(CtError::Usage("worker requires Linux or macOS".into()))
}

#[cfg(unix)]
#[derive(Serialize)]
struct WorktreeRow {
    outpost_id: Uuid,
    #[serde(flatten)]
    worktree: crate::worker::worktree::Listed,
}

#[cfg(unix)]
#[derive(Serialize)]
struct PrunedRow {
    outpost_id: Uuid,
    #[serde(flatten)]
    pruned: crate::worker::worktree::Pruned,
}

#[cfg(unix)]
fn state_directories(store: &WorkerStore, outpost: Option<&str>) -> CtResult<Vec<(Uuid, PathBuf)>> {
    let wanted = match outpost {
        None => None,
        Some(selector) => Some(match Uuid::parse_str(selector) {
            Ok(id) => id,
            Err(_) => store.load(selector)?.target_id,
        }),
    };
    let mut found = Vec::new();
    let Ok(outposts) = std::fs::read_dir(store.state_root()) else {
        return Ok(found);
    };
    for entry in outposts.flatten() {
        let Some(id) = entry
            .file_name()
            .to_str()
            .and_then(|name| Uuid::parse_str(name).ok())
        else {
            continue;
        };
        if wanted.is_some_and(|wanted| wanted != id) || !entry.path().is_dir() {
            continue;
        }
        let Ok(workdirs) = std::fs::read_dir(entry.path()) else {
            continue;
        };
        for workdir in workdirs.flatten() {
            if workdir.path().join("worktrees").is_dir() {
                found.push((id, workdir.path()));
            }
        }
    }
    found.sort();
    Ok(found)
}

#[cfg(unix)]
fn worktrees(base_url: &str, command: WorktreesCommand) -> CtResult<()> {
    use crate::worker::worktree;
    let store = WorkerStore::open_default(base_url)?;
    match command {
        WorktreesCommand::Ls { outpost, json } => {
            let mut rows = Vec::new();
            for (outpost_id, state) in state_directories(&store, outpost.as_deref())? {
                rows.extend(
                    worktree::list(&state)
                        .into_iter()
                        .map(|worktree| WorktreeRow {
                            outpost_id,
                            worktree,
                        }),
                );
            }
            let human = if rows.is_empty() {
                "No worktrees on this machine.".to_owned()
            } else {
                rows.iter()
                    .map(|row| {
                        let record = &row.worktree.record;
                        output::terminal_text(&format!(
                            "{}  {}  from {}  {}  {}",
                            record.branch,
                            row.worktree.state,
                            record.base,
                            idle_text(record.last_used),
                            row.worktree.path.display()
                        ))
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            };
            output::emit_worker(&rows, &human, json).map_err(CtError::Store)
        }
        WorktreesCommand::Prune { outpost, json } => {
            let mut rows = Vec::new();
            for (outpost_id, state) in state_directories(&store, outpost.as_deref())? {
                rows.extend(
                    worktree::prune(&state, std::time::SystemTime::now(), worktree::MAX_IDLE)
                        .into_iter()
                        .map(|pruned| PrunedRow { outpost_id, pruned }),
                );
            }
            let human = if rows.is_empty() {
                "Nothing to prune. Only worktrees unused for 14 days are considered.".to_owned()
            } else {
                rows.iter()
                    .map(|row| {
                        output::terminal_text(&format!(
                            "{}  {}",
                            row.pruned.branch, row.pruned.outcome
                        ))
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            };
            output::emit_worker(&rows, &human, json).map_err(CtError::Store)
        }
    }
}

#[cfg(unix)]
fn idle_text(last_used: u64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs());
    format!("used {}d ago", now.saturating_sub(last_used) / 86_400)
}

#[cfg(not(unix))]
fn worktrees(_base_url: &str, _command: WorktreesCommand) -> CtResult<()> {
    Err(CtError::Usage("worker requires Linux or macOS".into()))
}

#[cfg(unix)]
async fn bg_shim(args: BgShimArgs) -> CtResult<()> {
    super::bg_shim::run(args).await
}

#[cfg(not(unix))]
async fn bg_shim(_args: BgShimArgs) -> CtResult<()> {
    Err(CtError::Usage("worker requires Linux or macOS".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn choice(name: &str, target_id: Option<Uuid>) -> api::ExecutorChoicePublic {
        api::ExecutorChoicePublic {
            availability: api::ExecutorAvailability::Available,
            capabilities: vec![api::ExecutorCapability::Shell],
            kind: if target_id.is_some() {
                api::ExecutorChoiceKind::Outpost
            } else {
                api::ExecutorChoiceKind::Managed
            },
            last_verified_at: None,
            name: name.to_owned(),
            scope: api::ExecutorScope::Personal,
            target_id,
            verification_error: None,
            git: false,
            head_branch: None,
            branches: Vec::new(),
            worktree_supported: false,
            cli_version: None,
            cli_version_status: None,
            workers_disabled: None,
        }
    }

    #[test]
    fn ca_wo_21_resolve_matches_an_outpost_by_name_or_id() {
        let first = Uuid::from_u128(1);
        let second = Uuid::from_u128(2);
        let choices = vec![choice("build", Some(first)), choice("deploy", Some(second))];

        assert_eq!(resolve(&choices, "build").unwrap().target_id, Some(first));
        assert_eq!(
            resolve(&choices, &second.to_string()).unwrap().target_id,
            Some(second)
        );
    }

    #[test]
    fn ca_wo_21_resolve_rejects_an_unknown_or_ambiguous_selector() {
        let choices = vec![
            choice("build", Some(Uuid::from_u128(1))),
            choice("build", Some(Uuid::from_u128(2))),
        ];

        let ambiguous = resolve(&choices, "build").unwrap_err();
        assert!(
            matches!(&ambiguous, CtError::Usage(message) if message.contains("ambiguous")),
            "got {ambiguous:?}"
        );
        let missing = resolve(&choices, "release").unwrap_err();
        assert!(
            matches!(&missing, CtError::Usage(message) if message == "outpost not found"),
            "got {missing:?}"
        );
    }

    #[test]
    fn ca_wo_21_resolve_never_selects_managed_execution() {
        let choices = vec![choice("Managed", None)];

        assert!(matches!(
            resolve(&choices, "Managed"),
            Err(CtError::Usage(_))
        ));
    }
}
