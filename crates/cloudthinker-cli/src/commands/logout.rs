//! `cloudthinker logout` — best-effort revoke + clear local credentials.

use cloudthinker_client::{CtClient, CtResult, LogoutOutcome, persistent_store};

use crate::commands::env_token_is_set;
use crate::engine::exit::{self, ExitCode};
use crate::engine::output;

pub async fn run(base_url: &str, workspace: Option<&str>, all: bool) -> ExitCode {
    let store = match persistent_store(base_url, workspace) {
        Ok(store) => store,
        Err(err) => return exit::report(&err),
    };
    if !all && workspace.is_none() {
        let workspaces = store.workspaces().unwrap_or_default();
        if !workspaces.is_empty() && !workspaces.iter().any(|stored| stored.active) {
            output::progress(&format!(
                "No active workspace to log out. Stored workspaces: {}. Run `cloudthinker logout --workspace <id|name>` or `cloudthinker logout --all`.",
                workspaces
                    .iter()
                    .map(output::workspace_label)
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
            return ExitCode::Ok;
        }
    }
    let client = match CtClient::new(base_url, store.clone()) {
        Ok(client) => client,
        Err(err) => return exit::report(&err),
    };

    let result = if all {
        client.logout_all().await
    } else {
        client.logout().await
    };
    let code = finish_logout(result, all);
    if store.workspaces().is_ok_and(|stored| stored.is_empty()) {
        crate::commands::login::forget_url(base_url);
    }
    if env_token_is_set() {
        output::warn(
            "CLOUDTHINKER_TOKEN is still set, so commands keep using that credential. Unset it to finish logging out.",
        );
    }
    code
}

/// Report the logout outcome.
///
/// Extracted from `run` so the best-effort invariant is unit-testable without
/// a network round-trip: by the time `client.logout()` returns, the local
/// store has already been cleared (see `CtClient::logout`), so a failed
/// server-side revoke must never surface as a failed `cloudthinker logout`.
fn finish_logout(result: CtResult<LogoutOutcome>, all: bool) -> ExitCode {
    let outcome = match result {
        Ok(outcome) => outcome,
        Err(error) => return exit::report(&error),
    };
    output::progress(match (outcome, all) {
        (LogoutOutcome::NothingStored, _) => "No stored login for this host; nothing to log out.",
        (LogoutOutcome::Cleared, true) => "Logged out of all workspaces for this host.",
        (LogoutOutcome::Cleared, false) => "Logged out.",
    });
    ExitCode::Ok
}

#[cfg(test)]
mod tests {
    use cloudthinker_client::CtError;

    use super::*;

    #[test]
    fn finish_logout_reports_ok_when_revoke_succeeds() {
        assert_eq!(
            finish_logout(Ok(LogoutOutcome::Cleared), false),
            ExitCode::Ok
        );
        assert_eq!(
            finish_logout(Ok(LogoutOutcome::NothingStored), false),
            ExitCode::Ok
        );
    }

    #[test]
    fn finish_logout_reports_local_failure() {
        let result = Err(CtError::Transport("network down".into()));

        assert_eq!(finish_logout(result, false), ExitCode::JobFailed);
    }
}
