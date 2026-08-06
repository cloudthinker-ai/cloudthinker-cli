//! Thin command adapters: parse args → client call → rendered output.

pub mod chat;
pub mod login;
pub mod logout;
pub mod review;
pub mod update;
pub mod whoami;

use cloudthinker_client::{CtClient, CtError, resolve_store};

/// Build a read/refresh client (env override if `CLOUDTHINKER_TOKEN` is set,
/// otherwise the keyring-preferred store). Shared by every read-only command;
/// `login`/`logout` use `persistent_store` directly since they write
/// credentials.
pub(crate) fn build_client(base_url: &str, workspace: Option<&str>) -> Result<CtClient, CtError> {
    let store = resolve_store(base_url, workspace)?;
    CtClient::new(base_url, store)
}
