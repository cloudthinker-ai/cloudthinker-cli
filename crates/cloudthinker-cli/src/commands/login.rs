//! `cloudthinker login` — browser PKCE login over a loopback callback.

use std::time::Duration;

use cloudthinker_client::{
    CtClient, Loopback, PkceChallenge, SaveLocation, consent_url, persistent_store,
};

use crate::engine::exit::{self, ExitCode};
use crate::engine::output;

/// Consent code TTL — mirrors the server's one-time-code lifetime.
const LOGIN_WAIT: Duration = Duration::from_secs(300);

pub async fn run(base_url: &str, no_browser: bool) -> ExitCode {
    let store = match persistent_store(base_url) {
        Ok(store) => store,
        Err(err) => return exit::report(&err),
    };
    let client = match CtClient::new(base_url, store.clone()) {
        Ok(client) => client,
        Err(err) => return exit::report(&err),
    };

    let pkce = PkceChallenge::generate();
    let loopback = match Loopback::bind(pkce.state.clone()) {
        Ok(loopback) => loopback,
        Err(err) => return exit::report(&err),
    };
    let url = match consent_url(base_url, &pkce.challenge, loopback.port(), &pkce.state) {
        Ok(url) => url,
        Err(err) => return exit::report(&err),
    };

    // Always print the URL so a headless environment (or a failed browser open)
    // still has a path forward.
    output::progress(&format!(
        "Open this URL to authorize CloudThinker:\n  {url}"
    ));
    if !no_browser {
        let _ = open::that(&url);
    }

    let code = match loopback.wait_for_code(LOGIN_WAIT).await {
        Ok(code) => code,
        Err(err) => return exit::report(&err),
    };

    let token = match client.exchange_code(&code, &pkce.verifier).await {
        Ok(token) => token,
        Err(err) => return exit::report(&err),
    };

    match store.save(&token) {
        Ok(SaveLocation::File) => {
            output::warn("OS keyring unavailable; stored credentials in a 0600 file.");
        }
        Ok(SaveLocation::Keyring) => {}
        Err(err) => return exit::report(&err),
    }

    match token.workspace_id {
        Some(workspace) => output::progress(&format!("Logged in (workspace {workspace}).")),
        None => output::progress("Logged in."),
    }
    ExitCode::Ok
}
