//! Thin command adapters: parse args → client call → rendered output.

pub mod agent;
pub mod auth;
pub mod chat;
pub mod login;
pub mod logout;
pub mod review;
pub mod update;
pub mod whoami;

use cloudthinker_client::{CtClient, CtError, TOKEN_ENV_VAR, resolve_store};

/// Read by `--workspace`, and handed to the `agent` child so its own
/// `cloudthinker auth token` resolves the workspace this process resolved.
pub(crate) const WORKSPACE_ENV_VAR: &str = "CLOUDTHINKER_WORKSPACE";

/// True when `CLOUDTHINKER_TOKEN` carries a credential, which outranks the
/// stored ones and cannot be combined with a workspace selection.
pub(crate) fn env_token_is_set() -> bool {
    std::env::var(TOKEN_ENV_VAR).is_ok_and(|value| !value.trim().is_empty())
}

/// Build a read/refresh client (env override if `CLOUDTHINKER_TOKEN` is set,
/// otherwise the keyring-preferred store). Shared by every read-only command;
/// `login`/`logout` use `persistent_store` directly since they write
/// credentials.
pub(crate) fn build_client(base_url: &str, workspace: Option<&str>) -> Result<CtClient, CtError> {
    let store = resolve_store(base_url, workspace)?;
    CtClient::new(base_url, store)
}
