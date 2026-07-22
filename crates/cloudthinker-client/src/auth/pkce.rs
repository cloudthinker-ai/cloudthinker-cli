//! PKCE challenge generation + the loopback consent callback server.
//!
//! Login flow: generate a verifier/challenge/state, bind a loopback server on an
//! ephemeral port, send the browser to the consent page, and wait for the
//! consent page to redirect back to `http://127.0.0.1:<port>/callback` with the
//! one-time `code`. The verifier is then exchanged for a token.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::extract::{Query, State};
use axum::response::Html;
use axum::routing::get;
use base64::Engine;
use sha2::{Digest, Sha256};
use tokio::sync::oneshot;

use crate::error::{CtError, CtResult};

const VERIFIER_BYTES: usize = 64;
const STATE_BYTES: usize = 32;
const LOOPBACK_HOST: [u8; 4] = [127, 0, 0, 1];

fn b64url(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// A PKCE challenge triple bound to one login attempt.
pub struct PkceChallenge {
    /// Kept secret on the client; proves possession at token exchange.
    pub verifier: String,
    /// `base64url(sha256(verifier))` — travels to the consent page.
    pub challenge: String,
    /// CSRF guard echoed back on the callback.
    pub state: String,
}

impl PkceChallenge {
    pub fn generate() -> Self {
        let mut verifier_bytes = [0u8; VERIFIER_BYTES];
        let mut state_bytes = [0u8; STATE_BYTES];
        rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut verifier_bytes);
        rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut state_bytes);

        let verifier = b64url(&verifier_bytes);
        let challenge = b64url(Sha256::digest(verifier.as_bytes()).as_slice());
        let state = b64url(&state_bytes);
        Self {
            verifier,
            challenge,
            state,
        }
    }
}

/// Build the consent-page URL `{base}/auth/cli?challenge=&port=&state=`.
pub fn consent_url(base_url: &str, challenge: &str, port: u16, state: &str) -> CtResult<String> {
    let mut url =
        url::Url::parse(base_url).map_err(|e| CtError::Login(format!("invalid base url: {e}")))?;
    url.set_path("/auth/cli");
    url.query_pairs_mut()
        .append_pair("challenge", challenge)
        .append_pair("port", &port.to_string())
        .append_pair("state", state);
    Ok(url.to_string())
}

/// One-shot loopback server that receives the consent callback.
pub struct Loopback {
    listener: std::net::TcpListener,
    addr: SocketAddr,
    state: String,
}

impl Loopback {
    /// Bind `127.0.0.1:0`. Asserts the OS handed back an unprivileged port —
    /// the consent page refuses to redirect to ports below 1024, so a privileged
    /// port would be a dead end (invariant: bind loopback, unprivileged port).
    pub fn bind(state: String) -> CtResult<Self> {
        let listener = std::net::TcpListener::bind((IpAddr::from(LOOPBACK_HOST), 0))
            .map_err(|e| CtError::Login(format!("loopback bind: {e}")))?;
        let addr = listener
            .local_addr()
            .map_err(|e| CtError::Login(format!("loopback addr: {e}")))?;
        if addr.port() < 1024 {
            return Err(CtError::Login(format!(
                "loopback bound to privileged port {}",
                addr.port()
            )));
        }
        Ok(Self {
            listener,
            addr,
            state,
        })
    }

    pub fn port(&self) -> u16 {
        self.addr.port()
    }

    /// The bound IP — always loopback (asserted by CA-CLI-17).
    pub fn local_ip(&self) -> IpAddr {
        self.addr.ip()
    }

    /// Serve exactly one `/callback`, returning the one-time code.
    ///
    /// A `state` that does not match the one we generated is rejected without
    /// yielding a code and without leaving the server listening (fail-closed
    /// against a forged callback).
    pub async fn wait_for_code(self, timeout: Duration) -> CtResult<String> {
        let (tx, rx) = oneshot::channel::<CallbackOutcome>();
        let shared = Arc::new(Shared {
            expected_state: self.state,
            tx: Mutex::new(Some(tx)),
        });
        let app = Router::new()
            .route("/callback", get(callback))
            .with_state(shared);

        self.listener
            .set_nonblocking(true)
            .map_err(|e| CtError::Login(format!("loopback nonblocking: {e}")))?;
        let listener = tokio::net::TcpListener::from_std(self.listener)
            .map_err(|e| CtError::Login(format!("loopback adopt: {e}")))?;
        let server = tokio::spawn(async move { axum::serve(listener, app).await });

        let outcome = match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(outcome)) => outcome,
            Ok(Err(_)) => {
                server.abort();
                return Err(CtError::Login("callback channel closed".into()));
            }
            Err(_) => {
                server.abort();
                return Err(CtError::Timeout(
                    "timed out waiting for browser consent".into(),
                ));
            }
        };
        server.abort();

        match outcome {
            CallbackOutcome::Code(code) => Ok(code),
            CallbackOutcome::Denied => Err(CtError::LoginDenied),
            CallbackOutcome::StateMismatch => {
                Err(CtError::Auth("consent callback state mismatch".into()))
            }
        }
    }
}

enum CallbackOutcome {
    Code(String),
    Denied,
    StateMismatch,
}

struct Shared {
    expected_state: String,
    tx: Mutex<Option<oneshot::Sender<CallbackOutcome>>>,
}

/// Branded loopback page shown in the browser once consent returns. Self-
/// contained (inline CSS, no network) so it renders even offline; `{accent}`,
/// `{title}`, and `{body}` are the only per-outcome parts.
fn close_page(accent: &str, title: &str, body: &str) -> String {
    format!(
        "<!doctype html><html lang=en><meta charset=utf-8>\
<meta name=viewport content=\"width=device-width,initial-scale=1\">\
<title>CloudThinker CLI</title>\
<body style=\"margin:0;min-height:100vh;display:flex;align-items:center;justify-content:center;\
background:#fafafa;color:#18181b;font-family:system-ui,-apple-system,'Segoe UI',sans-serif\">\
<main style=\"text-align:center;padding:2rem;max-width:26rem\">\
<div style=\"width:56px;height:56px;margin:0 auto 1.5rem;border-radius:9999px;\
display:flex;align-items:center;justify-content:center;background:{accent}22;color:{accent};\
font-size:26px;line-height:1\">{glyph}</div>\
<h1 style=\"font-size:1.5rem;font-weight:600;letter-spacing:-.01em;margin:0 0 .5rem\">{title}</h1>\
<p style=\"font-size:.95rem;color:#52525b;margin:0;line-height:1.5\">{body}</p>\
</main></body></html>",
        accent = accent,
        title = title,
        body = body,
        glyph = if accent == "#dc2626" {
            "&times;"
        } else {
            "&check;"
        },
    )
}

fn close_html(outcome: &CallbackOutcome) -> String {
    match outcome {
        CallbackOutcome::Code(_) => close_page(
            "#0d9488",
            "You&rsquo;re all set",
            "CloudThinker CLI is signed in. You can close this window and return to your terminal.",
        ),
        CallbackOutcome::Denied => close_page(
            "#dc2626",
            "Sign-in denied",
            "No access was granted. You can close this window and return to your terminal.",
        ),
        CallbackOutcome::StateMismatch => close_page(
            "#dc2626",
            "Sign-in couldn&rsquo;t complete",
            "This sign-in link is invalid or expired. Run cloudthinker login again from your terminal.",
        ),
    }
}

async fn callback(
    State(shared): State<Arc<Shared>>,
    Query(params): Query<HashMap<String, String>>,
) -> ([(axum::http::HeaderName, &'static str); 1], Html<String>) {
    let outcome = classify(&shared.expected_state, &params);
    let html = close_html(&outcome);
    if let Ok(mut guard) = shared.tx.lock()
        && let Some(sender) = guard.take()
    {
        let _ = sender.send(outcome);
    }
    // `Connection: close` avoids a parked keep-alive socket hanging the NEXT
    // login (codex tiny_http gotcha, server.rs:509).
    ([(axum::http::header::CONNECTION, "close")], Html(html))
}

fn classify(expected_state: &str, params: &HashMap<String, String>) -> CallbackOutcome {
    if params.get("error").map(String::as_str) == Some("access_denied") {
        return CallbackOutcome::Denied;
    }
    match params.get("state") {
        Some(state) if state == expected_state => match params.get("code") {
            Some(code) if !code.is_empty() => CallbackOutcome::Code(code.clone()),
            _ => CallbackOutcome::StateMismatch,
        },
        _ => CallbackOutcome::StateMismatch,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn hit_callback(port: u16, query: &str) {
        // The listener is already bound, so the connection queues even before
        // axum accepts. A tiny delay lets the spawned server start serving.
        tokio::time::sleep(Duration::from_millis(50)).await;
        let _ = reqwest::get(format!("http://127.0.0.1:{port}/callback?{query}")).await;
    }

    #[test]
    fn challenge_is_sha256_of_verifier() {
        let pkce = PkceChallenge::generate();
        let expected = b64url(Sha256::digest(pkce.verifier.as_bytes()).as_slice());
        assert_eq!(pkce.challenge, expected);
        assert_ne!(pkce.state, pkce.verifier);
    }

    #[test]
    fn close_html_differs_per_outcome() {
        let ok = close_html(&CallbackOutcome::Code("c".into()));
        let denied = close_html(&CallbackOutcome::Denied);
        let mismatch = close_html(&CallbackOutcome::StateMismatch);
        assert!(ok.contains("You&rsquo;re all set"));
        assert!(ok.contains("#0d9488")); // success accent, not the error red
        assert!(denied.contains("Sign-in denied"));
        assert!(denied.contains("#dc2626"));
        assert!(mismatch.contains("couldn&rsquo;t complete"));
        // Every page is a complete, self-contained document (renders offline).
        for html in [&ok, &denied, &mismatch] {
            assert!(html.starts_with("<!doctype html>"));
            assert!(html.contains("</html>"));
        }
    }

    #[test]
    fn consent_url_carries_challenge_port_state() {
        let url = consent_url("https://app.example.com", "chal", 49152, "st8").unwrap();
        assert!(url.starts_with("https://app.example.com/auth/cli?"));
        assert!(url.contains("challenge=chal"));
        assert!(url.contains("port=49152"));
        assert!(url.contains("state=st8"));
    }

    // CA-CLI-17: the loopback server binds 127.0.0.1 on an unprivileged port,
    // never a routable interface.
    #[test]
    fn ca_cli_17_loopback_binds_loopback_only() {
        let loopback = Loopback::bind("state".into()).unwrap();
        assert!(loopback.local_ip().is_loopback());
        assert_eq!(loopback.local_ip(), IpAddr::from([127, 0, 0, 1]));
        assert!(loopback.port() >= 1024, "must be an unprivileged port");
    }

    #[tokio::test]
    async fn happy_callback_yields_code() {
        let loopback = Loopback::bind("state-ok".into()).unwrap();
        let port = loopback.port();
        let server =
            tokio::spawn(async move { loopback.wait_for_code(Duration::from_secs(5)).await });
        hit_callback(port, "code=abc123&state=state-ok").await;
        let result = server.await.unwrap();
        assert_eq!(result.unwrap(), "abc123");
    }

    // CA-CLI-2: a denied consent redirects with error=access_denied.
    #[tokio::test]
    async fn ca_cli_2_access_denied_maps_to_login_denied() {
        let loopback = Loopback::bind("state-deny".into()).unwrap();
        let port = loopback.port();
        let server =
            tokio::spawn(async move { loopback.wait_for_code(Duration::from_secs(5)).await });
        hit_callback(port, "error=access_denied&state=state-deny").await;
        let result = server.await.unwrap();
        assert!(
            matches!(result, Err(CtError::LoginDenied)),
            "got {result:?}"
        );
    }

    // CA-CLI-3: a callback whose state does not match is rejected (fail closed),
    // yielding no code.
    #[tokio::test]
    async fn ca_cli_3_state_mismatch_is_rejected() {
        let loopback = Loopback::bind("state-real".into()).unwrap();
        let port = loopback.port();
        let server =
            tokio::spawn(async move { loopback.wait_for_code(Duration::from_secs(5)).await });
        hit_callback(port, "code=abc&state=state-forged").await;
        let result = server.await.unwrap();
        assert!(matches!(result, Err(CtError::Auth(_))), "got {result:?}");
    }

    // CA-CLI-5: no callback within the wait window times out.
    #[tokio::test]
    async fn ca_cli_5_login_wait_times_out() {
        let loopback = Loopback::bind("state-timeout".into()).unwrap();
        let result = loopback.wait_for_code(Duration::from_millis(100)).await;
        assert!(matches!(result, Err(CtError::Timeout(_))), "got {result:?}");
    }
}
