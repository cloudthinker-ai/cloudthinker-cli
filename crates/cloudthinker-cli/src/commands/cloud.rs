use std::path::PathBuf;

use clap::{Args, Subcommand, ValueEnum};
use cloudthinker_client::{CloudExecutionInput, CloudResult, CtClient, CtError};
use uuid::Uuid;

use crate::commands::build_client;
use crate::engine::{
    exit::{self, ExitCode},
    output,
};

#[derive(Debug, Args)]
pub struct CloudArgs {
    #[command(subcommand)]
    command: CloudCommand,
}

#[derive(Debug, Subcommand)]
enum CloudCommand {
    Session {
        #[command(subcommand)]
        command: SessionCommand,
    },
    Connections {
        #[arg(long)]
        json: bool,
    },
    LoadSkill {
        #[arg(long)]
        session: Uuid,
        #[arg(long)]
        connection: String,
        #[arg(long)]
        name: Vec<String>,
        #[arg(long)]
        json: bool,
    },
    LoadTools {
        #[arg(long)]
        session: Uuid,
        #[arg(long)]
        connection: String,
        #[arg(long, required = true)]
        tool: Vec<String>,
        #[arg(long)]
        json: bool,
    },
    Exec(ExecArgs),
    Status {
        #[arg(long)]
        session: Uuid,
        #[arg(long, group = "operation")]
        task: Option<String>,
        #[arg(long, group = "operation")]
        write: Option<Uuid>,
        #[arg(long, group = "operation")]
        writes: bool,
        #[arg(long, default_value_t = 0, requires = "task")]
        since: u64,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Subcommand)]
enum SessionCommand {
    Create {
        #[arg(long)]
        title: Option<String>,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum, Default)]
enum Mode {
    #[default]
    Read,
    Write,
}

#[derive(Debug, Args)]
#[command(group(clap::ArgGroup::new("input").required(true).args(["command", "script_file", "write"])))]
struct ExecArgs {
    #[arg(long)]
    session: Uuid,
    #[arg(long)]
    command: Option<String>,
    #[arg(long)]
    script_file: Option<PathBuf>,
    #[arg(long, conflicts_with_all = ["mode", "connection", "background", "reason", "request_id", "timeout"])]
    write: Option<Uuid>,
    #[arg(long, value_enum)]
    mode: Option<Mode>,
    #[arg(long)]
    connection: Vec<String>,
    #[arg(long, default_value_t = 60, value_parser = clap::value_parser!(u16).range(1..=120))]
    timeout: u16,
    #[arg(long)]
    background: bool,
    #[arg(long)]
    reason: Option<String>,
    #[arg(long)]
    request_id: Option<Uuid>,
    #[arg(long)]
    json: bool,
}

pub async fn run(base_url: &str, workspace: Option<&str>, args: CloudArgs) -> ExitCode {
    let client = match build_client(base_url, workspace) {
        Ok(client) => client,
        Err(error) => return exit::report(&error),
    };
    let (result, json) = dispatch(&client, args.command).await;
    match result {
        Ok(result) => match output::emit_cloud(&result, json) {
            Ok(()) => cloud_exit(&result),
            Err(error) => exit::report(&CtError::Transport(error)),
        },
        Err(error) => exit::report(&error),
    }
}

async fn dispatch(
    client: &CtClient,
    command: CloudCommand,
) -> (Result<CloudResult, CtError>, bool) {
    match command {
        CloudCommand::Session {
            command: SessionCommand::Create { title, json },
        } => {
            let cwd = std::env::current_dir().map_err(|error| CtError::Usage(error.to_string()));
            let result = match cwd {
                Ok(cwd) => {
                    client
                        .cloud_create_session(&cwd.to_string_lossy(), title.as_deref())
                        .await
                }
                Err(error) => Err(error),
            };
            (result, json)
        }
        CloudCommand::Connections { json } => (client.cloud_connections().await, json),
        CloudCommand::LoadSkill {
            session,
            connection,
            name,
            json,
        } => (
            client.cloud_load_skill(session, &connection, name).await,
            json,
        ),
        CloudCommand::LoadTools {
            session,
            connection,
            tool,
            json,
        } => (
            client.cloud_load_tools(session, &connection, tool).await,
            json,
        ),
        CloudCommand::Exec(args) => {
            let json = args.json;
            (execute(client, args).await, json)
        }
        CloudCommand::Status {
            session,
            task,
            write,
            writes,
            since,
            json,
        } => {
            let result = match (task, write, writes) {
                (Some(task), None, false) => client.cloud_task_status(session, &task, since).await,
                (None, Some(write), false) => client.cloud_write_status(session, write).await,
                (None, None, true) => client.cloud_writes(session).await,
                _ => Err(CtError::Usage(
                    "Select --task, --write, or --writes.".into(),
                )),
            };
            (result, json)
        }
    }
}

async fn execute(client: &CtClient, args: ExecArgs) -> Result<CloudResult, CtError> {
    if let Some(write) = args.write {
        return client.cloud_run_write(args.session, write).await;
    }
    let script = match (args.command, args.script_file) {
        (Some(script), None) => script,
        (None, Some(path)) => {
            let file = tokio::fs::File::open(&path)
                .await
                .map_err(|error| CtError::Usage(format!("Cannot read script file: {error}")))?;
            let mut script = String::new();
            use tokio::io::AsyncReadExt;
            file.take(1_048_577)
                .read_to_string(&mut script)
                .await
                .map_err(|error| CtError::Usage(format!("Cannot read script file: {error}")))?;
            script
        }
        _ => return Err(CtError::Usage("Select --command or --script-file.".into())),
    };
    if script.is_empty() || script.len() > 1_048_576 {
        return Err(CtError::Usage(
            "Script must contain 1–1048576 UTF-8 bytes.".into(),
        ));
    }
    let write = matches!(args.mode.unwrap_or_default(), Mode::Write);
    if !write && (args.reason.is_some() || args.request_id.is_some()) {
        return Err(CtError::Usage(
            "--reason and --request-id require --mode write.".into(),
        ));
    }
    let reason = if write {
        Some(args.reason.ok_or_else(|| {
            CtError::Usage("Write requests require --reason describing the intended change.".into())
        })?)
    } else {
        None
    };
    let input = CloudExecutionInput {
        session: args.session,
        script,
        connections: args.connection,
        timeout: args.timeout,
        background: args.background,
    };
    if let Some(reason) = reason {
        let request_id = args.request_id.unwrap_or_else(Uuid::new_v4);
        eprintln!(
            "Write request {request_id}; retain it to reconcile an uncertain submission with cloud status --session {} --writes.",
            args.session
        );
        client.cloud_request_write(input, &reason, request_id).await
    } else {
        client.cloud_read(input).await
    }
}

fn cloud_exit(result: &CloudResult) -> ExitCode {
    match result.outcome() {
        cloudthinker_client::CloudOutcome::Success => ExitCode::Ok,
        cloudthinker_client::CloudOutcome::Failed => ExitCode::JobFailed,
        cloudthinker_client::CloudOutcome::Approval => ExitCode::ApprovalRequired,
    }
}
