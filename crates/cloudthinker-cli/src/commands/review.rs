//! `cloudthinker review` — inspect or watch a tracked code review by pasting
//! its GitLab/GitHub merge-request URL. Never triggers a review; reads one
//! already tracked server-side (`plans/product-cli-mr-f-review.md`).

use std::time::Duration;

use cloudthinker_client::{CtError, ReviewStatus, ReviewView, parse_mr_url};

use crate::engine::exit::{self, ExitCode};
use crate::engine::output::{self, ReviewEnvelope};
use crate::engine::watch::{Poll, WatchConfig, watch};

use super::build_client;

/// Show a review's current status. A read: exits 0 on any successful fetch —
/// the review's own status/verdict is advisory, not failure (CA-RV-SP5,
/// mirrors `chat status` CA-CLI-16).
pub async fn run_status(
    base_url: &str,
    workspace: Option<&str>,
    url: &str,
    json: bool,
) -> ExitCode {
    fetch_and_render(
        base_url,
        workspace,
        url,
        json,
        output::print_review_status_summary,
    )
    .await
}

/// List a review's findings, worst-severity first (CA-RV-2). Also a read
/// (CA-RV-SP5).
pub async fn run_findings(
    base_url: &str,
    workspace: Option<&str>,
    url: &str,
    json: bool,
) -> ExitCode {
    fetch_and_render(
        base_url,
        workspace,
        url,
        json,
        output::print_review_findings,
    )
    .await
}

/// Poll a review to a terminal state, printing the final verdict.
///
/// Exit 0 on any terminal outcome except `ReviewStatus::Failed` (exit 1,
/// CA-RV-3). A client-side deadline prints a resume hint and exits 4
/// (CA-RV-SP6); the review keeps running server-side.
pub async fn run_watch(
    base_url: &str,
    workspace: Option<&str>,
    url: &str,
    json: bool,
    timeout_secs: u64,
) -> ExitCode {
    let coords = match parse_mr_url(url) {
        Ok(coords) => coords,
        Err(err) => return exit::report(&err),
    };
    let client = match build_client(base_url, workspace) {
        Ok(client) => client,
        Err(err) => return exit::report(&err),
    };

    if !json {
        output::progress(&format!("Watching review for {url}…"));
    }

    let cfg = WatchConfig::for_run(Duration::from_secs(timeout_secs));
    let outcome = watch(
        async || {
            let view = client.lookup_review(&coords).await?;
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
        Ok(view) => finish_watch(&view, json),
        Err(CtError::Timeout(_)) => {
            output::progress(&format!(
                "Timed out. The review continues server-side — resume with: cloudthinker review status {url}"
            ));
            ExitCode::Timeout
        }
        Err(err) => report_lookup_error(&err, url),
    }
}

/// Shared body for `status`/`findings`: parse the URL, fetch once, render.
async fn fetch_and_render(
    base_url: &str,
    workspace: Option<&str>,
    url: &str,
    json: bool,
    human: fn(&ReviewView) -> Result<(), String>,
) -> ExitCode {
    let coords = match parse_mr_url(url) {
        Ok(coords) => coords,
        Err(err) => return exit::report(&err),
    };
    let client = match build_client(base_url, workspace) {
        Ok(client) => client,
        Err(err) => return exit::report(&err),
    };

    match client.lookup_review(&coords).await {
        Ok(view) => render(&view, json, human),
        Err(err) => report_lookup_error(&err, url),
    }
}

fn render(view: &ReviewView, json: bool, human: fn(&ReviewView) -> Result<(), String>) -> ExitCode {
    let result = if json {
        output::emit_json(&ReviewEnvelope::from_view(view))
    } else {
        human(view)
    };
    if let Err(err) = result {
        output::eprintln_error(&err);
        return ExitCode::JobFailed;
    }
    ExitCode::Ok
}

fn finish_watch(view: &ReviewView, json: bool) -> ExitCode {
    let result = if json {
        output::emit_json(&ReviewEnvelope::from_view(view))
    } else {
        output::print_review_status_summary(view)
    };
    if let Err(err) = result {
        output::eprintln_error(&err);
        return ExitCode::JobFailed;
    }
    match view.status {
        ReviewStatus::Failed => ExitCode::JobFailed,
        _ => ExitCode::Ok,
    }
}

/// A 404 (unknown coordinates) gets a review-specific message; everything
/// else goes through the standard error → exit-code mapping (CA-RV-SP4,
/// CA-RV-SP2, CA-RV-SP3).
fn report_lookup_error(err: &CtError, url: &str) -> ExitCode {
    match err {
        CtError::Api { status: 404, .. } => {
            output::eprintln_error(&format!("no code review found for {url}"));
            ExitCode::JobFailed
        }
        other => exit::report(other),
    }
}
