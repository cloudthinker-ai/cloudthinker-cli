//! `cloudthinker login` — browser PKCE login over a loopback callback.

use std::future::Future;
use std::time::Duration;

use cloudthinker_client::{
    CtClient, Loopback, PkceChallenge, SaveLocation, StoredToken, TokenStore, consent_url,
    persistent_store, wait_for_device_token,
};

use crate::engine::exit::{self, ExitCode};
use crate::engine::output;

/// Consent code TTL — mirrors the server's one-time-code lifetime.
const LOGIN_WAIT: Duration = Duration::from_secs(300);

pub async fn run(base_url: &str, no_browser: bool, device_auth: bool) -> ExitCode {
    let store = match persistent_store(base_url, None) {
        Ok(store) => store,
        Err(err) => return exit::report(&err),
    };
    let client = match CtClient::new(base_url, store.clone()) {
        Ok(client) => client,
        Err(err) => return exit::report(&err),
    };

    if device_auth {
        return run_device_login(&client, store.as_ref(), no_browser).await;
    }

    let pkce = PkceChallenge::generate();
    let loopback = match loopback_or_device_login(Loopback::bind(pkce.state.clone()), || {
        run_device_login(&client, store.as_ref(), no_browser)
    })
    .await
    {
        Ok(loopback) => loopback,
        Err(code) => return code,
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
        Err(err) => return exit::report(&callback_wait_error(err)),
    };

    let token = match client.exchange_code(&code, &pkce.verifier).await {
        Ok(token) => token,
        Err(err) => return exit::report(&err),
    };

    finish_login(store.as_ref(), &token)
}

async fn run_device_login(client: &CtClient, store: &dyn TokenStore, no_browser: bool) -> ExitCode {
    let authorization = match client.start_device_authorization().await {
        Ok(authorization) => authorization,
        Err(err) => return exit::report(&err),
    };

    output::progress(&format!(
        "Open this URL to authorize CloudThinker:\n  {}\n\nEnter this code:\n  {}",
        authorization.verification_uri, authorization.user_code
    ));
    if !no_browser {
        let _ = open::that(&authorization.verification_uri);
    }

    let token = match wait_for_device_token(client, &authorization).await {
        Ok(token) => token,
        Err(err) => return exit::report(&err),
    };
    finish_login(store, &token)
}

async fn loopback_or_device_login<F, Fut>(
    loopback: cloudthinker_client::CtResult<Loopback>,
    device_login: F,
) -> Result<Loopback, ExitCode>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = ExitCode>,
{
    match loopback {
        Ok(loopback) => Ok(loopback),
        Err(err) => {
            output::warn(&format!(
                "Loopback callback unavailable ({err}); switching to device-code login."
            ));
            Err(device_login().await)
        }
    }
}

fn callback_wait_error(error: cloudthinker_client::CtError) -> cloudthinker_client::CtError {
    match error {
        cloudthinker_client::CtError::Timeout(_) => cloudthinker_client::CtError::Timeout(
            "timed out waiting for browser consent; try `cloudthinker login --device-auth`".into(),
        ),
        error => error,
    }
}

/// Persist the freshly exchanged token and report the outcome.
///
/// Extracted from `run` so the save -> report ORDERING invariant (a failing
/// `store.save` must never report a successful login) is unit-testable
/// without a browser/loopback/network round-trip.
fn finish_login(store: &dyn TokenStore, token: &StoredToken) -> ExitCode {
    match store.save(token) {
        Ok(SaveLocation::File) => {
            output::warn("OS keyring unavailable; stored credentials in a 0600 file.");
        }
        Ok(SaveLocation::Keyring) => {}
        Err(err) => return exit::report(&err),
    }

    match (&token.workspace_name, token.workspace_id) {
        (Some(workspace), _) => output::progress(&format!(
            "Logged in to {}.",
            output::terminal_text(workspace)
        )),
        (None, Some(workspace)) => output::progress(&format!("Logged in to {workspace}.")),
        (None, None) => output::progress("Logged in."),
    }
    ExitCode::Ok
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use cloudthinker_client::{CtError, CtResult};

    use super::*;

    /// Records every save call so tests can assert the save actually happened
    /// (not just the exit code). `save_fails` simulates an unusable keyring
    /// AND file fallback (e.g. disk full, no writable config dir).
    #[derive(Default)]
    struct FakeStore {
        saved: Mutex<Vec<StoredToken>>,
        save_fails: bool,
    }

    impl FakeStore {
        fn failing() -> Self {
            Self {
                save_fails: true,
                ..Self::default()
            }
        }
    }

    impl TokenStore for FakeStore {
        fn load(&self) -> CtResult<Option<StoredToken>> {
            Ok(None)
        }

        fn save(&self, token: &StoredToken) -> CtResult<SaveLocation> {
            self.saved.lock().expect("mock lock").push(token.clone());
            if self.save_fails {
                return Err(CtError::Store("disk full".into()));
            }
            Ok(SaveLocation::Keyring)
        }

        fn clear(&self) -> CtResult<()> {
            Ok(())
        }

        fn refresh_enabled(&self) -> bool {
            true
        }
    }

    fn a_token() -> StoredToken {
        StoredToken {
            access_token: "access-token".into(),
            refresh_token: Some("refresh-token".into()),
            expires_at: None,
            workspace_id: None,
            workspace_name: None,
        }
    }

    #[test]
    fn finish_login_saves_then_reports_ok_when_store_succeeds() {
        let store = FakeStore::default();
        let code = finish_login(&store, &a_token());

        assert_eq!(code, ExitCode::Ok);
        assert_eq!(store.saved.lock().expect("mock lock").len(), 1);
    }

    // I7: the highest-value ordering invariant — a failing `store.save` must
    // NEVER report login success. A step-ordering bug (reporting success
    // before `save` returns `Ok`) would pass every other existing test.
    #[test]
    fn finish_login_never_reports_success_when_store_save_fails() {
        let code = finish_login(&FakeStore::failing(), &a_token());

        assert_ne!(
            code,
            ExitCode::Ok,
            "a failed store.save must not report login success"
        );
    }

    #[tokio::test]
    async fn bind_failure_starts_device_login_exactly_once() {
        let calls = Arc::new(AtomicUsize::new(0));
        let result = loopback_or_device_login(Err(CtError::Login("bind failed".into())), || {
            let calls = Arc::clone(&calls);
            async move {
                calls.fetch_add(1, Ordering::SeqCst);
                ExitCode::Ok
            }
        })
        .await;

        assert!(matches!(result, Err(ExitCode::Ok)));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn callback_timeout_preserves_browser_mode_and_prints_device_recovery() {
        let error = callback_wait_error(CtError::Timeout("old message".into()));

        assert!(matches!(
            error,
            CtError::Timeout(message) if message.contains("login --device-auth")
        ));
    }
}
