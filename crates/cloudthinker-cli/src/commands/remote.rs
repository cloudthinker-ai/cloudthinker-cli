use clap::{Args, Subcommand};

use crate::engine::exit::ExitCode;

#[derive(Debug, Args)]
#[command(
    args_conflicts_with_subcommands = true,
    after_help = "Name the machine the first time; after that, `cloudthinker remote` alone returns to it. The folder is picked for you: the same git repository or folder on the remote machine, its one running agent, or a short list to choose from. The agent keeps running when the connection drops or the laptop sleeps. Ctrl+Z detaches and leaves it running. Ctrl+V pastes a laptop clipboard image, and a file dragged onto the terminal is copied over.\n\nExamples:\n  cloudthinker remote workbox\n  cloudthinker remote\n  cloudthinker remote me@10.0.0.5:2222\n  cloudthinker remote workbox --detach --cwd ~/app -- --model fast\n  cloudthinker remote workbox --via 'ssh -J jump.example.com me@10.0.0.5'\n  cloudthinker remote ls workbox --json\n  cloudthinker remote forget workbox"
)]
pub struct RemoteArgs {
    #[command(subcommand)]
    pub command: Option<RemoteCommand>,
    /// SSH destination: a host from ~/.ssh/config, user@host, user@host:port, or ssh://user@host:port; defaults to the last machine you used.
    pub target: Option<String>,
    /// Attach to or start the agent with this name instead of picking one by folder.
    #[arg(long)]
    pub name: Option<String>,
    /// Folder on the remote machine to use instead of picking one.
    #[arg(long, value_name = "DIR")]
    pub cwd: Option<String>,
    /// Start the agent and leave it running without attaching, then print how to return to it; works without a terminal.
    #[arg(long)]
    pub detach: bool,
    /// Command that reaches the machine instead of ssh, such as 'ssh -J jump me@vm'; the remote command is added at the end. It is saved with the machine.
    #[arg(long, value_name = "COMMAND")]
    pub via: Option<String>,
    /// Arguments for the agent, after `--`; used only when the agent starts.
    #[arg(last = true, value_name = "AGENT_ARGS")]
    pub agent_args: Vec<String>,
}

#[derive(Debug, Subcommand)]
pub enum RemoteCommand {
    /// List the saved machines, or the agents running on one machine.
    Ls(RemoteLsArgs),
    /// Remove a machine from the saved machines.
    Forget(RemoteForgetArgs),
}

#[derive(Debug, Args)]
pub struct RemoteLsArgs {
    /// Machine whose running agents to list; without it, lists the saved machines.
    pub target: Option<String>,
    /// Command that reaches the machine instead of ssh; defaults to the one saved with the machine.
    #[arg(long, value_name = "COMMAND", requires = "target")]
    pub via: Option<String>,
    /// Print JSON on stdout instead of text.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct RemoteForgetArgs {
    /// Saved machine to remove.
    pub target: String,
}

#[derive(Debug, Args)]
pub struct RemoteSideArgs {
    /// Name of the agent on this machine.
    #[arg(long, default_value = "default")]
    pub name: String,
}

#[cfg(unix)]
pub async fn run(args: RemoteArgs) -> ExitCode {
    match args.command {
        Some(RemoteCommand::Ls(ls)) => listing::ls(ls).await,
        Some(RemoteCommand::Forget(forget)) => listing::forget(&forget.target),
        None => {
            crate::engine::remote::client::run(crate::engine::remote::client::ClientOptions {
                target: args.target,
                name: args.name,
                cwd: args.cwd,
                via: args.via,
                detach: args.detach,
                agent_args: args.agent_args,
            })
            .await
        }
    }
}

#[cfg(unix)]
mod listing {
    use std::time::{SystemTime, UNIX_EPOCH};

    use serde::Serialize;

    use super::RemoteLsArgs;
    use crate::engine::exit::{ExitCode, finish_output, report};
    use crate::engine::output::{emit_json, print_lines, terminal_text};
    use crate::engine::remote::client::{Machine, running};
    use crate::engine::remote::picker::{display_folder, uptime};
    use crate::engine::remote::targets::{self, Saved};

    #[derive(Serialize)]
    struct SavedMachines<'a> {
        machines: Vec<SavedMachine<'a>>,
    }

    #[derive(Serialize)]
    struct SavedMachine<'a> {
        target: &'a str,
        via: Option<&'a str>,
        default: bool,
    }

    #[derive(Serialize)]
    struct RunningAgents<'a> {
        target: &'a str,
        agents: Vec<Agent<'a>>,
    }

    #[derive(Serialize)]
    struct Agent<'a> {
        name: &'a str,
        cwd: &'a str,
        started_at: u64,
        title: &'a str,
        origin: Option<&'a str>,
    }

    pub async fn ls(args: RemoteLsArgs) -> ExitCode {
        match args.target {
            Some(target) => match running(Some(target), args.via).await {
                Ok(machine) => finish_output(show_agents(&machine, args.json)),
                Err(code) => code,
            },
            None => match targets::list() {
                Ok(saved) => finish_output(show_saved(&saved, args.json)),
                Err(error) => report(&error),
            },
        }
    }

    pub fn forget(target: &str) -> ExitCode {
        match targets::forget(target) {
            Ok(true) => finish_output(print_lines(&[format!("Forgot {target}")])),
            Ok(false) => report(&cloudthinker_client::CtError::Usage(format!(
                "{target} is not a saved machine; cloudthinker remote ls lists them"
            ))),
            Err(error) => report(&error),
        }
    }

    fn show_saved(saved: &[Saved], json: bool) -> Result<(), String> {
        if json {
            return emit_json(&SavedMachines {
                machines: saved
                    .iter()
                    .enumerate()
                    .map(|(index, saved)| SavedMachine {
                        target: &saved.target,
                        via: saved.via.as_deref(),
                        default: index == 0,
                    })
                    .collect(),
            });
        }
        if saved.is_empty() {
            return print_lines(&[
                "No saved machines; cloudthinker remote <ssh-host> saves one".to_string(),
            ]);
        }
        let lines: Vec<String> = saved
            .iter()
            .enumerate()
            .map(|(index, saved)| {
                let mark = if index == 0 { "  (default)" } else { "" };
                match &saved.via {
                    Some(via) => format!("{}{mark}  via {via}", terminal_text(&saved.target)),
                    None => format!("{}{mark}", terminal_text(&saved.target)),
                }
            })
            .collect();
        print_lines(&lines)
    }

    fn show_agents(machine: &Machine, json: bool) -> Result<(), String> {
        if json {
            return emit_json(&RunningAgents {
                target: &machine.target,
                agents: machine
                    .running
                    .iter()
                    .map(|agent| Agent {
                        name: &agent.name,
                        cwd: &agent.cwd,
                        started_at: agent.started_at,
                        title: &agent.title,
                        origin: agent.origin.as_deref(),
                    })
                    .collect(),
            });
        }
        if machine.running.is_empty() {
            return print_lines(&[format!("No agents run on {}", machine.target)]);
        }
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| since.as_secs());
        let lines: Vec<String> = machine
            .running
            .iter()
            .map(|agent| {
                let line = format!(
                    "{}  {}  {}",
                    terminal_text(&agent.name),
                    display_folder(&agent.cwd, &machine.home),
                    uptime(now, agent.started_at)
                );
                if agent.title.is_empty() {
                    line
                } else {
                    format!("{line}  {}", terminal_text(&agent.title))
                }
            })
            .collect();
        print_lines(&lines)
    }
}

#[cfg(not(unix))]
pub async fn run(_args: RemoteArgs) -> ExitCode {
    crate::engine::output::eprintln_error("cloudthinker remote is not supported on Windows yet");
    ExitCode::Usage
}

#[cfg(unix)]
pub async fn bridge(args: RemoteSideArgs) -> ExitCode {
    crate::engine::remote::bridge::run(&args.name).await
}

#[cfg(unix)]
pub async fn host(args: RemoteSideArgs) -> ExitCode {
    crate::engine::remote::host::run(&args.name).await
}

#[cfg(not(unix))]
pub async fn bridge(_args: RemoteSideArgs) -> ExitCode {
    crate::engine::output::eprintln_error("the remote agent host needs Linux or macOS");
    ExitCode::Usage
}

#[cfg(not(unix))]
pub async fn host(_args: RemoteSideArgs) -> ExitCode {
    crate::engine::output::eprintln_error("the remote agent host needs Linux or macOS");
    ExitCode::Usage
}
