//! `cloudthinker chat -p` and `cloudthinker chat status <id>`.

use std::time::Duration;

use cloudthinker_client::{CtError, RunStatus, RunView};
use uuid::Uuid;

use crate::engine::exit::{self, ExitCode};
use crate::engine::output::{self, ChatEnvelope};
use crate::engine::watch::{Poll, WatchConfig, watch};

use super::build_client;

/// Submit a prompt, watch to a terminal state, print the result.
pub async fn run_prompt(base_url: &str, prompt: &str, json: bool, timeout_secs: u64) -> ExitCode {
    let client = match build_client(base_url) {
        Ok(client) => client,
        Err(err) => return exit::report(&err),
    };

    let submitted = match client.submit_run(prompt).await {
        Ok(submitted) => submitted,
        Err(err) => return exit::report(&err),
    };
    let run_id = submitted.run_id;
    if !json {
        output::progress(&format!("Submitted run {run_id}. Waiting for Anna…"));
    }

    let cfg = WatchConfig::for_run(Duration::from_secs(timeout_secs));
    let outcome = watch(
        async || {
            let view = client.get_run(run_id).await?;
            if view.status.is_terminal() {
                Ok(Poll::Terminal(view))
            } else {
                Ok(Poll::Pending)
            }
        },
        &cfg,
    )
    .await;

    match outcome {
        Ok(view) => finish(&view, json),
        Err(CtError::Timeout(_)) => {
            output::progress(&format!(
                "Timed out. The run continues server-side — resume with: cloudthinker chat status {run_id}"
            ));
            ExitCode::Timeout
        }
        Err(err) => exit::report(&err),
    }
}

/// Show a single run's current status.
pub async fn run_status(base_url: &str, run_id: Uuid, json: bool) -> ExitCode {
    let client = match build_client(base_url) {
        Ok(client) => client,
        Err(err) => return exit::report(&err),
    };

    match client.get_run(run_id).await {
        Ok(view) => {
            if !json {
                output::print_status_summary(&view);
            } else if let Err(err) = output::emit_json(&ChatEnvelope::from_view(&view)) {
                output::eprintln_error(&err);
                return ExitCode::JobFailed;
            }
            // `chat status` is a read: a successful fetch exits 0 regardless of
            // the run's own status (CA-CLI-16).
            ExitCode::Ok
        }
        Err(CtError::Api { status: 404, .. }) => {
            output::eprintln_error(&format!("run not found: {run_id}"));
            ExitCode::JobFailed
        }
        Err(err) => exit::report(&err),
    }
}

/// Render a terminal run and pick its exit code.
fn finish(view: &RunView, json: bool) -> ExitCode {
    if json && let Err(err) = output::emit_json(&ChatEnvelope::from_view(view)) {
        output::eprintln_error(&err);
        return ExitCode::JobFailed;
    }

    match view.status {
        RunStatus::Succeeded => {
            if !json {
                // CA-CLI-10: the answer is the ONLY thing on stdout.
                output::print_answer(view.answer.as_deref().unwrap_or_default());
            }
            ExitCode::Ok
        }
        RunStatus::Failed => {
            if !json {
                let suffix = view
                    .failure_kind
                    .as_deref()
                    .map(|k| format!(" ({k})"))
                    .unwrap_or_default();
                let detail = view.message.as_deref().unwrap_or("the run failed");
                output::eprintln_error(&format!("{detail}{suffix}"));
            }
            ExitCode::JobFailed
        }
        RunStatus::RequiredApproval => {
            if !json {
                match &view.web_url {
                    Some(url) => output::progress(&format!(
                        "Approval required. Approve in your browser: {url}"
                    )),
                    None => output::progress("Approval required. Open CloudThinker to approve."),
                }
            }
            ExitCode::ApprovalRequired
        }
        // Non-terminal states never reach `finish` (watch only yields terminal).
        RunStatus::Pending | RunStatus::Running => ExitCode::JobFailed,
    }
}
