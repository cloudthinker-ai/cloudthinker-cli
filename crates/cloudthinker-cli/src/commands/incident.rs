use std::time::Duration;

use cloudthinker_client::{CtClient, CtError, IncidentStatus, IncidentView};
use uuid::Uuid;

use crate::engine::exit::{self, ExitCode};
use crate::engine::output;
use crate::engine::watch::{Poll, WatchConfig, watch};

use super::build_client;
use super::chat::{Interrupted, listen_for_interrupt};

pub async fn run_list(
    base_url: &str,
    workspace: Option<&str>,
    status: Option<IncidentStatus>,
    limit: u64,
    json: bool,
) -> ExitCode {
    let client = match build_client(base_url, workspace) {
        Ok(client) => client,
        Err(err) => return exit::report(&err),
    };
    let incidents = match client.list_incidents(status, limit).await {
        Ok(incidents) => incidents,
        Err(err) => return exit::report(&err),
    };
    let result = if json {
        output::emit_json(&incidents)
    } else {
        output::print_incident_list(&incidents)
    };
    finish_output(result)
}

pub async fn run_status(
    base_url: &str,
    workspace: Option<&str>,
    incident_id: Uuid,
    json: bool,
    wait: bool,
    timeout_secs: u64,
) -> ExitCode {
    let client = match build_client(base_url, workspace) {
        Ok(client) => client,
        Err(err) => return exit::report(&err),
    };
    let fetched = if wait {
        wait_for_incident(&client, incident_id, timeout_secs, listen_for_interrupt()).await
    } else {
        client
            .get_incident(incident_id)
            .await
            .map_err(Waited::Failed)
    };
    let view = match fetched {
        Ok(view) => view,
        Err(Waited::Failed(CtError::Api { status: 404, .. })) => {
            output::eprintln_error(&format!("incident not found: {incident_id}"));
            return ExitCode::JobFailed;
        }
        Err(Waited::Failed(CtError::Timeout(_))) => {
            output::progress(&format!(
                "Timed out. The incident is still open — resume with: cloudthinker incident status {incident_id} --wait"
            ));
            return ExitCode::Timeout;
        }
        Err(Waited::Failed(err)) => return exit::report(&err),
        Err(Waited::Interrupted) => {
            output::progress(&format!(
                "Stopped waiting. Resume with: cloudthinker incident status {incident_id} --wait"
            ));
            return ExitCode::Interrupted;
        }
    };
    let result = if json {
        output::emit_json(&view)
    } else {
        output::print_incident_status(&view)
    };
    finish_output(result)
}

enum Waited {
    Failed(CtError),
    Interrupted,
}

async fn wait_for_incident(
    client: &CtClient,
    incident_id: Uuid,
    timeout_secs: u64,
    interrupted: Interrupted,
) -> Result<IncidentView, Waited> {
    let cfg = WatchConfig::for_run(Duration::from_secs(timeout_secs));
    tokio::select! {
        outcome = watch(
            async || {
                let view = client.get_incident(incident_id).await?;
                if view.status.is_terminal() {
                    Ok(Poll::Terminal(view))
                } else {
                    Ok(Poll::Pending)
                }
            },
            &cfg,
        ) => outcome.map_err(Waited::Failed),
        () = interrupted => Err(Waited::Interrupted),
    }
}

fn finish_output(result: Result<(), String>) -> ExitCode {
    match result {
        Ok(()) => ExitCode::Ok,
        Err(err) => {
            output::eprintln_error(&err);
            ExitCode::JobFailed
        }
    }
}
