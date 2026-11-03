use serde::Serialize;

use super::CtClient;
use crate::error::{CtError, CtResult};
use crate::review_url::{MrCoordinates, MrProvider};

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LoupeReadinessState {
    Off,
    Ready,
    NeedsSetup,
    Busy,
    Offline,
    NoAnswer,
    Unavailable,
}

impl From<cloudthinker_api::types::LoupeReadinessState> for LoupeReadinessState {
    fn from(value: cloudthinker_api::types::LoupeReadinessState) -> Self {
        use cloudthinker_api::types::LoupeReadinessState as State;
        match value {
            State::Off => Self::Off,
            State::Ready => Self::Ready,
            State::NeedsSetup => Self::NeedsSetup,
            State::Busy => Self::Busy,
            State::Offline => Self::Offline,
            State::NoAnswer => Self::NoAnswer,
            State::Unavailable => Self::Unavailable,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct LoupeReadinessCheck {
    pub name: LoupeReadinessCheckName,
    pub passed: bool,
    pub required: bool,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LoupeReadinessCheckName {
    Checkout,
    Node,
    Bun,
    Playwright,
    Chromium,
    SignIn,
    DevEnv,
    AppRuntime,
    Sandbox,
}

impl From<cloudthinker_api::types::LoupeReadinessCheckName> for LoupeReadinessCheckName {
    fn from(value: cloudthinker_api::types::LoupeReadinessCheckName) -> Self {
        use cloudthinker_api::types::LoupeReadinessCheckName as Name;
        match value {
            Name::Checkout => Self::Checkout,
            Name::Node => Self::Node,
            Name::Bun => Self::Bun,
            Name::Playwright => Self::Playwright,
            Name::Chromium => Self::Chromium,
            Name::SignIn => Self::SignIn,
            Name::DevEnv => Self::DevEnv,
            Name::AppRuntime => Self::AppRuntime,
            Name::Sandbox => Self::Sandbox,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct LoupeReadiness {
    pub state: LoupeReadinessState,
    pub target_id: Option<uuid::Uuid>,
    pub repository: Option<String>,
    pub detail: Option<String>,
    pub checks: Vec<LoupeReadinessCheck>,
    pub observed: Option<LoupeSetupObservation>,
}

pub struct LoupeSetupScope<'a> {
    pub worker: uuid::Uuid,
    pub connection: uuid::Uuid,
    pub provider: &'a str,
    pub repository: &'a str,
}

#[derive(Debug, Clone, Serialize)]
pub struct LoupeSetupConversation {
    pub conversation_id: uuid::Uuid,
    pub needs_initial_message: bool,
    pub web_url: String,
}

pub fn valid_setup_web_url(value: &str) -> bool {
    url::Url::parse(value).is_ok_and(|url| {
        matches!(url.scheme(), "http" | "https")
            && url.host_str().is_some()
            && url.username().is_empty()
            && url.password().is_none()
    })
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LoupeSetupState {
    NotChecked,
    Partial,
    Full,
}

impl std::fmt::Display for LoupeSetupState {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::NotChecked => "not_checked",
            Self::Partial => "partial",
            Self::Full => "full",
        })
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct LoupeSetupObservation {
    pub state: LoupeSetupState,
    pub provider: String,
    pub repository: String,
    pub target_id: uuid::Uuid,
    pub connection_id: uuid::Uuid,
    pub checked_at: Option<chrono::DateTime<chrono::Utc>>,
    pub generation: Option<uuid::Uuid>,
    pub check_pending: bool,
    pub has_saved_observations: bool,
    pub enabled: bool,
    pub partial_accepted: bool,
    pub detail: String,
}

impl From<cloudthinker_api::types::LoupeSetupReadinessView> for LoupeSetupObservation {
    fn from(value: cloudthinker_api::types::LoupeSetupReadinessView) -> Self {
        use cloudthinker_api::types::LoupeSetupReadinessViewState as State;
        Self {
            state: match value.state {
                State::NotChecked => LoupeSetupState::NotChecked,
                State::Partial => LoupeSetupState::Partial,
                State::Full => LoupeSetupState::Full,
            },
            provider: value.provider.to_string(),
            repository: value.repository,
            target_id: value.target_id,
            connection_id: value.connection_id,
            checked_at: value.checked_at,
            generation: value.generation,
            check_pending: value.check_pending,
            has_saved_observations: value.has_saved_observations,
            enabled: value.enabled,
            partial_accepted: value.partial_accepted,
            detail: value.detail,
        }
    }
}

impl From<cloudthinker_api::types::LoupeReadinessResponse> for LoupeReadiness {
    fn from(value: cloudthinker_api::types::LoupeReadinessResponse) -> Self {
        Self {
            state: value.state.into(),
            target_id: value.target_id,
            repository: value.repository,
            detail: value.detail,
            observed: value.observed.map(Into::into),
            checks: value
                .checks
                .into_iter()
                .map(|check| LoupeReadinessCheck {
                    name: check.name.into(),
                    passed: check.passed,
                    required: check.required,
                    detail: check.detail,
                })
                .collect(),
        }
    }
}

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
    pub async fn start_loupe_setup_conversation(
        &self,
        worker: uuid::Uuid,
    ) -> CtResult<LoupeSetupConversation> {
        let request = cloudthinker_api::types::LoupeSetupConversationRequest {
            target_id: worker,
            restart: false,
        };
        let response = self
            .authed(async |client: cloudthinker_api::Client| {
                client
                    .code_review_start_loupe_setup_conversation(None, None, &request)
                    .await
            })
            .await?;
        Ok(LoupeSetupConversation {
            conversation_id: response.conversation_id,
            needs_initial_message: response.needs_initial_message,
            web_url: response.web_url,
        })
    }

    pub async fn check_loupe_readiness(
        &self,
        worker: Option<uuid::Uuid>,
        scope: Option<&LoupeSetupScope<'_>>,
    ) -> CtResult<LoupeReadiness> {
        let provider = scope
            .map(|scope| cloudthinker_api::types::CodeReviewProvider::try_from(scope.provider))
            .transpose()
            .map_err(|error| CtError::Usage(format!("invalid Review provider: {error}")))?;
        let repository = scope
            .map(|scope| cloudthinker_api::types::Repository::try_from(scope.repository))
            .transpose()
            .map_err(|error| CtError::Usage(format!("invalid repository: {error}")))?;
        if scope.is_some_and(|scope| Some(scope.worker) != worker) {
            return Err(CtError::Usage(
                "the scope must use the selected Worker".into(),
            ));
        }
        let response = self
            .authed(async |client: cloudthinker_api::Client| {
                client
                    .code_review_check_loupe_target(
                        scope.map(|scope| &scope.connection),
                        provider,
                        repository.as_ref(),
                        worker.as_ref(),
                    )
                    .await
            })
            .await?;
        Ok(response.into())
    }

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
