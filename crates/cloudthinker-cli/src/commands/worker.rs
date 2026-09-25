use std::path::PathBuf;

use clap::{Args, Subcommand};
use cloudthinker_client::auth::worker_store::{WORKER_TOKEN_ENV, WorkerCredential, WorkerStore};
use cloudthinker_client::{CtError, CtResult, worker_api::WorkerClient, worker_types as api};
use serde::Serialize;
use uuid::Uuid;

use super::build_client;
use super::worker_service::{self, ServiceCommand};
use crate::engine::{
    exit::{self, ExitCode},
    output,
};

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
    /// Show one outpost's availability as the workspace sees it.
    Status {
        /// Outpost UUID or exact name.
        #[arg(long, env = "CLOUDTHINKER_OUTPOST_ID")]
        outpost: String,

        /// Emit the machine-readable outpost contract.
        #[arg(long)]
        json: bool,
    },
    #[command(hide = true)]
    BgShim(BgShimArgs),
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

        /// Emit the machine-readable outpost contract.
        #[arg(long)]
        json: bool,
    },
    /// List the outposts this workspace can run work on.
    Ls {
        /// Emit the machine-readable outpost contract.
        #[arg(long)]
        json: bool,
    },
    /// Retire an outpost so no further work is routed to it.
    Archive {
        /// Outpost UUID or exact name.
        outpost: String,

        /// Emit the machine-readable outpost contract.
        #[arg(long)]
        json: bool,
    },
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

    /// Pass `NAME=VALUE` to shell commands; repeat for more.
    #[arg(long = "env")]
    env: Vec<String>,

    /// Pass this shell's whole environment to shell commands.
    #[arg(long)]
    inherit_env: bool,

    #[arg(long, hide = true)]
    stored_credential: bool,
}

#[derive(Debug, Args)]
pub struct BgShimArgs {
    #[arg(long)]
    pub task_dir: PathBuf,
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
    match execute(base_url, workspace, args).await {
        Ok(()) => ExitCode::Ok,
        Err(error) => exit::report(&error),
    }
}

async fn execute(base_url: &str, workspace: Option<&str>, args: WorkerArgs) -> CtResult<()> {
    match args.command {
        WorkerCommand::Start(args) => start(base_url, args).await,
        WorkerCommand::BgShim(args) => bg_shim(args).await,
        WorkerCommand::Service { command } => {
            worker_service::execute(base_url, workspace, command).await
        }
        WorkerCommand::Outpost {
            command: OutpostCommand::Create { name, shared, json },
        } => {
            let store = WorkerStore::open_default(base_url)?;
            let client = build_client(base_url, workspace)?;
            let created = client.create_outpost(&name, shared).await?;
            let boot = WorkerClient::exchange(base_url, &created.registration.reference).await?;
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
            let choices = build_client(base_url, workspace)?.list_outposts().await?;
            let choice = resolve(&choices, &outpost)?;
            output::emit_worker(choice, &output::outpost_status_line(choice), json)
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

#[derive(Debug, PartialEq, Eq)]
enum CredentialSource {
    Register(String),
    Stored(String),
    Env { target_id: Uuid, token: String },
}

fn credential_source(
    outpost: Option<&str>,
    register: Option<String>,
    stored_credential: bool,
    env_token: Option<String>,
) -> CtResult<CredentialSource> {
    if stored_credential && register.is_some() {
        return Err(CtError::Usage(
            "--stored-credential cannot be combined with --register".into(),
        ));
    }
    if let Some(reference) = register {
        return Ok(CredentialSource::Register(reference));
    }
    if stored_credential {
        return Ok(CredentialSource::Stored(
            outpost
                .ok_or_else(|| CtError::Usage("--stored-credential requires --outpost".into()))?
                .to_owned(),
        ));
    }
    if let Some(token) = env_token {
        let target_id = outpost
            .and_then(|selector| Uuid::parse_str(selector).ok())
            .ok_or_else(|| {
                CtError::Usage("CLOUDTHINKER_WORKER_TOKEN requires an outpost id".into())
            })?;
        if token.trim().is_empty() {
            return Err(CtError::Auth("worker credential is empty".into()));
        }
        return Ok(CredentialSource::Env { target_id, token });
    }
    Ok(CredentialSource::Stored(
        outpost
            .ok_or_else(|| CtError::Usage("provide --outpost or --register".into()))?
            .to_owned(),
    ))
}

fn registered_credential(
    outpost: Option<&str>,
    boot: api::WorkerBootstrap,
) -> CtResult<WorkerCredential> {
    if outpost
        .and_then(|selector| Uuid::parse_str(selector).ok())
        .is_some_and(|id| id != boot.target_id)
    {
        return Err(CtError::Protocol(
            "worker registration target mismatch".into(),
        ));
    }
    Ok(WorkerCredential {
        target_id: boot.target_id,
        name: outpost.unwrap_or("outpost").to_owned(),
        token: boot.credential,
    })
}

async fn resolve_credential(
    base_url: &str,
    store: &WorkerStore,
    args: &StartArgs,
) -> CtResult<WorkerCredential> {
    let source = credential_source(
        args.outpost.as_deref(),
        args.register.clone(),
        args.stored_credential,
        std::env::var(WORKER_TOKEN_ENV).ok(),
    )?;
    match source {
        CredentialSource::Register(reference) => {
            let boot = WorkerClient::exchange(base_url, &reference).await?;
            let credential = registered_credential(args.outpost.as_deref(), boot)?;
            store.save(&credential)?;
            Ok(credential)
        }
        CredentialSource::Stored(selector) => store.load(&selector),
        CredentialSource::Env { target_id, token } => Ok(WorkerCredential {
            target_id,
            name: "outpost".into(),
            token,
        }),
    }
}

#[cfg(unix)]
fn build_registration(
    identity: &crate::worker::config::WorkdirIdentity,
    concurrency: u16,
) -> CtResult<api::RegisterWorkerRequest> {
    let os = if cfg!(target_os = "macos") {
        "darwin"
    } else {
        "linux"
    };
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
        capabilities: vec![
            api::ExecutorCapability::FilesRead,
            api::ExecutorCapability::FilesWrite,
            api::ExecutorCapability::Shell,
            api::ExecutorCapability::Artifacts,
            api::ExecutorCapability::BackgroundShell,
        ],
    })
}

#[cfg(unix)]
async fn start(base_url: &str, args: StartArgs) -> CtResult<()> {
    use crate::worker::{
        background::ShimImage, config::WorkdirIdentity, executor::WorkdirExecutor, runtime, shell,
    };
    use std::sync::Arc;
    if !args.workdir.is_dir() {
        return Err(CtError::Usage(
            "--workdir must name an existing directory".into(),
        ));
    }
    let label = args.label.as_deref().unwrap_or("outpost folder");
    if label.contains('/') || label.contains('\\') || label.chars().any(char::is_control) {
        return Err(CtError::Usage(
            "--label must be a short display name, not a path".into(),
        ));
    }
    let store = WorkerStore::open_default(base_url)?;
    let credential = resolve_credential(base_url, &store, &args).await?;
    let identity = Arc::new(WorkdirIdentity::open(
        &args.workdir,
        store.state_root(),
        credential.target_id,
    )?);
    let environment = shell::environment(&args.env, args.inherit_env)
        .map_err(|code| CtError::Usage(code.into()))?;
    let client = WorkerClient::new(base_url, &credential.token)?;
    let registration = build_registration(&identity, args.concurrency)?;
    let shim = match std::env::current_exe() {
        Ok(path) => ShimImage::open(path, &environment).await,
        Err(error) => Err(error),
    }
    .map_err(|_| CtError::Store("worker executable unavailable".into()))?;
    if let Some(reason) = shim.refusal() {
        output::warn(&format!(
            "Background jobs are unavailable inside this service unit: {reason}"
        ));
    }
    let executor = Arc::new(WorkdirExecutor::new(
        identity,
        environment,
        vec![credential.token],
        shim,
    ));
    let shutdown = tokio_util::sync::CancellationToken::new();
    let signal = shutdown.clone();
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .map_err(|_| CtError::Protocol("worker signal handler unavailable".into()))?;
    tokio::spawn(async move {
        tokio::select! {_=tokio::signal::ctrl_c()=>{},_=term.recv()=>{}}
        signal.cancel();
    });
    output::progress(&output::worker_serving_line(&credential.name, label));
    runtime::run(
        client,
        executor,
        registration,
        shutdown,
        credential.target_id,
    )
    .await
}

#[cfg(not(unix))]
async fn start(_base_url: &str, _args: StartArgs) -> CtResult<()> {
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
        }
    }

    fn bootstrap(target_id: Uuid) -> api::WorkerBootstrap {
        api::WorkerBootstrap {
            credential: "worker-token".into(),
            credential_generation: 1,
            expires_at: chrono::Utc::now(),
            target_id,
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

    #[test]
    fn ca_wo_06_register_wins_over_a_stored_credential() {
        assert_eq!(
            credential_source(Some("build"), Some("one-time".into()), false, None).unwrap(),
            CredentialSource::Register("one-time".into())
        );
    }

    #[test]
    fn ca_wo_06_register_and_stored_credential_together_are_a_usage_error() {
        let error = credential_source(Some("build"), Some("one-time".into()), true, None)
            .expect_err("the two credential sources conflict");
        assert!(
            matches!(&error, CtError::Usage(message) if message.contains("--stored-credential")),
            "got {error:?}"
        );
    }

    #[test]
    fn ca_wo_04_stored_credential_needs_an_outpost_selector() {
        assert_eq!(
            credential_source(Some("build"), None, true, Some("env-token".into())).unwrap(),
            CredentialSource::Stored("build".into())
        );
        assert!(matches!(
            credential_source(None, None, true, None),
            Err(CtError::Usage(_))
        ));
    }

    #[test]
    fn ca_wo_05_environment_token_requires_an_outpost_uuid() {
        let id = Uuid::from_u128(7);
        assert_eq!(
            credential_source(Some(&id.to_string()), None, false, Some("env-token".into()))
                .unwrap(),
            CredentialSource::Env {
                target_id: id,
                token: "env-token".into()
            }
        );
        assert!(matches!(
            credential_source(Some("build"), None, false, Some("env-token".into())),
            Err(CtError::Usage(_))
        ));
    }

    #[test]
    fn ca_wo_05_an_empty_environment_token_is_an_auth_error() {
        let id = Uuid::from_u128(7);
        let error = credential_source(Some(&id.to_string()), None, false, Some("  ".into()))
            .expect_err("an empty credential cannot authenticate");
        assert!(matches!(error, CtError::Auth(_)), "got {error:?}");
    }

    #[test]
    fn ca_wo_04_no_source_at_all_asks_for_an_outpost_or_a_registration() {
        let error = credential_source(None, None, false, None)
            .expect_err("the worker has no credential to use");
        assert!(
            matches!(&error, CtError::Usage(message) if message == "provide --outpost or --register"),
            "got {error:?}"
        );
    }

    #[test]
    fn ca_wo_06_registration_keeps_the_exchanged_target() {
        let id = Uuid::from_u128(9);
        let credential = registered_credential(Some(&id.to_string()), bootstrap(id)).unwrap();
        assert_eq!(credential.target_id, id);
        assert_eq!(credential.token, "worker-token");
        assert_eq!(
            registered_credential(None, bootstrap(id)).unwrap().name,
            "outpost"
        );
    }

    #[test]
    fn ca_wo_06_registration_for_another_outpost_is_a_protocol_error() {
        let Err(error) = registered_credential(
            Some(&Uuid::from_u128(9).to_string()),
            bootstrap(Uuid::from_u128(10)),
        ) else {
            panic!("the exchanged target must match --outpost");
        };
        assert!(matches!(error, CtError::Protocol(_)), "got {error:?}");
    }
}
