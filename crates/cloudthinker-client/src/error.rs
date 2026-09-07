//! Typed errors for every wire + auth operation.
//!
//! The binary crate maps these onto stable exit codes (`engine/exit.rs`); keep
//! the variants coarse enough that the mapping stays a small table.

use cloudthinker_api::Error as ApiError;

/// Result alias used throughout the client crate.
pub type CtResult<T> = Result<T, CtError>;

/// Everything that can go wrong talking to the CloudThinker API.
#[derive(Debug, thiserror::Error)]
pub enum CtError {
    /// Authentication is missing, expired, or refresh failed — the user must
    /// run `cloudthinker login`.
    #[error("not authenticated: {0}")]
    Auth(String),

    /// Persisted credentials predate workspace-keyed storage. They are never
    /// migrated or used; a new login replaces them.
    #[error("stored credentials use an obsolete format; run `cloudthinker login`")]
    ObsoleteCredentials,

    /// A structured API error carrying the HTTP status and safe backend
    /// message. The exit-code mapping keys off `status`.
    #[error("API error {status}{}", .detail.as_deref().map(|d| format!(": {d}")).unwrap_or_default())]
    Api { status: u16, detail: Option<String> },

    /// A network / transport failure (connection reset, TLS, request timeout).
    /// The watch loop tolerates a bounded run of these before giving up.
    #[error("transport error: {0}")]
    Transport(String),

    /// The user gave an argument the client rejected before any request
    /// (e.g. an empty prompt) — maps to the usage exit code.
    #[error("{0}")]
    Usage(String),

    /// A client-imposed deadline (login wait or `chat --timeout`) elapsed.
    #[error("timed out: {0}")]
    Timeout(String),

    /// The user denied the consent screen during login.
    #[error("login was denied in the browser")]
    LoginDenied,

    /// A local token-store failure (keyring + file both unusable).
    #[error("token store error: {0}")]
    Store(String),

    /// Login PKCE / loopback plumbing failure.
    #[error("login failed: {0}")]
    Login(String),

    /// Local credentials were cleared, but one or more server sessions could
    /// not be revoked.
    #[error("logout incomplete: {0}")]
    Logout(String),

    /// The released `cloudthinker-agent` bundle could not be resolved,
    /// downloaded, verified, or installed. Distinct from `Transport` so a bad
    /// digest or a tampered archive never reads as a flaky network.
    #[error("agent install failed: {0}")]
    AgentInstall(String),

    /// A response body that failed to parse into either the documented success
    /// or error shape — the server sent something we don't understand rather
    /// than the user giving bad input. Kept distinct from `Api` so scripts
    /// don't treat "server sent garbage" as "bad user input".
    #[error("malformed response: {0}")]
    Protocol(String),
}

impl CtError {
    /// True when the watch loop should count this toward its transient-error
    /// budget and keep polling, rather than aborting immediately.
    pub fn is_transport(&self) -> bool {
        matches!(self, CtError::Transport(_))
    }
}

/// Parse safe human text from a current or legacy API error body.
///
/// Current responses use `error.message`. Legacy `detail` may be a string or a
/// validation array; validation messages are flattened to one line.
pub(crate) fn parse_error_message(body: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    if let Some(message) = value
        .get("error")
        .and_then(|error| error.get("message"))
        .and_then(|message| message.as_str())
        .filter(|message| !message.trim().is_empty())
    {
        return Some(message.to_string());
    }
    let detail = value.get("detail")?;
    match detail {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Array(items) => {
            let msgs: Vec<String> = items
                .iter()
                .filter_map(|item| item.get("msg").and_then(|m| m.as_str()))
                .map(str::to_string)
                .collect();
            if msgs.is_empty() {
                None
            } else {
                Some(msgs.join("; "))
            }
        }
        _ => None,
    }
}

/// Convert a generated-client error into a `CtError`.
///
/// Generic HTTP failures stay `UnexpectedResponse` so their status remains
/// available while old servers may still return detail-only bodies. Reading
/// that response body makes this conversion async. `InvalidResponsePayload`
/// only represents a declared body that could not be decoded.
pub(crate) async fn to_ct_error(err: ApiError<()>) -> CtError {
    match err {
        ApiError::ErrorResponse(rv) => {
            let status = rv.status().as_u16();
            CtError::Api {
                status,
                detail: None,
            }
        }
        ApiError::UnexpectedResponse(resp) => {
            let status = resp.status().as_u16();
            let detail = resp
                .text()
                .await
                .ok()
                .as_deref()
                .and_then(parse_error_message);
            CtError::Api { status, detail }
        }
        ApiError::InvalidResponsePayload(_, parse_err) => {
            CtError::Protocol(format!("unreadable response body: {parse_err}"))
        }
        ApiError::CommunicationError(e) => CtError::Transport(e.to_string()),
        ApiError::ResponseBodyError(e) => CtError::Transport(e.to_string()),
        ApiError::InvalidUpgrade(e) => CtError::Transport(e.to_string()),
        ApiError::InvalidRequest(s) => CtError::Transport(s),
        ApiError::Custom(s) => CtError::Transport(s),
    }
}
