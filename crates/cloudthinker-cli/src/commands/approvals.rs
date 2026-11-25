use std::time::Duration;

use cloudthinker_client::{ApprovalListQuery, ApprovalState, ApprovalView, CtClient, CtError};
use uuid::Uuid;

use crate::engine::exit::{self, ExitCode};
use crate::engine::output;
use crate::engine::watch::{Poll, WatchConfig, watch};

use super::build_client;
use super::chat::{Interrupted, listen_for_interrupt};

pub const DEFAULT_POLL_SECS: u64 = 5;
pub const MIN_POLL_SECS: u64 = 1;
pub const MAX_POLL_SECS: u64 = 300;

#[derive(Debug, Clone, Copy)]
pub struct WatchTiming {
    pub timeout_secs: u64,
    pub interval_secs: u64,
}

impl WatchTiming {
    fn config(self) -> WatchConfig {
        WatchConfig::every(
            Duration::from_secs(self.interval_secs.clamp(MIN_POLL_SECS, MAX_POLL_SECS)),
            Duration::from_secs(self.timeout_secs),
        )
    }
}

pub async fn run_list(
    base_url: &str,
    workspace: Option<&str>,
    query: &ApprovalListQuery,
    json: bool,
) -> ExitCode {
    let client = match build_client(base_url, workspace) {
        Ok(client) => client,
        Err(err) => return exit::report(&err),
    };
    match client.list_approvals(query).await {
        Ok(page) => exit::finish_output(output::emit_approval_page(&page, json)),
        Err(err) => exit::report(&err),
    }
}

pub async fn run_watch(
    base_url: &str,
    workspace: Option<&str>,
    request_id: Uuid,
    json: bool,
    timing: WatchTiming,
) -> ExitCode {
    let client = match build_client(base_url, workspace) {
        Ok(client) => client,
        Err(err) => return exit::report(&err),
    };
    let first = match client.get_approval(request_id).await {
        Ok(view) => view,
        Err(err) => return failed(request_id, Waited::Failed(err)),
    };
    if let Err(error) = output::emit_approval_watch(&first, json, true) {
        return exit::finish_output(Err(error));
    }
    if first.state.is_closed() {
        return decided(&first);
    }
    output::progress("Waiting for a decision. Ctrl-C stops watching; the request stays open.");
    let last = match wait_for_decision(&client, request_id, timing, listen_for_interrupt()).await {
        Ok(view) => view,
        Err(waited) => return failed(request_id, waited),
    };
    if let Err(error) = output::emit_approval_watch(&last, json, false) {
        return exit::finish_output(Err(error));
    }
    decided(&last)
}

fn decided(view: &ApprovalView) -> ExitCode {
    match view.state {
        ApprovalState::Approved => ExitCode::Ok,
        ApprovalState::Pending => ExitCode::JobFailed,
        ApprovalState::Declined | ApprovalState::Expired | ApprovalState::Superseded => {
            output::progress(&format!(
                "Not approved: the request was {}.",
                view.state.label()
            ));
            ExitCode::JobFailed
        }
    }
}

fn failed(request_id: Uuid, waited: Waited) -> ExitCode {
    match waited {
        Waited::Failed(CtError::Api { status: 404, .. }) => {
            output::eprintln_error(&format!("approval request not found: {request_id}"));
            ExitCode::JobFailed
        }
        Waited::Failed(CtError::Timeout(_)) => {
            output::progress(&format!(
                "Timed out. The request is still pending; resume with: cloudthinker approvals watch {request_id}"
            ));
            ExitCode::Timeout
        }
        Waited::Failed(err) => exit::report(&err),
        Waited::Interrupted => {
            output::progress(&format!(
                "Stopped watching; the request stays open. Resume with: cloudthinker approvals watch {request_id}"
            ));
            ExitCode::Interrupted
        }
    }
}

enum Waited {
    Failed(CtError),
    Interrupted,
}

async fn wait_for_decision(
    client: &CtClient,
    request_id: Uuid,
    timing: WatchTiming,
    interrupted: Interrupted,
) -> Result<ApprovalView, Waited> {
    let cfg = timing.config();
    tokio::select! {
        outcome = watch(
            async || {
                let view = client.get_approval(request_id).await?;
                if view.state.is_closed() {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn watch_timing_polls_at_a_fixed_clamped_interval() {
        let cfg = WatchTiming {
            timeout_secs: 60,
            interval_secs: 7,
        }
        .config();
        assert_eq!(cfg.base_delay, Duration::from_secs(7));
        assert_eq!(cfg.max_delay, Duration::from_secs(7));
        assert!((cfg.factor - 1.0).abs() < f64::EPSILON);
        assert_eq!(cfg.overall_timeout, Duration::from_secs(60));
        let floor = WatchTiming {
            timeout_secs: 60,
            interval_secs: 0,
        }
        .config();
        assert_eq!(floor.base_delay, Duration::from_secs(MIN_POLL_SECS));
    }
}
