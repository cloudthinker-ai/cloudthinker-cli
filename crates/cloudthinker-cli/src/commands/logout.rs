//! `cloudthinker logout` — best-effort revoke + clear local credentials.

use cloudthinker_client::{CtClient, CtResult, persistent_store};

use crate::engine::exit::{self, ExitCode};
use crate::engine::output;

pub async fn run(base_url: &str, workspace: Option<&str>, all: bool) -> ExitCode {
    let store = match persistent_store(base_url, workspace) {
        Ok(store) => store,
        Err(err) => return exit::report(&err),
    };
    let client = match CtClient::new(base_url, store) {
        Ok(client) => client,
        Err(err) => return exit::report(&err),
    };

    let result = if all {
        client.logout_all().await
    } else {
        client.logout().await
    };
    finish_logout(result, all)
}

/// Report the logout outcome.
///
/// Extracted from `run` so the best-effort invariant is unit-testable without
/// a network round-trip: by the time `client.logout()` returns, the local
/// store has already been cleared (see `CtClient::logout`), so a failed
/// server-side revoke must never surface as a failed `cloudthinker logout`.
fn finish_logout(result: CtResult<()>, all: bool) -> ExitCode {
    if let Err(error) = result {
        return exit::report(&error);
    }
    output::progress(if all {
        "Logged out of all workspaces for this host."
    } else {
        "Logged out."
    });
    ExitCode::Ok
}

#[cfg(test)]
mod tests {
    use cloudthinker_client::CtError;

    use super::*;

    #[test]
    fn finish_logout_reports_ok_when_revoke_succeeds() {
        assert_eq!(finish_logout(Ok(()), false), ExitCode::Ok);
    }

    #[test]
    fn finish_logout_reports_local_failure() {
        let result = Err(CtError::Transport("network down".into()));

        assert_eq!(finish_logout(result, false), ExitCode::JobFailed);
    }
}
