//! `cloudthinker agent` — install and exec the local coding agent.

use std::ffi::OsString;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};

use cloudthinker_client::{
    CliIdentity, CtError, DEFAULT_RELEASE_BASE_URL, agent_bin_root, host_target_triple,
    install_agent, installed_agent_binary,
};
use uuid::Uuid;

use crate::commands::{WORKSPACE_ENV_VAR, build_client, env_token_is_set};
use crate::engine::exit::{self, ExitCode};
use crate::engine::{login_guide, output};

/// The version of this binary; the agent bundle is released under the same tag.
const AGENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Dev override: exec this path instead of an installed release.
const AGENT_BIN_ENV_VAR: &str = "CLOUDTHINKER_AGENT_BIN";

/// Handed to the child so its `cloudthinker auth token` resolves the same host.
const URL_ENV_VAR: &str = "CLOUDTHINKER_URL";

pub async fn run(base_url: &str, workspace: Option<&str>, args: Vec<OsString>) -> ExitCode {
    crate::commands::update::offer_on_start().await;
    let identity = match resolve_identity(base_url, workspace).await {
        Ok(identity) => identity,
        Err(code) => return code,
    };
    let binary = match resolve_binary().await {
        Ok(binary) => binary,
        Err(err) => return exit::report(&err),
    };
    exec_agent(
        &binary,
        args,
        base_url,
        child_workspace_env(env_token_is_set(), identity.workspace_id),
    )
}

/// Prove the login before anything else, and offer the login itself when there
/// is none. The agent is a long interactive session that spends the workspace's
/// credits; sending the user away to another command to come back is the wrong
/// first minute.
async fn resolve_identity(
    base_url: &str,
    workspace: Option<&str>,
) -> Result<CliIdentity, ExitCode> {
    let error = match whoami(base_url, workspace).await {
        Ok(identity) => return Ok(identity),
        Err(error) => error,
    };
    match login_guide::plan(&error, env_token_is_set(), std::io::stdin().is_terminal()) {
        login_guide::Plan::Report => Err(exit::report(&error)),
        login_guide::Plan::Tell(hint) => {
            output::eprintln_error(&error.to_string());
            output::progress(hint);
            Err(ExitCode::Auth)
        }
        login_guide::Plan::LogIn => {
            output::progress(login_guide::OPENING_LOGIN);
            match crate::commands::login::run(base_url, false, false).await {
                ExitCode::Ok => {}
                code => return Err(code),
            }
            whoami(base_url, workspace)
                .await
                .map_err(|error| exit::report(&error))
        }
    }
}

async fn whoami(base_url: &str, workspace: Option<&str>) -> Result<CliIdentity, CtError> {
    build_client(base_url, workspace)?.whoami().await
}

/// The overridden path, the already-installed bundle, or a fresh install.
async fn resolve_binary() -> cloudthinker_client::CtResult<PathBuf> {
    if let Some(path) = std::env::var_os(AGENT_BIN_ENV_VAR) {
        output::warn(&format!(
            "agent: environment variable {AGENT_BIN_ENV_VAR} is set, so the local agent runs from that path instead of the installed release"
        ));
        return Ok(PathBuf::from(path));
    }
    let bin_root = agent_bin_root()?;
    if let Some(binary) = installed_agent_binary(&bin_root, AGENT_VERSION) {
        return Ok(binary);
    }
    let triple = host_target_triple()?;
    output::progress(&format!(
        "downloading cloudthinker-agent {AGENT_VERSION} for {triple}"
    ));
    let installed =
        install_agent(DEFAULT_RELEASE_BASE_URL, AGENT_VERSION, triple, &bin_root).await?;
    for failure in &installed.prune_failures {
        output::warn(&format!("agent: {failure}"));
    }
    Ok(installed.binary)
}

/// A child running on the parent's `CLOUDTHINKER_TOKEN` inherits it and must
/// get no workspace variable, because the two together are a usage error.
fn child_workspace_env(token_env_is_set: bool, workspace_id: Uuid) -> Option<String> {
    (!token_env_is_set).then(|| workspace_id.to_string())
}

#[cfg(unix)]
fn exec_agent(
    binary: &Path,
    args: Vec<OsString>,
    base_url: &str,
    workspace_id: Option<String>,
) -> ExitCode {
    use std::os::unix::process::CommandExt;

    let mut command = std::process::Command::new(binary);
    command.args(args).env(URL_ENV_VAR, base_url);
    match workspace_id {
        Some(workspace_id) => command.env(WORKSPACE_ENV_VAR, workspace_id),
        None => command.env_remove(WORKSPACE_ENV_VAR),
    };
    let error = command.exec();
    output::eprintln_error(&format!("could not run {}: {error}", binary.display()));
    ExitCode::JobFailed
}

#[cfg(not(unix))]
fn exec_agent(
    _binary: &Path,
    _args: Vec<OsString>,
    _base_url: &str,
    _workspace_id: Option<String>,
) -> ExitCode {
    output::eprintln_error("cloudthinker agent is not available on this platform yet");
    ExitCode::JobFailed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stored_credentials_pin_the_child_to_the_resolved_workspace() {
        let workspace_id = Uuid::from_u128(1);

        assert_eq!(
            child_workspace_env(false, workspace_id),
            Some("00000000-0000-0000-0000-000000000001".to_string())
        );
    }

    #[test]
    fn an_environment_token_passes_through_without_a_workspace_variable() {
        assert_eq!(child_workspace_env(true, Uuid::from_u128(1)), None);
    }
}
