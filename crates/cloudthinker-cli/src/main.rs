//! `cloudthinker` — the customer-facing CLI.
//!
//! A job-runner: log in via the browser, submit a headless prompt to Anna, and
//! watch it to a terminal state. Domain crates own the wire (`cloudthinker-client`);
//! this crate is clap dispatch + the render/exit engine.

// Tests lean on unwrap/expect/panic; the deny lints stay in force for all
// non-test code.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

// Dev-deps used only by the integration test target (`tests/e2e.rs`); silence
// `unused_crate_dependencies` for the bin's own test build.
#[cfg(test)]
use assert_cmd as _;
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

use clap::{Args, Parser, Subcommand};
use uuid::Uuid;

use engine::exit::ExitCode;

const DEFAULT_BASE_URL: &str = "https://app.cloudthinker.io";
const DEFAULT_TIMEOUT_SECS: u64 = 2400;

#[derive(Debug, Parser)]
#[command(
    name = "cloudthinker",
    version,
    about = "CloudThinker CLI — headless chat with Anna",
    disable_help_subcommand = true,
    after_help = "Agent guidance: cloudthinker --skill (then --skill <module> as needed)."
)]
struct Cli {
    #[arg(long, num_args = 0..=1, default_missing_value = "index", value_name = "MODULE",
        help = "Print the bundled agent skill or one module and exit")]
    skill: Option<skill::Module>,

    /// API base URL (consent page + `/api/v1` live under it).
    #[arg(long, env = "CLOUDTHINKER_URL", default_value = DEFAULT_BASE_URL, global = true)]
    url: String,

    /// Use stored credentials for this workspace ID or exact name.
    #[arg(long, env = commands::WORKSPACE_ENV_VAR, global = true)]
    workspace: Option<String>,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Run this machine as an outpost worker, and manage its outposts.
    Worker(commands::worker::WorkerArgs),
    /// Log in via your browser.
    Login(LoginArgs),
    /// Log out and clear stored credentials.
    Logout(LogoutArgs),
    /// Show the live account and workspace for the selected credential.
    Whoami {
        /// Emit the machine-readable identity contract.
        #[arg(long)]
        json: bool,
    },
    /// Read the credential the CLI uses for the selected workspace.
    Auth(AuthArgs),
    /// Send a headless prompt to Anna, or check a run's status.
    Chat(ChatArgs),
    /// Inspect or watch a tracked code review by its merge-request URL.
    Review(ReviewArgs),
    /// Update `cloudthinker` to the latest GitHub release.
    Update(UpdateArgs),
    /// Run the local coding agent (pi) against this workspace.
    Agent(AgentArgs),
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

    /// Use a short code instead of a loopback browser callback.
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
    after_help = "Examples:\n  cloudthinker chat -p 'Check production health'\n  cloudthinker chat -p 'Now inspect the database' --continue <run-or-conversation-uuid>\n  cloudthinker chat -p 'Start an audit' --no-wait --json\n  cloudthinker chat status <run-uuid> --wait\n  cloudthinker chat ls --limit 10"
)]
struct ChatArgs {
    #[command(subcommand)]
    command: Option<ChatSub>,

    /// Prompt to send to Anna (headless).
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
struct ReviewArgs {
    #[command(subcommand)]
    command: ReviewSub,
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
    let cli = Cli::parse();
    dispatch(cli).await.process()
}

async fn dispatch(cli: Cli) -> ExitCode {
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
    let base_url = cli.url;
    let workspace = cli.workspace;
    if workspace.is_some() && commands::env_token_is_set() {
        engine::output::eprintln_error("--workspace cannot be used with CLOUDTHINKER_TOKEN");
        return ExitCode::Usage;
    }
    let command = cli
        .command
        .unwrap_or_else(|| Command::Agent(AgentArgs { args: Vec::new() }));
    if workspace.is_some() && matches!(&command, Command::Login(_)) {
        engine::output::eprintln_error(
            "--workspace selects stored credentials and cannot be used with login",
        );
        return ExitCode::Usage;
    }
    match command {
        Command::Login(args) => {
            commands::login::run(&base_url, args.no_browser, args.device_auth).await
        }
        Command::Logout(args) => {
            commands::logout::run(&base_url, workspace.as_deref(), args.all).await
        }
        Command::Whoami { json } => {
            commands::whoami::run(&base_url, workspace.as_deref(), json).await
        }
        Command::Auth(args) => match args.command {
            AuthSub::Token => commands::auth::run_token(&base_url, workspace.as_deref()).await,
        },
        Command::Chat(args) => match args.command {
            Some(ChatSub::Status {
                run_id,
                json,
                wait,
                timeout,
            }) => {
                commands::chat::run_status(
                    &base_url,
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
                commands::chat::run_list(&base_url, workspace.as_deref(), conversation, limit, json)
                    .await
            }
            None => match args.prompt {
                Some(prompt) => {
                    commands::chat::run_prompt(
                        &base_url,
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
            ReviewSub::Status { mr_url, json } => {
                commands::review::run_status(&base_url, workspace.as_deref(), &mr_url, json).await
            }
            ReviewSub::Findings { mr_url, json } => {
                commands::review::run_findings(&base_url, workspace.as_deref(), &mr_url, json).await
            }
            ReviewSub::Watch {
                mr_url,
                json,
                timeout,
            } => {
                commands::review::run_watch(&base_url, workspace.as_deref(), &mr_url, json, timeout)
                    .await
            }
        },
        Command::Worker(args) => commands::worker::run(&base_url, workspace.as_deref(), args).await,
        // Self-update talks to GitHub releases, not the CloudThinker API; the
        // global `--url` only picks the release channel (the prod origin
        // follows stable, any other origin dev prereleases), and `--workspace`
        // stays ignored.
        Command::Update(args) => commands::update::run(args.force, args.json, &base_url).await,
        Command::Agent(args) => {
            commands::agent::run(&base_url, workspace.as_deref(), args.args).await
        }
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
    fn auth_token_parses_as_its_own_subcommand() {
        let cli = Cli::try_parse_from(["cloudthinker", "auth", "token"]).expect("valid auth args");

        match cli.command.expect("a named subcommand") {
            Command::Auth(args) => assert!(matches!(args.command, AuthSub::Token)),
            _ => panic!("expected auth command"),
        }
    }
}
