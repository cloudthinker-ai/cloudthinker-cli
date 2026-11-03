use crate::engine::exit::{self, ExitCode};
use crate::engine::output;

use super::build_client;

pub async fn run_list(base_url: &str, workspace: Option<&str>, json: bool) -> ExitCode {
    let client = match build_client(base_url, workspace) {
        Ok(client) => client,
        Err(error) => return exit::report(&error),
    };
    match client.list_agents().await {
        Ok(agents) => exit::finish_output(output::emit_agents(&agents.data, json)),
        Err(error) => exit::report(&error),
    }
}

pub async fn run_show(
    base_url: &str,
    workspace: Option<&str>,
    reference: &str,
    json: bool,
) -> ExitCode {
    let client = match build_client(base_url, workspace) {
        Ok(client) => client,
        Err(error) => return exit::report(&error),
    };
    match client.agent(reference).await {
        Ok(agent) => exit::finish_output(output::emit_agent(&agent, json)),
        Err(error) => exit::report(&error),
    }
}
