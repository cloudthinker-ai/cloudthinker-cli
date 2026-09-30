//! `cloudthinker agent` — install and exec the local coding agent.

use std::ffi::OsString;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use crate::commands::{WORKSPACE_ENV_VAR, build_client, env_token_is_set};
use crate::engine::exit::{self, ExitCode};
use crate::engine::{login_guide, output};
use cloudthinker_client::{
    CliIdentity, CtError, DEFAULT_RELEASE_BASE_URL, agent_bin_root, any_agent_installed,
    host_target_triple, install_agent, installed_agent_binary,
};

/// The version of this binary; the agent bundle is released under the same tag.
const AGENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Dev override: exec this path instead of an installed release.
const AGENT_BIN_ENV_VAR: &str = "CLOUDTHINKER_AGENT_BIN";

/// Handed to the child so its `cloudthinker auth token` resolves the same host.
const URL_ENV_VAR: &str = "CLOUDTHINKER_URL";

const BIN_ENV_VAR: &str = "CLOUDTHINKER_BIN";

pub async fn run(base_url: &str, workspace: Option<&str>, args: Vec<OsString>) -> ExitCode {
    let mut timing = crate::engine::timing::PhaseTimer::from_env();
    timing.mark("wrapper.dispatch");
    let workspace_id = if asks_for_help(&args) {
        None
    } else {
        let pending_check = crate::commands::update::offer_on_start(base_url).await;
        timing.mark("wrapper.update_check");
        let identity = resolve_identity(base_url, workspace).await;
        pending_check.settle().await;
        match identity {
            Ok(identity) => Some(identity.workspace_id.to_string()),
            Err(code) => return code,
        }
    };
    timing.mark("wrapper.identity");
    let binary = match resolve_binary().await {
        Ok(binary) => binary,
        Err(err) => return exit::report(&err),
    };
    timing.mark("wrapper.install_check");
    exec_agent(&binary, args, base_url, workspace_id)
}

fn asks_for_help(args: &[OsString]) -> bool {
    args.iter().any(|arg| arg == "--help" || arg == "-h")
}

/// Prove the login before anything else, and offer the login itself when there
/// is none. The agent is a long interactive session that spends the workspace's
/// credits; sending the user away to another command to come back is the wrong
/// first minute.
async fn resolve_identity(
    base_url: &str,
    workspace: Option<&str>,
) -> Result<CliIdentity, ExitCode> {
    let client = build_client(base_url, workspace).map_err(|error| exit::report(&error))?;
    let error = match client.whoami().await {
        Ok(identity) => return Ok(identity),
        Err(error) => error,
    };
    let provenance = client
        .credential_provenance()
        .map_err(|error| exit::report(&error))?;
    let plan = login_guide::plan(&error, provenance, std::io::stdin().is_terminal());
    if let Some((line, next)) = login_guide::explain(&error, plan, base_url) {
        output::eprintln_error(&line);
        output::progress(&next);
    }
    match plan {
        login_guide::Plan::Report => Err(exit::report(&error)),
        login_guide::Plan::Tell(_) => Err(ExitCode::Auth),
        login_guide::Plan::LogIn => {
            match crate::commands::login::run(
                base_url,
                crate::commands::login::LoginOptions::default(),
            )
            .await
            {
                ExitCode::Ok => {}
                code => return Err(code),
            }
            whoami(base_url, workspace)
                .await
                .map_err(|error| exit::report(&error))
        }
    }
}

pub(super) async fn run_local_review(
    base_url: &str,
    workspace: Option<&str>,
    repository: &Path,
    prompt: &str,
    timeout: std::time::Duration,
) -> Result<String, ExitCode> {
    let identity = resolve_identity(base_url, workspace).await?;
    let binary = resolve_binary()
        .await
        .map_err(|error| exit::report(&error))?;
    let session_dir = tempfile::tempdir().map_err(|error| {
        output::eprintln_error(&format!(
            "could not create a temporary local agent session: {error}"
        ));
        ExitCode::JobFailed
    })?;
    let mut command = tokio::process::Command::new(&binary);
    command
        .current_dir(repository)
        .args([
            "--cloudthinker-local-review",
            "--models",
            "cloudthinker/*",
            "--tools",
            "read,grep,find,ls",
            "--no-extensions",
            "--no-skills",
            "--no-context-files",
            "--session-dir",
        ])
        .arg(session_dir.path())
        .args(["--print", "-p", prompt])
        .env(URL_ENV_VAR, base_url)
        .env_remove(WORKSPACE_ENV_VAR)
        .env_remove(BIN_ENV_VAR)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if !env_token_is_set() {
        command.env(WORKSPACE_ENV_VAR, identity.workspace_id.to_string());
    }
    if let Some(own_binary) = own_binary() {
        command.env(BIN_ENV_VAR, own_binary);
    }
    let child = command.spawn().map_err(|error| {
        output::eprintln_error(&format!(
            "could not run local CloudThinker agent {}: {error}",
            binary.display()
        ));
        ExitCode::JobFailed
    })?;
    let result = tokio::time::timeout(timeout, child.wait_with_output()).await;
    let output = match result {
        Ok(Ok(output)) if output.status.success() => output,
        Ok(Ok(output)) => {
            let detail = String::from_utf8_lossy(&output.stderr);
            output::eprintln_error(&format!(
                "local CloudThinker agent failed: {}",
                detail.trim()
            ));
            return Err(ExitCode::JobFailed);
        }
        Ok(Err(error)) => {
            output::eprintln_error(&format!(
                "could not wait for local CloudThinker agent: {error}"
            ));
            return Err(ExitCode::JobFailed);
        }
        Err(_) => {
            output::eprintln_error(
                "timed out waiting for the local CloudThinker agent; rerun the review to try again",
            );
            return Err(ExitCode::Timeout);
        }
    };
    String::from_utf8(output.stdout).map_err(|error| {
        output::eprintln_error(&format!(
            "local CloudThinker agent returned non-UTF-8 output: {error}"
        ));
        ExitCode::JobFailed
    })
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
    let step = output::step(&format!("Setting up cloudthinker {AGENT_VERSION}"));
    let installed =
        install_agent(DEFAULT_RELEASE_BASE_URL, AGENT_VERSION, triple, &bin_root).await?;
    drop(step);
    for failure in &installed.prune_failures {
        output::warn(&format!("agent: {failure}"));
    }
    Ok(installed.binary)
}

pub async fn prefetch_bundle(version: &str) -> cloudthinker_client::CtResult<()> {
    if std::env::var_os(AGENT_BIN_ENV_VAR).is_some() {
        return Ok(());
    }
    let bin_root = agent_bin_root()?;
    if installed_agent_binary(&bin_root, version).is_some() {
        return Ok(());
    }
    install_agent(
        DEFAULT_RELEASE_BASE_URL,
        version,
        host_target_triple()?,
        &bin_root,
    )
    .await
    .map(|_| ())
}

pub fn bundle_in_use() -> bool {
    std::env::var_os(AGENT_BIN_ENV_VAR).is_none()
        && agent_bin_root().is_ok_and(|bin_root| any_agent_installed(&bin_root))
}

fn own_binary() -> Option<PathBuf> {
    std::env::current_exe().ok()
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
    command
        .args(args)
        .env(URL_ENV_VAR, base_url)
        .env_remove("CLOUDTHINKER_TOKEN");
    match own_binary() {
        Some(own_binary) => command.env(BIN_ENV_VAR, own_binary),
        None => command.env_remove(BIN_ENV_VAR),
    };
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
    fn a_help_request_skips_the_login() {
        assert!(asks_for_help(&["--help".into()]));
        assert!(asks_for_help(&["--tui-mode".into(), "-h".into()]));
        assert!(!asks_for_help(&["--tui-mode".into(), "fullscreen".into()]));
        assert!(!asks_for_help(&[]));
    }
}
