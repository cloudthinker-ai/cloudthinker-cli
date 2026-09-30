//! `cloudthinker auth token|status|switch` output.

use cloudthinker_client::{CtError, WorkspaceSelector, login_command, persistent_store};

use crate::commands::{build_client, env_token_is_set};
use crate::engine::exit::{self, ExitCode};
use crate::engine::output;

pub async fn run_token(base_url: &str, workspace: Option<&str>) -> ExitCode {
    let client = match build_client(base_url, workspace) {
        Ok(client) => client,
        Err(err) => return exit::report(&err),
    };
    let _deferred = defer_termination();
    let token = match client.access_token().await {
        Ok(token) => token,
        Err(err) => return exit::report(&err),
    };
    match output::print_access_token(&token) {
        Ok(()) => ExitCode::Ok,
        Err(err) => exit::report(&CtError::Transport(err)),
    }
}

pub fn run_status(base_url: &str, json: bool) -> ExitCode {
    let workspaces = match persistent_store(base_url, None).and_then(|store| store.workspaces()) {
        Ok(workspaces) => workspaces,
        Err(err) => return exit::report(&err),
    };
    let status = output::AuthStatus {
        host: base_url.trim_end_matches('/').to_string(),
        environment_token: env_token_is_set(),
        workspaces,
    };
    match output::emit_auth_status(&status, &login_command(base_url), json) {
        Ok(()) => ExitCode::Ok,
        Err(err) => exit::report(&CtError::Transport(err)),
    }
}

pub fn run_switch(base_url: &str, workspace: &str) -> ExitCode {
    let selector = WorkspaceSelector::IdOrName(workspace.to_string());
    let activated =
        match persistent_store(base_url, None).and_then(|store| store.activate(&selector)) {
            Ok(activated) => activated,
            Err(err) => return exit::report(&err),
        };
    output::progress(&format!(
        "Switched to {}.",
        output::workspace_label(&activated)
    ));
    if env_token_is_set() {
        output::warn("CLOUDTHINKER_TOKEN is set, so commands keep using it until you unset it.");
    }
    ExitCode::Ok
}

#[cfg(unix)]
fn defer_termination() -> Vec<tokio::signal::unix::Signal> {
    use tokio::signal::unix::{SignalKind, signal};

    [
        SignalKind::interrupt(),
        SignalKind::terminate(),
        SignalKind::hangup(),
    ]
    .into_iter()
    .filter_map(|kind| signal(kind).ok())
    .collect()
}

#[cfg(not(unix))]
fn defer_termination() {}
