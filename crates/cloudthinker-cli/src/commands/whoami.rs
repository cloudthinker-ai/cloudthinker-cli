//! `cloudthinker whoami` output.

use crate::commands::build_client;
use crate::engine::exit::{self, ExitCode};
use crate::engine::output;

pub async fn run(base_url: &str, workspace: Option<&str>, json: bool) -> ExitCode {
    let client = match build_client(base_url, workspace) {
        Ok(client) => client,
        Err(err) => return exit::report(&err),
    };
    if std::env::var_os(cloudthinker_client::CONFIG_DIR_ENV_VAR).is_some_and(|dir| !dir.is_empty())
        && let Ok(dir) = cloudthinker_client::config_dir()
    {
        output::progress(&format!(
            "credentials: {} (from {})",
            dir.join("credentials.json").display(),
            cloudthinker_client::CONFIG_DIR_ENV_VAR
        ));
    }
    let identity = match client.whoami().await {
        Ok(identity) => identity,
        Err(err) => return exit::report(&err),
    };
    match output::emit_whoami(&identity, json) {
        Ok(()) => ExitCode::Ok,
        Err(err) => exit::report(&cloudthinker_client::CtError::Transport(err)),
    }
}
