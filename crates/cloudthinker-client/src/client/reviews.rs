use serde::Serialize;

use super::CtClient;
use crate::error::CtResult;
use crate::review_url::{MrCoordinates, MrProvider};

/// Terminal + non-terminal review states, serialized with the API's wire
/// values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewStatus {
    Queued,
    InReview,
    ReviewComplete,
    Filtered,
    Failed,
    Stopped,
}

impl ReviewStatus {
    /// True once the review has reached a state that will not change on its
    /// own (`review_complete`, `filtered`, `failed`, or `stopped`).
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::ReviewComplete | Self::Filtered | Self::Failed | Self::Stopped
        )
    }
}

impl From<cloudthinker_api::types::ReviewStatus> for ReviewStatus {
    fn from(status: cloudthinker_api::types::ReviewStatus) -> Self {
        use cloudthinker_api::types::ReviewStatus as A;
        match status {
            A::Queued => Self::Queued,
            A::InReview => Self::InReview,
            A::ReviewComplete => Self::ReviewComplete,
            A::Filtered => Self::Filtered,
            A::Failed => Self::Failed,
            A::Stopped => Self::Stopped,
        }
    }
}

/// The review's overall verdict — advisory display, never drives the exit
/// code (only `ReviewStatus::Failed` does, CA-RV-3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewVerdict {
    InReview,
    Approved,
    ReviewSuggested,
    ChangesRequested,
    Failed,
    Stopped,
    Filtered,
}

impl From<cloudthinker_api::types::CodeReviewOverviewVerdict> for ReviewVerdict {
    fn from(verdict: cloudthinker_api::types::CodeReviewOverviewVerdict) -> Self {
        use cloudthinker_api::types::CodeReviewOverviewVerdict as A;
        match verdict {
            A::InReview => Self::InReview,
            A::Approved => Self::Approved,
            A::ReviewSuggested => Self::ReviewSuggested,
            A::ChangesRequested => Self::ChangesRequested,
            A::Failed => Self::Failed,
            A::Stopped => Self::Stopped,
            A::Filtered => Self::Filtered,
        }
    }
}

/// Unresolved finding counts per severity (mirrors the review-summary panel).
#[derive(Debug, Clone, Copy, Serialize)]
pub struct ReviewSeverityCounts {
    pub critical: i64,
    pub high: i64,
    pub medium: i64,
    pub low: i64,
}

impl From<cloudthinker_api::types::CodeReviewSeverityCounts> for ReviewSeverityCounts {
    fn from(api: cloudthinker_api::types::CodeReviewSeverityCounts) -> Self {
        Self {
            critical: api.critical,
            high: api.high,
            medium: api.medium,
            low: api.low,
        }
    }
}

/// A single finding on the reviewed merge request.
#[derive(Debug, Clone, Serialize)]
pub struct ReviewFinding {
    pub severity: String,
    pub file_path: Option<String>,
    pub line_number: Option<i64>,
    pub issue_title: String,
    pub category: Option<String>,
    pub resolved: bool,
}

impl From<cloudthinker_api::types::CodeReviewDetailFinding> for ReviewFinding {
    fn from(api: cloudthinker_api::types::CodeReviewDetailFinding) -> Self {
        Self {
            severity: api.severity,
            file_path: api.file_path,
            line_number: api.line_number,
            issue_title: api.issue_title,
            category: api.category,
            resolved: api.resolved,
        }
    }
}

/// Rank a severity string worst-first for sorting (`findings`, CA-RV-2); an
/// unrecognized value sorts last rather than panicking.
fn severity_rank(severity: &str) -> u8 {
    match severity {
        "critical" => 0,
        "high" => 1,
        "medium" => 2,
        "low" => 3,
        _ => 4,
    }
}

/// A resolved review detail (`GET /code-review/merge-requests/lookup`).
#[derive(Debug, Clone)]
pub struct ReviewView {
    pub mr_iid: i64,
    pub status: ReviewStatus,
    pub verdict: ReviewVerdict,
    pub findings_count: i64,
    pub title: String,
    pub url: Option<String>,
    pub repository_path: Option<String>,
    pub provider: String,
    pub severity_counts: ReviewSeverityCounts,
    /// Worst-severity-first (CA-RV-2).
    pub findings: Vec<ReviewFinding>,
}

impl ReviewView {
    fn from_api(api: cloudthinker_api::types::CodeReviewMergeRequestDetail) -> Self {
        let mut findings: Vec<ReviewFinding> =
            api.findings.into_iter().map(ReviewFinding::from).collect();
        findings.sort_by_key(|f| severity_rank(&f.severity));
        Self {
            mr_iid: api.mr_iid,
            status: api.review_status.into(),
            verdict: api.verdict.into(),
            findings_count: api.findings_count,
            title: api.title,
            url: api.url,
            repository_path: api.repository_path,
            provider: api.provider.to_string(),
            severity_counts: api.severity_counts.into(),
            findings,
        }
    }
}

fn to_api_provider(provider: MrProvider) -> cloudthinker_api::types::CodeReviewProvider {
    match provider {
        MrProvider::Gitlab => cloudthinker_api::types::CodeReviewProvider::Gitlab,
        MrProvider::Github => cloudthinker_api::types::CodeReviewProvider::Github,
    }
}

impl CtClient {
    /// Resolve MR coordinates (parsed client-side from a pasted URL) to their
    /// tracked review detail. Unknown coordinates surface as
    /// `CtError::Api { status: 404, .. }`.
    pub async fn lookup_review(&self, coords: &MrCoordinates) -> CtResult<ReviewView> {
        let provider = to_api_provider(coords.provider);
        let mr_iid = coords.mr_iid;
        let project_path = coords.project_path.as_str();
        let view = self
            .authed(async |c: cloudthinker_api::Client| {
                c.code_review_lookup_code_review_merge_request(mr_iid, project_path, provider, None)
                    .await
            })
            .await?;
        Ok(ReviewView::from_api(view))
    }
}
