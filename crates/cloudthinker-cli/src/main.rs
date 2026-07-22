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
use predicates as _;

mod commands;
mod engine;

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
    disable_help_subcommand = true
)]
struct Cli {
    /// API base URL (consent page + `/api/v1` live under it).
    #[arg(long, env = "CLOUDTHINKER_URL", default_value = DEFAULT_BASE_URL, global = true)]
    url: String,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Log in via your browser.
    Login(LoginArgs),
    /// Log out and clear stored credentials.
    Logout,
    /// Send a headless prompt to Anna, or check a run's status.
    Chat(ChatArgs),
    /// Inspect or watch a tracked code review by its merge-request URL.
    Review(ReviewArgs),
}

#[derive(Debug, Args)]
struct LoginArgs {
    /// Print the consent URL instead of opening a browser.
    #[arg(long)]
    no_browser: bool,
}

#[derive(Debug, Args)]
struct ChatArgs {
    #[command(subcommand)]
    status: Option<ChatStatus>,

    /// Prompt to send to Anna (headless).
    #[arg(short = 'p', long = "prompt")]
    prompt: Option<String>,

    /// Emit a JSON envelope on stdout instead of plain text.
    #[arg(long)]
    json: bool,

    /// Stop waiting after this many seconds (the run continues server-side).
    #[arg(long, default_value_t = DEFAULT_TIMEOUT_SECS)]
    timeout: u64,
}

#[derive(Debug, Subcommand)]
enum ChatStatus {
    /// Show the status (and answer, if finished) of a run.
    Status {
        /// The run id returned by a previous `chat -p`.
        run_id: Uuid,
        /// Emit a JSON envelope on stdout instead of plain text.
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

#[tokio::main]
async fn main() -> std::process::ExitCode {
    let cli = Cli::parse();
    dispatch(cli).await.process()
}

async fn dispatch(cli: Cli) -> ExitCode {
    let base_url = cli.url;
    match cli.command {
        Command::Login(args) => commands::login::run(&base_url, args.no_browser).await,
        Command::Logout => commands::logout::run(&base_url).await,
        Command::Chat(args) => match args.status {
            Some(ChatStatus::Status { run_id, json }) => {
                commands::chat::run_status(&base_url, run_id, json).await
            }
            None => match args.prompt {
                Some(prompt) => {
                    commands::chat::run_prompt(&base_url, &prompt, args.json, args.timeout).await
                }
                None => {
                    engine::output::eprintln_error(
                        "provide a prompt with `-p <PROMPT>` or use `chat status <run_id>`",
                    );
                    ExitCode::Usage
                }
            },
        },
        Command::Review(args) => match args.command {
            ReviewSub::Status { mr_url, json } => {
                commands::review::run_status(&base_url, &mr_url, json).await
            }
            ReviewSub::Findings { mr_url, json } => {
                commands::review::run_findings(&base_url, &mr_url, json).await
            }
            ReviewSub::Watch {
                mr_url,
                json,
                timeout,
            } => commands::review::run_watch(&base_url, &mr_url, json, timeout).await,
        },
    }
}
