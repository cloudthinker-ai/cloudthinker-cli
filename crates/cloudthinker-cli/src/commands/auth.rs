//! `cloudthinker auth token` output.

use crate::commands::build_client;
use crate::engine::exit::{self, ExitCode};
use crate::engine::output;

pub async fn run_token(base_url: &str, workspace: Option<&str>) -> ExitCode {
    let client = match build_client(base_url, workspace) {
        Ok(client) => client,
        Err(err) => return exit::report(&err),
    };
    let token = match client.access_token().await {
        Ok(token) => token,
        Err(err) => return exit::report(&err),
    };
    match output::print_access_token(&token) {
        Ok(()) => ExitCode::Ok,
        Err(err) => exit::report(&cloudthinker_client::CtError::Transport(err)),
    }
}
