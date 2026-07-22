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

    /// A structured API error carrying the HTTP status and (when present) the
    /// backend `detail`. The exit-code mapping keys off `status`.
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
}

impl CtError {
    /// True when the watch loop should count this toward its transient-error
    /// budget and keep polling, rather than aborting immediately.
    pub fn is_transport(&self) -> bool {
        matches!(self, CtError::Transport(_))
    }
}

/// Parse a FastAPI error body's `detail` into human text.
///
/// `detail` is either a plain string (a raised `HTTPException`, e.g. the
/// secret-gate 422) or the validation array (`[{msg, loc, ...}]`). Both shapes
/// are flattened to a single line.
pub(crate) fn parse_detail(body: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
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
/// Async because `UnexpectedResponse` (any status the spec did not declare —
/// 400/401/404 on our surface) still has an unread body we want the `detail`
/// from. `InvalidResponsePayload` fires when a *documented* non-2xx body failed
/// to match its typed shape; on our surface the only documented non-2xx is 422,
/// so that branch is the secret-gate string-detail case.
pub(crate) async fn to_ct_error(
    err: ApiError<cloudthinker_api::types::HttpValidationError>,
) -> CtError {
    match err {
        ApiError::ErrorResponse(rv) => {
            let status = rv.status().as_u16();
            let detail = validation_detail(rv.into_inner());
            CtError::Api { status, detail }
        }
        ApiError::UnexpectedResponse(resp) => {
            let status = resp.status().as_u16();
            let detail = resp.text().await.ok().as_deref().and_then(parse_detail);
            CtError::Api { status, detail }
        }
        ApiError::InvalidResponsePayload(bytes, _) => {
            let text = String::from_utf8_lossy(&bytes);
            CtError::Api {
                status: 422,
                detail: parse_detail(&text),
            }
        }
        ApiError::CommunicationError(e) => CtError::Transport(e.to_string()),
        ApiError::ResponseBodyError(e) => CtError::Transport(e.to_string()),
        ApiError::InvalidUpgrade(e) => CtError::Transport(e.to_string()),
        ApiError::InvalidRequest(s) => CtError::Transport(s),
        ApiError::Custom(s) => CtError::Transport(s),
    }
}

fn validation_detail(body: cloudthinker_api::types::HttpValidationError) -> Option<String> {
    let msgs: Vec<String> = body.detail.into_iter().map(|v| v.msg).collect();
    if msgs.is_empty() {
        None
    } else {
        Some(msgs.join("; "))
    }
}
