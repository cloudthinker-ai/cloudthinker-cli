//! Headless chat submit, continuation, status, and run-list commands.

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use cloudthinker_client::{CtClient, CtError, RunStatus, RunView};
use uuid::Uuid;

use crate::engine::exit::{self, ExitCode};
use crate::engine::output::{self, ChatEnvelope, ChatSubmittedEnvelope};
use crate::engine::watch::{Poll, WatchConfig, watch};

use super::build_client;

pub struct PromptOptions<'a> {
    pub base_url: &'a str,
    pub workspace: Option<&'a str>,
    pub prompt: &'a str,
    pub continue_id: Option<Uuid>,
    pub selected_agent_reference: Option<&'a str>,
    pub no_wait: bool,
    pub json: bool,
    pub timeout_secs: u64,
}

/// Submit a prompt, watch to a terminal state, print the result.
pub async fn run_prompt(options: PromptOptions<'_>) -> ExitCode {
    let client = match build_client(options.base_url, options.workspace) {
        Ok(client) => client,
        Err(err) => return exit::report(&err),
    };

    let conversation_id = match options.continue_id {
        Some(id) => match client.resolve_conversation_id(id).await {
            Ok(conversation_id) => Some(conversation_id),
            Err(err) => return exit::report(&err),
        },
        None => None,
    };
    let submitted = match client
        .submit_run(
            options.prompt,
            conversation_id,
            options.selected_agent_reference,
        )
        .await
    {
        Ok(submitted) => submitted,
        Err(CtError::Api { status: 404, .. }) if options.continue_id.is_some() => {
            output::eprintln_error("conversation or run not found");
            return ExitCode::JobFailed;
        }
        Err(err) => return exit::report(&err),
    };
    if options.no_wait {
        let result = if options.json {
            output::emit_json(&ChatSubmittedEnvelope::from(&submitted))
        } else {
            output::print_submitted(&submitted)
        };
        return match result {
            Ok(()) => ExitCode::Ok,
            Err(err) => {
                output::eprintln_error(&err);
                ExitCode::JobFailed
            }
        };
    }

    let run_id = submitted.run_id;
    let interrupted = listen_for_interrupt();
    if !options.json {
        output::progress(&format!(
            "Submitted run {run_id} to your CloudThinker workspace (cloud). It runs there and cannot see your local files."
        ));
    }

    wait_for_run(
        &client,
        run_id,
        options.json,
        options.timeout_secs,
        &format!("cloudthinker chat status {run_id}"),
        interrupted,
    )
    .await
}

pub(crate) type Interrupted = Pin<Box<dyn Future<Output = ()> + Send>>;

#[cfg(unix)]
pub(crate) fn listen_for_interrupt() -> Interrupted {
    use tokio::signal::unix::{SignalKind, signal};
    match signal(SignalKind::interrupt()) {
        Ok(mut interrupt) => Box::pin(async move {
            interrupt.recv().await;
        }),
        Err(error) => {
            output::warn(&format!(
                "could not listen for Ctrl-C before the wait: {error}"
            ));
            Box::pin(async {
                let _ = tokio::signal::ctrl_c().await;
            })
        }
    }
}

#[cfg(not(unix))]
pub(crate) fn listen_for_interrupt() -> Interrupted {
    Box::pin(async {
        let _ = tokio::signal::ctrl_c().await;
    })
}

async fn wait_for_run(
    client: &CtClient,
    run_id: Uuid,
    json: bool,
    timeout_secs: u64,
    resume: &str,
    interrupted: Interrupted,
) -> ExitCode {
    let live = (!json).then(|| output::live_status(&waiting_line(None)));
    let cfg = WatchConfig::for_run(Duration::from_secs(timeout_secs));
    let outcome = tokio::select! {
        outcome = watch(
            async || {
                let view = client.get_run(run_id).await?;
                if let Some(live) = &live {
                    live.set_message(waiting_line(Some(view.status)));
                }
                if view.status.is_terminal() {
                    Ok(Poll::Terminal(view))
                } else {
                    Ok(Poll::Pending)
                }
            },
            &cfg,
        ) => outcome,
        () = interrupted => {
            drop(live);
            output::progress(&format!(
                "Stopped waiting. The run continues server-side — resume with: cloudthinker chat status {run_id} --wait"
            ));
            return ExitCode::Interrupted;
        }
    };
    drop(live);

    match outcome {
        Ok(view) => finish(&view, json),
        Err(CtError::Timeout(_)) => {
            output::progress(&format!(
                "Timed out. The run continues server-side — resume with: {resume}"
            ));
            ExitCode::Timeout
        }
        Err(err) => exit::report(&err),
    }
}

fn waiting_line(status: Option<RunStatus>) -> String {
    match status {
        Some(status) => format!(
            "CloudThinker is working — {}. Ctrl-C stops waiting; the run continues",
            output::status_label(status)
        ),
        None => "CloudThinker is working. Ctrl-C stops waiting; the run continues".to_string(),
    }
}

/// Show a single run's current status.
pub async fn run_status(
    base_url: &str,
    workspace: Option<&str>,
    run_id: Uuid,
    json: bool,
    wait: bool,
    timeout_secs: u64,
) -> ExitCode {
    let client = match build_client(base_url, workspace) {
        Ok(client) => client,
        Err(err) => return exit::report(&err),
    };

    if wait {
        return wait_for_run(
            &client,
            run_id,
            json,
            timeout_secs,
            &format!("cloudthinker chat status {run_id} --wait"),
            listen_for_interrupt(),
        )
        .await;
    }

    match client.get_run(run_id).await {
        Ok(view) => {
            let result = if json {
                output::emit_json(&ChatEnvelope::from_view(&view))
            } else {
                output::print_status_summary(&view)
            };
            if let Err(err) = result {
                output::eprintln_error(&err);
                return ExitCode::JobFailed;
            }
            if view.status.is_terminal() {
                output::continuation_hint(view.conversation_id, view.web_url.as_deref());
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

/// List recent headless runs, optionally within one conversation.
pub async fn run_list(
    base_url: &str,
    workspace: Option<&str>,
    conversation_id: Option<Uuid>,
    limit: u64,
    json: bool,
) -> ExitCode {
    let client = match build_client(base_url, workspace) {
        Ok(client) => client,
        Err(err) => return exit::report(&err),
    };
    let runs = match client.list_runs(limit, conversation_id).await {
        Ok(runs) => runs,
        Err(err) => return exit::report(&err),
    };
    let result = if json {
        output::emit_json(&runs)
    } else {
        output::print_run_list(&runs)
    };
    match result {
        Ok(()) => ExitCode::Ok,
        Err(err) => {
            output::eprintln_error(&err);
            ExitCode::JobFailed
        }
    }
}

/// Render a terminal run and pick its exit code.
fn finish(view: &RunView, json: bool) -> ExitCode {
    if json && let Err(err) = output::emit_json(&ChatEnvelope::from_view(view)) {
        output::eprintln_error(&err);
        return ExitCode::JobFailed;
    }

    output::continuation_hint(view.conversation_id, view.web_url.as_deref());

    match view.status {
        RunStatus::Succeeded => {
            if !json {
                // CA-CLI-10: the answer is the ONLY thing on stdout.
                if let Err(err) = output::print_answer(view.answer.as_deref().unwrap_or_default()) {
                    output::eprintln_error(&err);
                    return ExitCode::JobFailed;
                }
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
