//! `cloudthinker logout` — best-effort revoke + clear local credentials.

use cloudthinker_client::{CtClient, persistent_store};

use crate::engine::exit::{self, ExitCode};
use crate::engine::output;

pub async fn run(base_url: &str) -> ExitCode {
    let store = match persistent_store(base_url) {
        Ok(store) => store,
        Err(err) => return exit::report(&err),
    };
    let client = match CtClient::new(base_url, store) {
        Ok(client) => client,
        Err(err) => return exit::report(&err),
    };

    // Best-effort: a revoke failure still clears the local store and exits 0.
    let _ = client.logout().await;
    output::progress("Logged out.");
    ExitCode::Ok
}
