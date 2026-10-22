use clap::Args;

use crate::engine::exit::ExitCode;

#[derive(Debug, Args)]
#[command(
    after_help = "Name the machine the first time; after that, `cloudthinker remote` alone returns to it. The folder is picked for you: the same git repository or folder on the remote machine, its one running agent, or a short list to choose from. The agent keeps running when the connection drops or the laptop sleeps. Ctrl+Z detaches and leaves it running. Ctrl+V pastes a laptop clipboard image, and a file dragged onto the terminal is copied over.\n\nExamples:\n  cloudthinker remote workbox\n  cloudthinker remote"
)]
pub struct RemoteArgs {
    /// SSH destination: a host from ~/.ssh/config, user@host, or ssh://user@host:port; defaults to the last machine you used.
    pub target: Option<String>,
    /// Attach to or start the agent with this name instead of picking one by folder.
    #[arg(long)]
    pub name: Option<String>,
    /// Folder on the remote machine to use instead of picking one.
    #[arg(long, value_name = "DIR")]
    pub cwd: Option<String>,
    /// Arguments for the agent, after `--`; used only when the agent starts.
    #[arg(last = true, value_name = "AGENT_ARGS")]
    pub agent_args: Vec<String>,
}

#[derive(Debug, Args)]
pub struct RemoteSideArgs {
    /// Name of the agent on this machine.
    #[arg(long, default_value = "default")]
    pub name: String,
}

#[cfg(unix)]
pub async fn run(args: RemoteArgs) -> ExitCode {
    crate::engine::remote::client::run(crate::engine::remote::client::ClientOptions {
        target: args.target,
        name: args.name,
        cwd: args.cwd,
        agent_args: args.agent_args,
    })
    .await
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
