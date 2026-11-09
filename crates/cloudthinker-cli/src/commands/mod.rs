//! Thin command adapters: parse args → client call → rendered output.

pub mod agent;
pub mod agents;
pub mod auth;
#[cfg(unix)]
pub mod bg_shim;
pub mod chat;
pub mod cloud;
pub mod completion;
pub mod connections;
pub mod cyber;
pub mod cyber_config;
pub mod cyber_discovery;
pub mod cyber_doctor;
pub mod cyber_memory;
pub mod doctor;
pub mod incident;
pub mod login;
pub mod logout;
pub mod policy;
pub mod recommendations;
pub mod remote;
pub mod review;
pub mod update;
pub mod whoami;
pub mod worker;
#[cfg(unix)]
pub mod worker_doctor;
pub mod worker_service;
pub mod workspace;

use cloudthinker_client::{CtClient, CtError, CtResult, TOKEN_ENV_VAR, resolve_store};
use serde::Serialize;

use crate::engine::exit::{self, ExitCode};
use crate::engine::output;

pub(crate) fn render_fetched<T: Serialize>(
    result: CtResult<T>,
    json: bool,
    print: impl FnOnce(&T) -> Result<(), String>,
) -> ExitCode {
    match result {
        Ok(value) => {
            let rendered = if json {
                output::emit_json(&value)
            } else {
                print(&value)
            };
            exit::finish_output(rendered)
        }
        Err(err) => exit::report(&err),
    }
}

/// Read by `--workspace`, and handed to the `agent` child so its own
/// `cloudthinker auth token` resolves the workspace this process resolved.
pub(crate) const WORKSPACE_ENV_VAR: &str = "CLOUDTHINKER_WORKSPACE";

/// True when `CLOUDTHINKER_TOKEN` carries a credential, which outranks the
/// stored ones and cannot be combined with a workspace selection.
pub(crate) fn env_token_is_set() -> bool {
    std::env::var(TOKEN_ENV_VAR).is_ok_and(|value| !value.trim().is_empty())
}

/// Build a read/refresh client (env override if `CLOUDTHINKER_TOKEN` is set,
/// otherwise the credentials file). Shared by every read-only command;
/// `login`/`logout` use `persistent_store` directly since they write
/// credentials.
pub(crate) fn build_client(base_url: &str, workspace: Option<&str>) -> Result<CtClient, CtError> {
    let store = resolve_store(base_url, workspace)?;
    CtClient::new(base_url, store)
}
