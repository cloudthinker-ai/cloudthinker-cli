use std::collections::BTreeSet;

use cloudthinker_client::{login_command, persistent_store};

use crate::engine::exit::{self, ExitCode};
use crate::engine::output;

use super::{build_client, env_token_is_set};

pub async fn run_list(base_url: &str, workspace: Option<&str>, json: bool) -> ExitCode {
    let client = match build_client(base_url, workspace) {
        Ok(client) => client,
        Err(error) => return exit::report(&error),
    };
    let saved = match persistent_store(base_url, None).and_then(|store| store.workspaces()) {
        Ok(stored) => Some(
            stored
                .into_iter()
                .map(|row| row.workspace_id)
                .collect::<BTreeSet<_>>(),
        ),
        Err(error) => {
            output::warn(&format!(
                "could not read the workspace logins saved on this machine: {error}"
            ));
            None
        }
    };
    let env_token = env_token_is_set();
    match client.list_workspaces(saved.as_ref()).await {
        Ok(workspaces) => {
            if env_token {
                output::warn(
                    "CLOUDTHINKER_TOKEN is set, so commands keep using it until you unset it; --workspace and auth switch apply only after that.",
                );
            }
            exit::finish_output(output::emit_workspaces(
                &workspaces,
                &login_command(base_url),
                !env_token,
                json,
            ))
        }
        Err(error) => exit::report(&error),
    }
}
