//! `cloudthinker` — the customer-facing CLI.
//!
//! A job-runner: log in via the browser, submit a headless prompt to CloudThinker, and
//! watch it to a terminal state. Domain crates own the wire (`cloudthinker-client`);
//! this crate is clap dispatch + the render/exit engine.

// Dev-deps used only by the integration test target (`tests/e2e.rs`); silence
// `unused_crate_dependencies` for the bin's own test build.
#[cfg(test)]
use assert_cmd as _;
#[cfg(test)]
use base64 as _;
#[cfg(test)]
use portable_pty as _;
#[cfg(test)]
use predicates as _;

mod commands;
mod engine;
mod skill;
#[cfg(unix)]
mod worker;

use std::ffi::OsString;
use std::path::PathBuf;

use clap::parser::ValueSource;
use clap::{Args, CommandFactory, FromArgMatches, Parser, Subcommand, ValueEnum};
use cloudthinker_client::{
    CyberFindingStatus, CyberIntensity, CyberSeverity, CyberTriageState, FindingFilter,
};
use uuid::Uuid;

/// Probe intensity as a CLI choice. Mapped to the client's `CyberIntensity` so
/// the wire enum stays in `cloudthinker-client`.
#[derive(Debug, Clone, Copy, ValueEnum)]
enum IntensityArg {
    Safe,
    Aggressive,
    Full,
}

impl From<IntensityArg> for CyberIntensity {
    fn from(value: IntensityArg) -> Self {
        match value {
            IntensityArg::Safe => CyberIntensity::Safe,
            IntensityArg::Aggressive => CyberIntensity::Aggressive,
            IntensityArg::Full => CyberIntensity::Full,
        }
    }
}

/// A finding's agent-owned status, as a `finding ls` filter.
#[derive(Debug, Clone, Copy, ValueEnum)]
enum FindingStatusArg {
    Open,
    Resolved,
    Dismissed,
    NeedsVerification,
}

impl From<FindingStatusArg> for CyberFindingStatus {
    fn from(value: FindingStatusArg) -> Self {
        match value {
            FindingStatusArg::Open => CyberFindingStatus::Open,
            FindingStatusArg::Resolved => CyberFindingStatus::Resolved,
            FindingStatusArg::Dismissed => CyberFindingStatus::Dismissed,
            FindingStatusArg::NeedsVerification => CyberFindingStatus::NeedsVerification,
        }
    }
}

/// A finding's human-owned triage state, as a `finding ls` filter.
#[derive(Debug, Clone, Copy, ValueEnum)]
enum TriageStateArg {
    None,
    InProgress,
    AwaitingRetest,
}

impl From<TriageStateArg> for CyberTriageState {
    fn from(value: TriageStateArg) -> Self {
        match value {
            TriageStateArg::None => CyberTriageState::None,
            TriageStateArg::InProgress => CyberTriageState::InProgress,
            TriageStateArg::AwaitingRetest => CyberTriageState::AwaitingRetest,
        }
    }
}

/// A finding severity, as a `finding ls` filter.
#[derive(Debug, Clone, Copy, ValueEnum)]
enum SeverityArg {
    Critical,
    High,
    Medium,
    Low,
    Info,
}

impl From<SeverityArg> for CyberSeverity {
    fn from(value: SeverityArg) -> Self {
        match value {
            SeverityArg::Critical => CyberSeverity::Critical,
            SeverityArg::High => CyberSeverity::High,
            SeverityArg::Medium => CyberSeverity::Medium,
            SeverityArg::Low => CyberSeverity::Low,
            SeverityArg::Info => CyberSeverity::Info,
        }
    }
}

use engine::exit::ExitCode;

const DEFAULT_BASE_URL: &str = cloudthinker_client::DEFAULT_BASE_URL;
const DEFAULT_TIMEOUT_SECS: u64 = 2400;

#[derive(Debug, Parser)]
#[command(
    name = "cloudthinker",
    version,
    about = "CloudThinker CLI — chat, local pentests, code review, and developer workflows",
    disable_help_subcommand = true,
    after_help = "Get started:\n  cloudthinker login                  Sign in; over SSH it shows a short code instead of a browser\n  cloudthinker                        Start the coding agent in this folder\n  cloudthinker chat -p 'Check prod'   Ask Anna from a script; only the answer goes to stdout\n\nAgent guidance: cloudthinker --skill (then --skill <module> as needed)."
)]
struct Cli {
    #[arg(long, num_args = 0..=1, default_missing_value = "index", value_name = "MODULE",
        help = "Print the internal operating guide for AI agents and exit")]
    skill: Option<skill::Module>,

    /// CloudThinker address, for example https://app.cloudthinker.io. Defaults to the address of your last `login --url`, else https://app.cloudthinker.io.
    #[arg(long, env = "CLOUDTHINKER_URL", global = true, value_name = "URL")]
    url: Option<String>,

    /// Use stored credentials for this workspace ID or exact name.
    #[arg(long, env = commands::WORKSPACE_ENV_VAR, global = true)]
    workspace: Option<String>,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Start the coding agent in this folder. A bare `cloudthinker` runs it too.
    Agent(AgentArgs),
    Cloud(commands::cloud::CloudArgs),
    /// Send a headless prompt to Anna, or check a run's status.
    Chat(ChatArgs),
    /// Review local changes or inspect a tracked review by its merge-request URL.
    Review(ReviewArgs),
    /// Log in. Over SSH or without a display, it shows a short code instead of a browser.
    Login(LoginArgs),
    /// Log out and clear stored credentials.
    Logout(LogoutArgs),
    /// Show the live account and workspace for the selected credential.
    Whoami {
        /// Emit the machine-readable identity contract.
        #[arg(long)]
        json: bool,
    },
    /// List, switch, or print the stored workspace logins for this host.
    Auth(AuthArgs),
    /// Run a local Cyber pentest for an App target.
    Cyber(CyberArgs),
    /// Run this machine as an outpost worker, and manage its outposts.
    Worker(commands::worker::WorkerArgs),
    /// Update `cloudthinker` to the latest GitHub release.
    Update(UpdateArgs),
    /// Print a shell completion script.
    #[command(
        after_help = "Examples:\n  cloudthinker completion bash > ~/.local/share/bash-completion/completions/cloudthinker\n  cloudthinker completion zsh > \"${fpath[1]}/_cloudthinker\"\n  cloudthinker completion fish > ~/.config/fish/completions/cloudthinker.fish"
    )]
    Completion {
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },
}

#[derive(Debug, Args)]
#[command(
    trailing_var_arg = true,
    allow_hyphen_values = true,
    disable_help_flag = true
)]
struct AgentArgs {
    /// Arguments passed to the local agent verbatim.
    #[arg(value_name = "AGENT_ARGS")]
    args: Vec<OsString>,
}

#[derive(Debug, Args)]
struct LoginArgs {
    /// Print the consent URL instead of opening a browser.
    #[arg(long)]
    no_browser: bool,

    /// Use a short code instead of a loopback browser callback. Set CLOUDTHINKER_LOGIN=browser to keep the browser over SSH.
    #[arg(long)]
    device_auth: bool,
}

#[derive(Debug, Args)]
struct AuthArgs {
    #[command(subcommand)]
    command: AuthSub,
}

#[derive(Debug, Subcommand)]
enum AuthSub {
    /// Print the current access token for a tool that shells out for a bearer; it is a secret.
    Token,
    /// List the stored workspace logins for this host and mark the active one.
    Status {
        /// Emit the stored logins as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Make a stored workspace login the active one for this host.
    Switch {
        /// Workspace ID or exact name.
        workspace: String,
    },
}

#[derive(Debug, Args)]
struct LogoutArgs {
    /// Clear every stored workspace credential for this host.
    #[arg(long)]
    all: bool,
}

#[derive(Debug, Args)]
#[command(
    args_conflicts_with_subcommands = true,
    after_help = "Examples:\n  cloudthinker chat -p 'Check production health'\n  kubectl logs deploy/api --tail 200 | cloudthinker chat -p 'Why does this crash?'\n  cloudthinker chat -p - < prompt.md\n  cloudthinker chat -p 'Now inspect the database' --continue <run-or-conversation-uuid>\n  cloudthinker chat -p 'Start an audit' --no-wait --json\n  cloudthinker chat status <run-uuid> --wait\n  cloudthinker chat ls --limit 10"
)]
struct ChatArgs {
    #[command(subcommand)]
    command: Option<ChatSub>,

    /// Prompt to send to CloudThinker (headless). `-` reads the prompt from stdin; other piped stdin is added to the prompt.
    #[arg(short = 'p', long = "prompt")]
    prompt: Option<String>,

    /// Continue the conversation identified by a run or conversation UUID.
    #[arg(long = "continue", value_name = "UUID")]
    continue_id: Option<Uuid>,

    /// Return immediately after submission instead of polling the run.
    #[arg(long)]
    no_wait: bool,

    /// Emit a JSON envelope on stdout instead of plain text.
    #[arg(long)]
    json: bool,

    /// Stop waiting after this many seconds (the run continues server-side).
    #[arg(long, default_value_t = DEFAULT_TIMEOUT_SECS)]
    timeout: u64,
}

#[derive(Debug, Subcommand)]
enum ChatSub {
    /// Show the status (and answer, if finished) of a run.
    Status {
        /// The run id returned by a previous `chat -p`.
        run_id: Uuid,
        /// Emit a JSON envelope on stdout instead of plain text.
        #[arg(long)]
        json: bool,
        /// Poll until the run reaches a terminal state.
        #[arg(long)]
        wait: bool,
        /// Stop waiting after this many seconds (the run continues server-side).
        #[arg(long, default_value_t = DEFAULT_TIMEOUT_SECS)]
        timeout: u64,
    },
    /// List recent headless runs in the selected workspace.
    Ls {
        /// Only show runs in this conversation.
        #[arg(long)]
        conversation: Option<Uuid>,
        /// Maximum number of runs to show.
        #[arg(long, default_value_t = 10, value_parser = clap::value_parser!(u64).range(1..=50))]
        limit: u64,
        /// Emit JSON on stdout instead of a human table.
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Args)]
#[command(
    args_conflicts_with_subcommands = true,
    after_help = "Examples:\n  cloudthinker review\n  cloudthinker review --base origin/develop\n  cloudthinker review --json --timeout 300\n  cloudthinker review --base origin/main --fail-on high\n  cloudthinker review status <MR_URL> --json"
)]
struct ReviewArgs {
    #[command(subcommand)]
    command: Option<ReviewSub>,

    /// Review changes since the merge base with this ref, including dirty edits.
    #[arg(long)]
    base: Option<String>,

    /// Emit one JSON result object on stdout.
    #[arg(long)]
    json: bool,

    /// Stop the local review agent after this many seconds.
    #[arg(long, default_value_t = DEFAULT_TIMEOUT_SECS, value_parser = clap::value_parser!(u64).range(1..=7200))]
    timeout: u64,

    /// Exit 6 when a finding has this severity or a worse one.
    #[arg(long, value_enum, value_name = "SEVERITY")]
    fail_on: Option<commands::review::FailOn>,
}

#[derive(Debug, Args)]
struct CyberArgs {
    #[command(subcommand)]
    command: CyberSub,
}

#[derive(Debug, Args)]
struct CyberConfigArgs {
    #[command(subcommand)]
    command: CyberConfigSub,
}

#[derive(Debug, Args)]
struct CyberAppArgs {
    #[command(subcommand)]
    command: CyberAppSub,
}

#[derive(Debug, Subcommand)]
enum CyberAppSub {
    /// List Apps visible in the selected workspace.
    Ls {
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Args)]
struct CyberDomainArgs {
    #[command(subcommand)]
    command: CyberDomainSub,
}

#[derive(Debug, Subcommand)]
enum CyberDomainSub {
    /// List workspace domains and their ownership proof.
    Ls {
        #[arg(long)]
        json: bool,
    },
    /// Add a domain to prove ownership of.
    Add {
        domain: String,
        #[arg(long)]
        json: bool,
    },
    /// Re-check a domain's ownership proof.
    Verify {
        domain_id: Uuid,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Subcommand)]
enum CyberConfigSub {
    /// Print the resolved project-over-user configuration.
    Show {
        #[arg(long)]
        json: bool,
    },
    /// Print one resolved setting.
    Get {
        key: String,
        #[arg(long)]
        json: bool,
    },
    /// Set one project setting, or a user default with --user.
    Set {
        key: String,
        value: String,
        #[arg(long)]
        user: bool,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Args)]
struct CyberRunArgs {
    #[command(subcommand)]
    command: CyberRunSub,
}

#[derive(Debug, Subcommand)]
enum CyberRunSub {
    /// Resolve or create an App, check setup, launch a local run, and bind this session.
    Open {
        /// Existing App UUID or exact App name.
        #[arg(long)]
        app: Option<String>,
        /// Create an App when --app does not resolve.
        #[arg(long)]
        name: Option<String>,
        /// Target used when creating an App.
        #[arg(long)]
        target: Option<String>,
        /// Select OWASP API coverage when creating an App.
        #[arg(long)]
        api: bool,
        /// How far a probe may push. Mode and scan-focus are derived, not set.
        #[arg(long, value_enum)]
        intensity: Option<IntensityArg>,
        /// The owning agent-cli conversation to bind.
        #[arg(long, env = "CLOUDTHINKER_CONVERSATION_ID")]
        conversation_id: Option<Uuid>,
        #[arg(
            long = "include",
            help = "Include a path or host pattern. May be repeated."
        )]
        include: Vec<String>,
        #[arg(
            long = "exclude",
            help = "Exclude a path or host pattern. May be repeated."
        )]
        exclude: Vec<String>,
        #[arg(long)]
        json: bool,
    },
    /// Create a RUNNING local run for an App, without binding a session.
    Launch {
        /// The App to test (CloudThinker > Cyber > the App's UUID).
        app_id: Uuid,
        /// How far a probe may push. Mode and scan-focus are derived, not set.
        #[arg(long, value_enum)]
        intensity: Option<IntensityArg>,
        #[arg(
            long = "include",
            help = "Include a path or host pattern. May be repeated."
        )]
        include: Vec<String>,
        #[arg(
            long = "exclude",
            help = "Exclude a path or host pattern. May be repeated."
        )]
        exclude: Vec<String>,
        #[arg(long)]
        json: bool,
    },
    /// Bind this session's conversation to an existing local run and print the brief.
    Bind {
        /// The run UUID returned by `cyber run launch`.
        run_id: Uuid,
        /// This session's CloudThinker conversation UUID.
        conversation_id: Uuid,
        #[arg(long)]
        json: bool,
    },
    /// Show a run's state: lifecycle, host, session binding, finding counts. --wait polls.
    Status {
        /// The run UUID returned by `cyber run launch`.
        run_id: Uuid,
        /// Poll until the run reaches a terminal state.
        #[arg(long)]
        wait: bool,
        /// Stop waiting after this many seconds (the run continues server-side).
        #[arg(long, default_value_t = DEFAULT_TIMEOUT_SECS)]
        timeout: u64,
        #[arg(long)]
        json: bool,
    },
    /// Cancel a run through the shared terminal transition CAS.
    Cancel {
        run_id: Uuid,
        #[arg(long)]
        json: bool,
    },
    /// Settle a finished run; the backend's finalize CAS decides the winner.
    Settle {
        run_id: Uuid,
        /// Settle as failed instead of succeeded.
        #[arg(long)]
        failed: bool,
        /// A short closing note recorded with the settlement (max 2000 chars).
        #[arg(long, value_name = "TEXT")]
        message: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Replay a run's mirrored local-agent transcript.
    Session {
        run_id: Uuid,
        #[arg(long)]
        json: bool,
    },
    /// Upload bounded text files or binary artifacts into the run's durable evidence tree.
    Evidence {
        run_id: Uuid,
        /// Evidence files to upload, by path (relative paths upload under that name).
        #[arg(value_name = "FILE", num_args = 1..)]
        paths: Vec<String>,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Subcommand)]
enum CyberSub {
    #[command(about = "Discover the run's surface on this machine.")]
    Discover {
        run_id: Uuid,
        #[arg(long, default_value_t = 5000, value_parser = clap::value_parser!(u64).range(1..=20000))]
        max_urls: u64,
        #[arg(long, default_value_t = 720, value_parser = clap::value_parser!(u64).range(1..=720))]
        timeout: u64,
        #[arg(long)]
        json: bool,
    },
    /// The App record in this workspace.
    App(CyberAppArgs),
    /// Workspace domain ownership proof.
    Domain(CyberDomainArgs),
    /// A Cyber pentest run that executes on this machine.
    Run(CyberRunArgs),
    /// The backend-owned probe plan and its coverage gate.
    Probe(CyberProbeArgs),
    /// An App's findings.
    Finding(CyberFindingArgs),
    /// Pull canonical App memory to a local directory for a local run.
    Memory(CyberMemoryArgs),
    /// Read or write project/user Cyber defaults.
    Config(CyberConfigArgs),
    /// Report local pentest readiness; `--fix` repairs the probe toolpack.
    Doctor {
        /// Download every missing or outdated pinned tool for this platform.
        #[arg(long)]
        fix: bool,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Args)]
struct CyberMemoryArgs {
    #[command(subcommand)]
    command: CyberMemorySub,
}

#[derive(Debug, Subcommand)]
enum CyberMemorySub {
    /// Pull an App's canonical findings, surface, and optional context files.
    Pull {
        app_id: Uuid,
        #[arg(long, required = true)]
        output: PathBuf,
        /// Download non-credential App context documents into the output directory.
        #[arg(long)]
        include_context: bool,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Args)]
struct CyberProbeArgs {
    #[command(subcommand)]
    command: CyberProbeSub,
}

#[derive(Debug, Subcommand)]
enum CyberProbeSub {
    /// Print the backend-issued probe plan for this run.
    Plan {
        run_id: Uuid,
        #[arg(long)]
        json: bool,
    },
    /// Execute the backend's plan on this machine and report the results.
    Exec {
        run_id: Uuid,
        /// Probe selected rows; omit to probe all executable rows.
        #[arg(long, value_delimiter = ',')]
        rows: Vec<String>,
        /// Identity role from [cyber.auth], or `anonymous` for no auth header.
        #[arg(long)]
        identity: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Show the run's coverage ledger and completion gate.
    Coverage {
        run_id: Uuid,
        #[arg(long)]
        json: bool,
    },
    /// Read the attack-surface overview for authoring fan-out themes.
    Surface {
        run_id: Uuid,
        #[arg(long)]
        json: bool,
    },
    /// Group plan rows for parallel work.
    Partition {
        run_id: Uuid,
        /// Themes doc as JSON, or `@path` to a JSON file. Omit for surface only.
        #[arg(long)]
        themes: Option<String>,
        #[arg(long)]
        max_rows_per_shard: Option<i64>,
        #[arg(long)]
        only_status: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Report a reasoned observation for one or more plan rows.
    Ingest {
        run_id: Uuid,
        plan_id: String,
        /// The plan rows this observation closes (comma-separated row_ids).
        #[arg(long = "row", value_delimiter = ',')]
        rows: Vec<String>,
        /// Coverage status, e.g. covered, candidate, blocked, skipped_with_reason.
        #[arg(long)]
        status: String,
        /// Why this status; required for candidate, blocked, skipped_with_reason.
        #[arg(long, default_value = "")]
        reason: String,
        /// A ref to the evidence backing the observation.
        #[arg(long, default_value = "")]
        evidence: String,
        /// The worker that examined the rows.
        #[arg(long, default_value = "cloudthinker-cli")]
        worker: String,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Args)]
struct CyberFindingArgs {
    #[command(subcommand)]
    command: CyberFindingSub,
}

#[derive(Debug, Subcommand)]
enum CyberFindingSub {
    /// List an App's findings.
    Ls {
        app_id: Uuid,
        #[arg(long, default_value_t = 1)]
        page: u64,
        #[arg(long, default_value_t = 50)]
        take: u64,
        /// Keep only findings on this agent-owned status.
        #[arg(long, value_enum)]
        status: Option<FindingStatusArg>,
        /// Keep only findings at this human triage state.
        #[arg(long, value_enum)]
        triage: Option<TriageStateArg>,
        /// Keep only findings at this severity.
        #[arg(long, value_enum)]
        severity: Option<SeverityArg>,
        #[arg(long)]
        json: bool,
    },
    /// Show one finding.
    Get {
        finding_id: Uuid,
        #[arg(long)]
        json: bool,
    },
    /// Export an App's findings as PDF, or one finding with --finding.
    Export {
        app_id: Uuid,
        #[arg(long)]
        finding_id: Option<Uuid>,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Subcommand)]
enum ReviewSub {
    /// One-shot summary of a review's current status.
    Status {
        /// The GitLab/GitHub merge-request URL.
        mr_url: String,
        /// Emit a JSON envelope on stdout instead of plain text.
        #[arg(long)]
        json: bool,
    },
    /// List a review's findings, worst-severity first.
    Findings {
        /// The GitLab/GitHub merge-request URL.
        mr_url: String,
        /// Emit a JSON envelope on stdout instead of plain text.
        #[arg(long)]
        json: bool,
    },
    /// Poll a review until it reaches a terminal state.
    Watch {
        /// The GitLab/GitHub merge-request URL.
        mr_url: String,
        /// Emit a JSON envelope on stdout instead of plain text.
        #[arg(long)]
        json: bool,
        /// Stop waiting after this many seconds (the review continues server-side).
        #[arg(long, default_value_t = DEFAULT_TIMEOUT_SECS)]
        timeout: u64,
        /// Exit 6 when the finished review has a finding of this severity or a worse one.
        #[arg(long, value_enum, value_name = "SEVERITY")]
        fail_on: Option<commands::review::FailOn>,
    },
}

#[derive(Debug, Args)]
struct UpdateArgs {
    /// Install the latest release even when already up to date.
    #[arg(long)]
    force: bool,

    /// Emit a JSON envelope on stdout instead of plain text.
    #[arg(long)]
    json: bool,
}

#[tokio::main]
async fn main() -> std::process::ExitCode {
    let matches = Cli::command().get_matches();
    let url_from_flag = url_from_flag(&matches);
    let cli = match Cli::from_arg_matches(&matches) {
        Ok(cli) => cli,
        Err(error) => error.exit(),
    };
    dispatch(cli, url_from_flag).await.process()
}

fn url_from_flag(matches: &clap::ArgMatches) -> bool {
    matches.value_source("url") == Some(ValueSource::CommandLine)
}

fn resolve_base_url(explicit: Option<String>) -> String {
    cloudthinker_client::resolve_base_url(
        explicit,
        &cloudthinker_client::CliConfig::load_default(),
        DEFAULT_BASE_URL,
    )
}

fn shows_update_notice(command: &Command) -> bool {
    matches!(
        command,
        Command::Chat(_)
            | Command::Review(_)
            | Command::Login(_)
            | Command::Logout(_)
            | Command::Whoami { .. }
    ) || matches!(command, Command::Auth(args) if !matches!(args.command, AuthSub::Token))
}

async fn dispatch(cli: Cli, url_from_flag: bool) -> ExitCode {
    if let Some(module) = cli.skill {
        if cli.command.is_some() {
            engine::output::eprintln_error("--skill cannot be combined with a command");
            return ExitCode::Usage;
        }
        return match engine::output::print_document(module.document()) {
            Ok(()) => ExitCode::Ok,
            Err(error) => {
                engine::output::eprintln_error(&error);
                ExitCode::JobFailed
            }
        };
    }
    let base_url = resolve_base_url(cli.url);
    let workspace = cli.workspace;
    let command = cli
        .command
        .unwrap_or_else(|| Command::Agent(AgentArgs { args: Vec::new() }));
    if workspace.is_some() && commands::env_token_is_set() {
        engine::output::eprintln_error("--workspace cannot be used with CLOUDTHINKER_TOKEN");
        return ExitCode::Usage;
    }
    if workspace.is_some() && matches!(&command, Command::Login(_)) {
        engine::output::eprintln_error(
            "--workspace selects stored credentials and cannot be used with login",
        );
        return ExitCode::Usage;
    }
    let notice = if shows_update_notice(&command) {
        commands::update::start_notice(&base_url)
    } else {
        None
    };
    let code = run_command(command, &base_url, workspace, url_from_flag).await;
    if let Some(notice) = notice {
        notice.finish().await;
    }
    code
}

async fn run_command(
    command: Command,
    base_url: &str,
    workspace: Option<String>,
    url_from_flag: bool,
) -> ExitCode {
    match command {
        Command::Login(args) => {
            commands::login::run(
                base_url,
                commands::login::LoginOptions {
                    no_browser: args.no_browser,
                    device_auth: args.device_auth,
                    remember_url: url_from_flag,
                },
            )
            .await
        }
        Command::Logout(args) => {
            commands::logout::run(base_url, workspace.as_deref(), args.all).await
        }
        Command::Cloud(args) => commands::cloud::run(base_url, workspace.as_deref(), args).await,
        Command::Whoami { json } => {
            commands::whoami::run(base_url, workspace.as_deref(), json).await
        }
        Command::Auth(args) => match args.command {
            AuthSub::Token => commands::auth::run_token(base_url, workspace.as_deref()).await,
            AuthSub::Status { json } => commands::auth::run_status(base_url, json),
            AuthSub::Switch {
                workspace: selection,
            } => commands::auth::run_switch(base_url, &selection),
        },
        Command::Chat(args) => match args.command {
            Some(ChatSub::Status {
                run_id,
                json,
                wait,
                timeout,
            }) => {
                commands::chat::run_status(
                    base_url,
                    workspace.as_deref(),
                    run_id,
                    json,
                    wait,
                    timeout,
                )
                .await
            }
            Some(ChatSub::Ls {
                conversation,
                limit,
                json,
            }) => {
                commands::chat::run_list(base_url, workspace.as_deref(), conversation, limit, json)
                    .await
            }
            None => match args.prompt {
                Some(prompt) => {
                    let prompt = match engine::piped_input::resolve_prompt(&prompt) {
                        Ok(prompt) => prompt,
                        Err(message) => {
                            engine::output::eprintln_error(&message);
                            return ExitCode::Usage;
                        }
                    };
                    commands::chat::run_prompt(
                        base_url,
                        workspace.as_deref(),
                        &prompt,
                        args.continue_id,
                        args.no_wait,
                        args.json,
                        args.timeout,
                    )
                    .await
                }
                None => {
                    engine::output::eprintln_error(
                        "provide `-p <PROMPT>`, `chat status <run_id>`, or `chat ls`",
                    );
                    ExitCode::Usage
                }
            },
        },
        Command::Review(args) => match args.command {
            None => {
                commands::review::run_local(commands::review::LocalReviewOptions {
                    base_url,
                    workspace: workspace.as_deref(),
                    base_ref: args.base.as_deref(),
                    json: args.json,
                    timeout_secs: args.timeout,
                    fail_on: args.fail_on,
                })
                .await
            }
            Some(ReviewSub::Status { mr_url, json }) => {
                commands::review::run_status(base_url, workspace.as_deref(), &mr_url, json).await
            }
            Some(ReviewSub::Findings { mr_url, json }) => {
                commands::review::run_findings(base_url, workspace.as_deref(), &mr_url, json).await
            }
            Some(ReviewSub::Watch {
                mr_url,
                json,
                timeout,
                fail_on,
            }) => {
                commands::review::run_watch(
                    base_url,
                    workspace.as_deref(),
                    &mr_url,
                    json,
                    timeout,
                    fail_on,
                )
                .await
            }
        },
        Command::Cyber(args) => match args.command {
            CyberSub::App(args) => match args.command {
                CyberAppSub::Ls { json } => {
                    commands::cyber::list_apps(base_url, workspace.as_deref(), json).await
                }
            },
            CyberSub::Domain(args) => match args.command {
                CyberDomainSub::Ls { json } => {
                    commands::cyber::list_domains(base_url, workspace.as_deref(), json).await
                }
                CyberDomainSub::Add { domain, json } => {
                    commands::cyber::create_domain(base_url, workspace.as_deref(), &domain, json)
                        .await
                }
                CyberDomainSub::Verify { domain_id, json } => {
                    commands::cyber::check_domain(base_url, workspace.as_deref(), domain_id, json)
                        .await
                }
            },
            CyberSub::Run(args) => match args.command {
                CyberRunSub::Open {
                    app,
                    name,
                    target,
                    api,
                    intensity,
                    conversation_id,
                    include,
                    exclude,
                    json,
                } => {
                    commands::cyber::open_run(
                        base_url,
                        workspace.as_deref(),
                        commands::cyber::RunOpenOptions {
                            app_ref: app.as_deref(),
                            app_name: name.as_deref(),
                            target: target.as_deref(),
                            api_coverage: api,
                            intensity: intensity.map(Into::into),
                            conversation_id,
                            scope: commands::cyber::RunScopeOptions { include, exclude },
                            json,
                        },
                    )
                    .await
                }
                CyberRunSub::Launch {
                    app_id,
                    intensity,
                    include,
                    exclude,
                    json,
                } => {
                    commands::cyber::run_launch(
                        base_url,
                        workspace.as_deref(),
                        app_id,
                        intensity.map(Into::into),
                        commands::cyber::RunScopeOptions { include, exclude },
                        json,
                    )
                    .await
                }
                CyberRunSub::Bind {
                    run_id,
                    conversation_id,
                    json,
                } => {
                    commands::cyber::bind_run(
                        base_url,
                        workspace.as_deref(),
                        run_id,
                        conversation_id,
                        json,
                    )
                    .await
                }
                CyberRunSub::Status {
                    run_id,
                    wait,
                    timeout,
                    json,
                } => {
                    if wait {
                        commands::cyber::watch(
                            base_url,
                            workspace.as_deref(),
                            run_id,
                            timeout,
                            json,
                        )
                        .await
                    } else {
                        commands::cyber::run_status(base_url, workspace.as_deref(), run_id, json)
                            .await
                    }
                }
                CyberRunSub::Cancel { run_id, json } => {
                    commands::cyber::cancel(base_url, workspace.as_deref(), run_id, json).await
                }
                CyberRunSub::Settle {
                    run_id,
                    failed,
                    message,
                    json,
                } => {
                    commands::cyber::settle(
                        base_url,
                        workspace.as_deref(),
                        run_id,
                        failed,
                        message,
                        json,
                    )
                    .await
                }
                CyberRunSub::Session { run_id, json } => {
                    commands::cyber::run_session(base_url, workspace.as_deref(), run_id, json).await
                }
                CyberRunSub::Evidence {
                    run_id,
                    paths,
                    json,
                } => {
                    commands::cyber::submit_evidence(
                        base_url,
                        workspace.as_deref(),
                        run_id,
                        &paths,
                        json,
                    )
                    .await
                }
            },
            CyberSub::Discover {
                run_id,
                max_urls,
                timeout,
                json,
            } => {
                commands::cyber_discovery::discover(
                    base_url,
                    workspace.as_deref(),
                    run_id,
                    max_urls,
                    timeout,
                    json,
                )
                .await
            }
            CyberSub::Probe(args) => match args.command {
                CyberProbeSub::Plan { run_id, json } => {
                    commands::cyber::plan(base_url, workspace.as_deref(), run_id, json).await
                }
                CyberProbeSub::Exec {
                    run_id,
                    rows,
                    identity,
                    json,
                } => {
                    commands::cyber::scan(
                        base_url,
                        workspace.as_deref(),
                        run_id,
                        &rows,
                        identity.as_deref(),
                        json,
                    )
                    .await
                }
                CyberProbeSub::Coverage { run_id, json } => {
                    commands::cyber::coverage(base_url, workspace.as_deref(), run_id, json).await
                }
                CyberProbeSub::Surface { run_id, json } => {
                    commands::cyber::surface(base_url, workspace.as_deref(), run_id, json).await
                }
                CyberProbeSub::Partition {
                    run_id,
                    themes,
                    max_rows_per_shard,
                    only_status,
                    json,
                } => {
                    commands::cyber::partition(
                        base_url,
                        workspace.as_deref(),
                        run_id,
                        themes.as_deref(),
                        max_rows_per_shard,
                        only_status,
                        json,
                    )
                    .await
                }
                CyberProbeSub::Ingest {
                    run_id,
                    plan_id,
                    rows,
                    status,
                    reason,
                    evidence,
                    worker,
                    json,
                } => {
                    commands::cyber::ingest(
                        base_url,
                        workspace.as_deref(),
                        run_id,
                        commands::cyber::IngestOptions {
                            plan_id: &plan_id,
                            rows: &rows,
                            status: &status,
                            reason: &reason,
                            evidence: &evidence,
                            worker: &worker,
                            json,
                        },
                    )
                    .await
                }
            },
            CyberSub::Finding(args) => match args.command {
                CyberFindingSub::Ls {
                    app_id,
                    page,
                    take,
                    status,
                    triage,
                    severity,
                    json,
                } => {
                    let filter = FindingFilter {
                        status: status.map(Into::into),
                        triage_state: triage.map(Into::into),
                        severity: severity.map(Into::into),
                    };
                    commands::cyber::findings_list(
                        base_url,
                        workspace.as_deref(),
                        app_id,
                        page,
                        take,
                        filter,
                        json,
                    )
                    .await
                }
                CyberFindingSub::Get { finding_id, json } => {
                    commands::cyber::finding_get(base_url, workspace.as_deref(), finding_id, json)
                        .await
                }
                CyberFindingSub::Export {
                    app_id,
                    finding_id,
                    json,
                } => {
                    commands::cyber::findings_export(
                        base_url,
                        workspace.as_deref(),
                        app_id,
                        finding_id,
                        json,
                    )
                    .await
                }
            },
            CyberSub::Memory(args) => match args.command {
                CyberMemorySub::Pull {
                    app_id,
                    output,
                    include_context,
                    json,
                } => {
                    commands::cyber_memory::pull(
                        base_url,
                        workspace.as_deref(),
                        app_id,
                        &output,
                        include_context,
                        json,
                    )
                    .await
                }
            },
            CyberSub::Config(args) => match args.command {
                CyberConfigSub::Show { json } => commands::cyber_config::show(json),
                CyberConfigSub::Get { key, json } => commands::cyber_config::get(&key, json),
                CyberConfigSub::Set {
                    key,
                    value,
                    user,
                    json,
                } => commands::cyber_config::set(&key, &value, user, json),
            },
            CyberSub::Doctor { fix, json } => commands::cyber_doctor::doctor(fix, json).await,
        },
        Command::Worker(args) => commands::worker::run(base_url, workspace.as_deref(), args).await,
        // Self-update talks to GitHub releases, not the CloudThinker API; the
        // global `--url` only picks the release channel (the prod origin
        // follows stable, any other origin dev prereleases), and `--workspace`
        // stays ignored.
        Command::Update(args) => commands::update::run(args.force, args.json, base_url).await,
        Command::Agent(args) => {
            commands::agent::run(base_url, workspace.as_deref(), args.args).await
        }
        Command::Completion { shell } => commands::completion::run(shell, &mut Cli::command()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn login_accepts_explicit_device_auth_without_disabling_no_browser() {
        let cli = Cli::try_parse_from(["cloudthinker", "login", "--device-auth", "--no-browser"])
            .expect("valid login args");

        match cli.command.expect("a named subcommand") {
            Command::Login(args) => {
                assert!(args.device_auth);
                assert!(args.no_browser);
            }
            _ => panic!("expected login command"),
        }
    }

    #[test]
    fn ca_cli_skill_keeps_agent_arguments_verbatim() {
        let cli = Cli::try_parse_from(["cloudthinker", "agent", "--skill", "chat"])
            .expect("valid agent args");
        assert!(cli.skill.is_none());
        match cli.command.expect("a named subcommand") {
            Command::Agent(args) => assert_eq!(args.args, ["--skill", "chat"].map(OsString::from)),
            _ => panic!("expected agent command"),
        }
    }

    #[test]
    fn agent_captures_every_trailing_argument_verbatim() {
        let cli = Cli::try_parse_from([
            "cloudthinker",
            "agent",
            "-p",
            "hello",
            "--model",
            "cloudthinker/pro",
        ])
        .expect("valid agent args");

        match cli.command.expect("a named subcommand") {
            Command::Agent(args) => assert_eq!(
                args.args,
                ["-p", "hello", "--model", "cloudthinker/pro"]
                    .map(OsString::from)
                    .to_vec()
            ),
            _ => panic!("expected agent command"),
        }
    }

    #[test]
    fn agent_hands_the_help_flag_to_the_local_agent() {
        for flag in ["--help", "-h"] {
            let cli = Cli::try_parse_from(["cloudthinker", "agent", flag, "--tui-mode"])
                .expect("valid agent args");

            match cli.command.expect("a named subcommand") {
                Command::Agent(args) => {
                    assert_eq!(args.args, [flag, "--tui-mode"].map(OsString::from).to_vec())
                }
                _ => panic!("expected agent command"),
            }
        }
    }

    #[test]
    fn agent_passes_a_global_option_name_through_after_a_double_dash() {
        let cli = Cli::try_parse_from(["cloudthinker", "agent", "--", "--workspace", "mine"])
            .expect("valid agent args");

        match cli.command.expect("a named subcommand") {
            Command::Agent(args) => assert_eq!(
                args.args,
                ["--workspace", "mine"].map(OsString::from).to_vec()
            ),
            _ => panic!("expected agent command"),
        }
    }

    #[test]
    fn a_bare_invocation_runs_the_agent_with_no_arguments() {
        let cli = Cli::try_parse_from(["cloudthinker"]).expect("a bare invocation parses");

        assert!(cli.command.is_none());
    }

    #[test]
    fn a_bare_invocation_keeps_its_global_options() {
        let cli = Cli::try_parse_from(["cloudthinker", "--workspace", "ops"])
            .expect("a bare invocation with globals parses");

        assert!(cli.command.is_none());
        assert_eq!(cli.workspace.as_deref(), Some("ops"));
    }

    #[test]
    fn only_a_url_flag_counts_as_a_request_to_remember_the_address() {
        for args in [
            [
                "cloudthinker",
                "login",
                "--url",
                "https://dev.cloudthinker.io",
            ],
            [
                "cloudthinker",
                "--url",
                "https://dev.cloudthinker.io",
                "login",
            ],
        ] {
            let matches = Cli::command()
                .try_get_matches_from(args)
                .expect("valid login args");
            assert!(url_from_flag(&matches), "{args:?}");
        }
        let matches = Cli::command()
            .try_get_matches_from(["cloudthinker", "login"])
            .expect("valid login args");
        assert!(!url_from_flag(&matches));
    }

    #[test]
    fn auth_token_parses_as_its_own_subcommand() {
        let cli = Cli::try_parse_from(["cloudthinker", "auth", "token"]).expect("valid auth args");

        match cli.command.expect("a named subcommand") {
            Command::Auth(args) => assert!(matches!(args.command, AuthSub::Token)),
            _ => panic!("expected auth command"),
        }
    }

    #[test]
    fn cyber_discovery_defaults_and_bounds() {
        let args = [
            "cloudthinker",
            "cyber",
            "discover",
            "00000000-0000-0000-0000-000000000001",
        ];
        let cli = Cli::try_parse_from(args).unwrap();
        let Some(Command::Cyber(parsed)) = cli.command else {
            panic!("expected Cyber command");
        };
        assert!(matches!(
            parsed.command,
            CyberSub::Discover {
                max_urls: 5000,
                timeout: 720,
                json: false,
                ..
            }
        ));
        for (flag, value) in [
            ("--max-urls", "0"),
            ("--max-urls", "20001"),
            ("--timeout", "0"),
            ("--timeout", "721"),
        ] {
            assert!(Cli::try_parse_from(args.into_iter().chain([flag, value])).is_err());
        }
        assert!(
            Cli::try_parse_from(args.into_iter().chain([
                "--max-urls",
                "20",
                "--timeout",
                "45",
                "--json"
            ]))
            .is_ok()
        );
    }

    #[test]
    fn cyber_run_bind_parses_with_its_operands() {
        let cli = Cli::try_parse_from([
            "cloudthinker",
            "cyber",
            "run",
            "bind",
            "00000000-0000-0000-0000-000000000001",
            "00000000-0000-0000-0000-000000000002",
        ])
        .expect("valid cyber run bind args");
        match cli.command.expect("a named subcommand") {
            Command::Cyber(args) => match args.command {
                CyberSub::Run(args) => match args.command {
                    CyberRunSub::Bind {
                        run_id,
                        conversation_id,
                        json,
                    } => {
                        assert_eq!(run_id, Uuid::from_u128(1));
                        assert_eq!(conversation_id, Uuid::from_u128(2));
                        assert!(!json);
                    }
                    _ => panic!("expected cyber run bind"),
                },
                _ => panic!("expected cyber run"),
            },
            _ => panic!("expected cyber command"),
        }
    }

    #[test]
    fn cyber_run_session_takes_a_run_id() {
        let cli = Cli::try_parse_from([
            "cloudthinker",
            "cyber",
            "run",
            "session",
            "00000000-0000-0000-0000-000000000003",
            "--json",
        ])
        .expect("valid cyber run session args");
        match cli.command.expect("a named subcommand") {
            Command::Cyber(args) => match args.command {
                CyberSub::Run(args) => match args.command {
                    CyberRunSub::Session { run_id, json } => {
                        assert_eq!(run_id, Uuid::from_u128(3));
                        assert!(json);
                    }
                    _ => panic!("expected cyber run session"),
                },
                _ => panic!("expected cyber run"),
            },
            _ => panic!("expected cyber command"),
        }
    }

    #[test]
    fn cyber_probe_plan_exec_and_coverage_take_a_run_id() {
        for (verb, expected) in [
            ("plan", Uuid::from_u128(4)),
            ("exec", Uuid::from_u128(5)),
            ("coverage", Uuid::from_u128(6)),
        ] {
            let cli = Cli::try_parse_from([
                "cloudthinker",
                "cyber",
                "probe",
                verb,
                &expected.to_string(),
                "--json",
            ])
            .expect("valid cyber probe args");
            let Command::Cyber(args) = cli.command.expect("a named subcommand") else {
                panic!("expected cyber command");
            };
            let CyberSub::Probe(args) = args.command else {
                panic!("expected cyber probe");
            };
            let run_id = match args.command {
                CyberProbeSub::Plan { run_id, json } | CyberProbeSub::Coverage { run_id, json } => {
                    assert!(json);
                    run_id
                }
                CyberProbeSub::Exec {
                    run_id, rows, json, ..
                } => {
                    assert!(json);
                    assert!(rows.is_empty());
                    run_id
                }
                other => panic!("unexpected probe subcommand: {other:?}"),
            };
            assert_eq!(run_id, expected);
        }
    }

    #[test]
    fn cyber_probe_exec_takes_a_lane_row_subset() {
        let run_id = Uuid::from_u128(7);
        let cli = Cli::try_parse_from([
            "cloudthinker",
            "cyber",
            "probe",
            "exec",
            &run_id.to_string(),
            "--rows",
            "row-a,row-b",
        ])
        .expect("valid cyber probe exec args");
        let Command::Cyber(args) = cli.command.expect("a named subcommand") else {
            panic!("expected cyber command");
        };
        let CyberSub::Probe(args) = args.command else {
            panic!("expected cyber probe");
        };
        let CyberProbeSub::Exec { rows, .. } = args.command else {
            panic!("expected cyber probe exec");
        };
        assert_eq!(rows, vec!["row-a".to_string(), "row-b".to_string()]);
    }

    #[test]
    fn cyber_probe_ingest_batches_rows_with_one_status() {
        let run_id = Uuid::from_u128(8);
        let cli = Cli::try_parse_from([
            "cloudthinker",
            "cyber",
            "probe",
            "ingest",
            &run_id.to_string(),
            "wp-abc",
            "--row",
            "row-a,row-b",
            "--status",
            "covered",
            "--reason",
            "workspace-scoped",
        ])
        .expect("valid cyber probe ingest args");
        let Command::Cyber(args) = cli.command.expect("a named subcommand") else {
            panic!("expected cyber command");
        };
        let CyberSub::Probe(args) = args.command else {
            panic!("expected cyber probe");
        };
        let CyberProbeSub::Ingest {
            plan_id,
            rows,
            status,
            worker,
            ..
        } = args.command
        else {
            panic!("expected cyber probe ingest");
        };
        assert_eq!(plan_id, "wp-abc");
        assert_eq!(rows, vec!["row-a".to_string(), "row-b".to_string()]);
        assert_eq!(status, "covered");
        assert_eq!(worker, "cloudthinker-cli");
    }

    #[test]
    fn cyber_run_evidence_takes_multiple_paths() {
        let cli = Cli::try_parse_from([
            "cloudthinker",
            "cyber",
            "run",
            "evidence",
            "00000000-0000-0000-0000-000000000001",
            "a.md",
            "b/b.md",
            "--json",
        ])
        .expect("valid cyber run evidence args");
        match cli.command.expect("a named subcommand") {
            Command::Cyber(args) => match args.command {
                CyberSub::Run(args) => match args.command {
                    CyberRunSub::Evidence {
                        run_id,
                        paths,
                        json,
                    } => {
                        assert_eq!(run_id, Uuid::from_u128(1));
                        assert_eq!(paths, ["a.md", "b/b.md"]);
                        assert!(json);
                    }
                    _ => panic!("expected cyber run evidence"),
                },
                _ => panic!("expected cyber run"),
            },
            _ => panic!("expected cyber command"),
        }
    }

    #[test]
    fn cyber_run_settle_defaults_to_success_with_optional_failure_and_message() {
        let cli = Cli::try_parse_from([
            "cloudthinker",
            "cyber",
            "run",
            "settle",
            "00000000-0000-0000-0000-000000000001",
            "--failed",
            "--message",
            "scan died mid-run",
        ])
        .expect("valid cyber run settle args");
        match cli.command.expect("a named subcommand") {
            Command::Cyber(args) => match args.command {
                CyberSub::Run(args) => match args.command {
                    CyberRunSub::Settle {
                        run_id,
                        failed,
                        message,
                        json,
                    } => {
                        assert_eq!(run_id, Uuid::from_u128(1));
                        assert!(failed);
                        assert_eq!(message.as_deref(), Some("scan died mid-run"));
                        assert!(!json);
                    }
                    _ => panic!("expected cyber run settle"),
                },
                _ => panic!("expected cyber run"),
            },
            _ => panic!("expected cyber command"),
        }
    }

    #[test]
    fn cyber_run_status_wait_parses() {
        let cli = Cli::try_parse_from([
            "cloudthinker",
            "cyber",
            "run",
            "status",
            "00000000-0000-0000-0000-000000000001",
            "--wait",
            "--timeout",
            "3",
        ])
        .expect("valid cyber run status args");
        match cli.command.expect("a named subcommand") {
            Command::Cyber(args) => match args.command {
                CyberSub::Run(args) => match args.command {
                    CyberRunSub::Status {
                        run_id,
                        wait,
                        timeout,
                        json,
                    } => {
                        assert_eq!(run_id, Uuid::from_u128(1));
                        assert!(wait);
                        assert_eq!(timeout, 3);
                        assert!(!json);
                    }
                    _ => panic!("expected cyber run status"),
                },
                _ => panic!("expected cyber run"),
            },
            _ => panic!("expected cyber command"),
        }
    }

    #[test]
    fn cyber_run_open_intensity_flag_parses() {
        let cli = Cli::try_parse_from([
            "cloudthinker",
            "cyber",
            "run",
            "open",
            "--name",
            "api",
            "--target",
            "http://localhost:8000",
            "--intensity",
            "aggressive",
        ])
        .expect("valid cyber run open args");
        match cli.command.expect("a named subcommand") {
            Command::Cyber(args) => match args.command {
                CyberSub::Run(args) => match args.command {
                    CyberRunSub::Open { intensity, .. } => {
                        assert!(matches!(intensity, Some(IntensityArg::Aggressive)));
                    }
                    _ => panic!("expected cyber run open"),
                },
                _ => panic!("expected cyber run"),
            },
            _ => panic!("expected cyber command"),
        }
    }

    #[test]
    fn cyber_run_scope_flags_repeat_for_open_and_launch() {
        let open = Cli::try_parse_from([
            "cloudthinker",
            "cyber",
            "run",
            "open",
            "--name",
            "api",
            "--target",
            "http://localhost:8000",
            "--include",
            "/health",
            "--include",
            "api.example.test",
            "--exclude",
            "/admin",
            "--exclude",
            "internal.example.test",
        ])
        .expect("valid scoped cyber run open args");
        match open.command.expect("a named subcommand") {
            Command::Cyber(args) => match args.command {
                CyberSub::Run(args) => match args.command {
                    CyberRunSub::Open {
                        include, exclude, ..
                    } => {
                        assert_eq!(include, ["/health", "api.example.test"]);
                        assert_eq!(exclude, ["/admin", "internal.example.test"]);
                    }
                    _ => panic!("expected cyber run open"),
                },
                _ => panic!("expected cyber run"),
            },
            _ => panic!("expected cyber command"),
        }

        let launch = Cli::try_parse_from([
            "cloudthinker",
            "cyber",
            "run",
            "launch",
            "11111111-1111-1111-1111-111111111111",
            "--include",
            "/health",
            "--include",
            "api.example.test",
            "--exclude",
            "/admin",
            "--exclude",
            "internal.example.test",
        ])
        .expect("valid cyber run launch args");
        match launch.command.expect("a named subcommand") {
            Command::Cyber(args) => match args.command {
                CyberSub::Run(args) => match args.command {
                    CyberRunSub::Launch {
                        include, exclude, ..
                    } => {
                        assert_eq!(include, ["/health", "api.example.test"]);
                        assert_eq!(exclude, ["/admin", "internal.example.test"]);
                    }
                    _ => panic!("expected cyber run launch"),
                },
                _ => panic!("expected cyber run"),
            },
            _ => panic!("expected cyber command"),
        }
    }

    #[test]
    fn cyber_finding_ls_filters_parse() {
        let cli = Cli::try_parse_from([
            "cloudthinker",
            "cyber",
            "finding",
            "ls",
            "11111111-1111-1111-1111-111111111111",
            "--status",
            "needs-verification",
            "--triage",
            "in-progress",
            "--severity",
            "high",
        ])
        .expect("valid cyber finding ls args");
        match cli.command.expect("a named subcommand") {
            Command::Cyber(args) => match args.command {
                CyberSub::Finding(args) => match args.command {
                    CyberFindingSub::Ls {
                        status,
                        triage,
                        severity,
                        ..
                    } => {
                        assert!(matches!(status, Some(FindingStatusArg::NeedsVerification)));
                        assert!(matches!(triage, Some(TriageStateArg::InProgress)));
                        assert!(matches!(severity, Some(SeverityArg::High)));
                    }
                    _ => panic!("expected cyber finding ls"),
                },
                _ => panic!("expected cyber finding"),
            },
            _ => panic!("expected cyber command"),
        }
    }

    #[test]
    fn cyber_doctor_fix_and_json_flags_parse() {
        let cli = Cli::try_parse_from(["cloudthinker", "cyber", "doctor", "--fix", "--json"])
            .expect("valid cyber doctor args");
        match cli.command.expect("a named subcommand") {
            Command::Cyber(args) => match args.command {
                CyberSub::Doctor { fix, json } => {
                    assert!(fix);
                    assert!(json);
                }
                _ => panic!("expected cyber doctor"),
            },
            _ => panic!("expected cyber command"),
        }
    }

    #[test]
    fn cyber_operational_surface_parses_canonical_commands() {
        let cases = [
            vec![
                "cyber",
                "run",
                "open",
                "--name",
                "Store",
                "--target",
                "https://store.test",
            ],
            vec![
                "cyber",
                "config",
                "set",
                "auth.primary",
                "CYBER_PRIMARY_AUTH",
            ],
            vec!["cyber", "app", "ls"],
            vec!["cyber", "doctor"],
            vec!["cyber", "doctor", "--fix"],
            vec!["cyber", "domain", "ls"],
            vec!["cyber", "domain", "add", "example.com"],
            vec![
                "cyber",
                "domain",
                "verify",
                "00000000-0000-0000-0000-000000000001",
            ],
            vec![
                "cyber",
                "run",
                "cancel",
                "00000000-0000-0000-0000-000000000001",
            ],
            vec![
                "cyber",
                "finding",
                "ls",
                "00000000-0000-0000-0000-000000000001",
            ],
            vec![
                "cyber",
                "finding",
                "get",
                "00000000-0000-0000-0000-000000000001",
            ],
            vec![
                "cyber",
                "finding",
                "export",
                "00000000-0000-0000-0000-000000000001",
            ],
        ];
        for args in cases {
            let mut argv = vec!["cloudthinker"];
            argv.extend(args);
            Cli::try_parse_from(argv).expect("canonical cyber command parses");
        }
    }
}
