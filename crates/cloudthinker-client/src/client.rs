//! `CtClient` — the typed wire surface every command calls.
//!
//! Wraps the generated `cloudthinker_api::Client`, injects the bearer token,
//! and owns the 401-retry / proactive-refresh dance so commands never touch
//! reqwest, serde, or refresh logic directly.

use std::num::NonZeroU64;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::Serialize;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::auth::refresh::RefreshCoordinator;
use crate::auth::store::{
    CredentialProvenance, EnvTokenStore, FileStore, StoredToken, TOKEN_ENV_VAR, TokenStore,
    WorkspaceSelector, acquire_credential_lock,
};
use crate::error::{CtError, CtResult, to_ct_error};
use crate::retry::classify;
use crate::review_url::{MrCoordinates, MrProvider};

mod incidents;
mod memory;
mod recommendations;
pub use incidents::{IncidentStatus, IncidentView};
pub use memory::{CyberMemoryContextSource, CyberMemoryFile, CyberMemorySnapshot};
pub use recommendations::{RecommendationStatus, RecommendationView};

pub const DEFAULT_BASE_URL: &str = "https://app.cloudthinker.io";
const REQUEST_TIMEOUT_SECS: u64 = 30;
const MAX_OBSERVATIONS_PER_BATCH: usize = 500;
// One plan check against the target. Shorter than an API call: a hung target
// must not stall the whole plan, and a timeout is a truthful `blocked` row.
const PROBE_TIMEOUT_SECS: u64 = 20;

fn observation_batches<'a>(
    observations: &'a [Observation],
) -> impl Iterator<Item = &'a [Observation]> + 'a {
    observations.chunks(MAX_OBSERVATIONS_PER_BATCH)
}

fn validate_observation_ids(observations: &[Observation]) -> CtResult<()> {
    let mut row_ids = std::collections::HashSet::with_capacity(observations.len());
    for observation in observations {
        if observation.row_id.is_empty() {
            return Err(CtError::Usage(
                "observation row id must not be empty".to_string(),
            ));
        }
        if !row_ids.insert(observation.row_id.as_str()) {
            return Err(CtError::Usage(format!(
                "duplicate observation row id `{}`",
                observation.row_id
            )));
        }
    }
    Ok(())
}

/// A short-lived device authorization shown in another browser.
#[derive(Debug, Clone)]
pub struct DeviceAuthorization {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub expires_in: Duration,
    pub interval: Duration,
}

/// One response from the RFC 8628-style device token poll.
#[derive(Debug, Clone)]
pub enum DeviceTokenPoll {
    Pending(Duration),
    SlowDown(Duration),
    Unavailable(Duration),
    Token(StoredToken),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LogoutOutcome {
    Cleared,
    NothingStored,
}

#[derive(Debug, Clone, Serialize)]
pub struct CliIdentity {
    pub host: String,
    pub user_id: Uuid,
    pub user_email: String,
    pub workspace_id: Uuid,
    pub workspace_name: String,
}

/// Terminal + non-terminal run states, serialized with the API's wire values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Pending,
    Running,
    Succeeded,
    Failed,
    RequiredApproval,
}

impl RunStatus {
    /// True once the run has reached a state that will not change on its own.
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Succeeded | Self::Failed | Self::RequiredApproval
        )
    }
}

impl From<cloudthinker_api::types::AgentRunStatus> for RunStatus {
    fn from(status: cloudthinker_api::types::AgentRunStatus) -> Self {
        use cloudthinker_api::types::AgentRunStatus as A;
        match status {
            A::Pending => Self::Pending,
            A::Running => Self::Running,
            A::Succeeded => Self::Succeeded,
            A::Failed => Self::Failed,
            A::RequiredApproval => Self::RequiredApproval,
        }
    }
}

/// Result of `POST /cli/runs`.
#[derive(Debug, Clone)]
pub struct SubmittedRun {
    pub run_id: Uuid,
    pub conversation_id: Uuid,
    pub status: RunStatus,
    pub web_url: String,
}

impl SubmittedRun {
    fn from_api(api: cloudthinker_api::types::HeadlessRunSubmitted) -> Self {
        Self {
            run_id: api.run_id,
            conversation_id: api.conversation_id,
            status: api.status.into(),
            web_url: api.web_url,
        }
    }
}

/// One recent headless run returned by `GET /cli/runs`.
#[derive(Debug, Clone, Serialize)]
pub struct RunListItem {
    pub run_id: Uuid,
    pub conversation_id: Option<Uuid>,
    pub status: RunStatus,
    pub prompt_preview: Option<String>,
    pub created_at: DateTime<Utc>,
    pub web_url: Option<String>,
}

impl From<cloudthinker_api::types::HeadlessRunListItem> for RunListItem {
    fn from(api: cloudthinker_api::types::HeadlessRunListItem) -> Self {
        Self {
            run_id: api.run_id,
            conversation_id: api.conversation_id,
            status: api.status.into(),
            prompt_preview: api.prompt_preview,
            created_at: api.created_at,
            web_url: api.web_url,
        }
    }
}

/// A polled view of a run (`GET /cli/runs/{id}`).
#[derive(Debug, Clone)]
pub struct RunView {
    pub run_id: Uuid,
    pub conversation_id: Option<Uuid>,
    pub status: RunStatus,
    pub answer: Option<String>,
    pub message: Option<String>,
    pub failure_kind: Option<String>,
    pub web_url: Option<String>,
}

impl RunView {
    fn from_api(api: cloudthinker_api::types::HeadlessRunStatus) -> Self {
        Self {
            run_id: api.run_id,
            conversation_id: api.conversation_id,
            status: api.status.into(),
            answer: api.answer,
            message: api.message,
            failure_kind: api.failure_kind,
            web_url: api.web_url,
        }
    }
}

/// Lifecycle of an AppSec run — running | success | failed | cancelled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CyberRunResult {
    Running,
    Success,
    Failed,
    Cancelled,
}

impl From<cloudthinker_api::types::RunResult> for CyberRunResult {
    fn from(result: cloudthinker_api::types::RunResult) -> Self {
        use cloudthinker_api::types::RunResult as A;
        match result {
            A::Running => Self::Running,
            A::Success => Self::Success,
            A::Failed => Self::Failed,
            A::Cancelled => Self::Cancelled,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CyberExecutionHost {
    Cloud,
    Local,
}

impl From<cloudthinker_api::types::RunExecutionHost> for CyberExecutionHost {
    fn from(host: cloudthinker_api::types::RunExecutionHost) -> Self {
        match host {
            cloudthinker_api::types::RunExecutionHost::Cloud => Self::Cloud,
            cloudthinker_api::types::RunExecutionHost::Local => Self::Local,
        }
    }
}

/// One Cyber run, as `cloudthinker cyber run launch`/`cyber run status` see it.
#[derive(Debug, Clone, Serialize)]
pub struct CyberRun {
    pub run_id: Uuid,
    pub app_id: Uuid,
    pub result: CyberRunResult,
    pub execution_host: CyberExecutionHost,
    pub conversation_id: Option<Uuid>,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub findings_discovered: i64,
    pub findings_resolved: i64,
    pub findings_critical: i64,
    pub findings_confirmed: i64,
    pub findings_confirmed_critical: i64,
}

/// The auth posture a run declares. The local CLI derives it from context
/// (source and identities on this machine); the backend clamps it to the App's
/// allowed modes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CyberMode {
    Black,
    Gray,
    White,
}

impl CyberMode {
    fn into_api(self) -> cloudthinker_api::types::Mode {
        match self {
            CyberMode::Black => cloudthinker_api::types::Mode::Black,
            CyberMode::Gray => cloudthinker_api::types::Mode::Gray,
            CyberMode::White => cloudthinker_api::types::Mode::White,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            CyberMode::Black => "black",
            CyberMode::Gray => "gray",
            CyberMode::White => "white",
        }
    }
}

/// How far a probe may push. The engineer's choice, not derived.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CyberIntensity {
    Safe,
    Aggressive,
    Full,
}

impl CyberIntensity {
    fn into_api(self) -> cloudthinker_api::types::Intensity {
        match self {
            CyberIntensity::Safe => cloudthinker_api::types::Intensity::Safe,
            CyberIntensity::Aggressive => cloudthinker_api::types::Intensity::Aggressive,
            CyberIntensity::Full => cloudthinker_api::types::Intensity::Full,
        }
    }
}

/// The agent-owned axis of a finding, for filtering `finding ls`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CyberFindingStatus {
    Open,
    Resolved,
    Dismissed,
    NeedsVerification,
}

impl CyberFindingStatus {
    fn into_api(self) -> cloudthinker_api::types::FindingStatus {
        match self {
            CyberFindingStatus::Open => cloudthinker_api::types::FindingStatus::Open,
            CyberFindingStatus::Resolved => cloudthinker_api::types::FindingStatus::Resolved,
            CyberFindingStatus::Dismissed => cloudthinker_api::types::FindingStatus::Dismissed,
            CyberFindingStatus::NeedsVerification => {
                cloudthinker_api::types::FindingStatus::NeedsVerification
            }
        }
    }
}

/// The human-owned triage axis of a finding, for filtering `finding ls`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CyberTriageState {
    None,
    InProgress,
    AwaitingRetest,
}

impl CyberTriageState {
    fn into_api(self) -> cloudthinker_api::types::TriageState {
        match self {
            CyberTriageState::None => cloudthinker_api::types::TriageState::None,
            CyberTriageState::InProgress => cloudthinker_api::types::TriageState::InProgress,
            CyberTriageState::AwaitingRetest => {
                cloudthinker_api::types::TriageState::AwaitingRetest
            }
        }
    }
}

/// A finding severity, for filtering `finding ls`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CyberSeverity {
    Critical,
    High,
    Medium,
    Low,
    Info,
}

impl CyberSeverity {
    fn into_api(self) -> cloudthinker_api::types::Severity {
        match self {
            CyberSeverity::Critical => cloudthinker_api::types::Severity::Critical,
            CyberSeverity::High => cloudthinker_api::types::Severity::High,
            CyberSeverity::Medium => cloudthinker_api::types::Severity::Medium,
            CyberSeverity::Low => cloudthinker_api::types::Severity::Low,
            CyberSeverity::Info => cloudthinker_api::types::Severity::Info,
        }
    }
}

/// The optional filters for `finding ls`. An unset field means no filter, so an
/// empty `FindingFilter` reproduces the whole-page default.
#[derive(Debug, Clone, Copy, Default)]
pub struct FindingFilter {
    pub status: Option<CyberFindingStatus>,
    pub triage_state: Option<CyberTriageState>,
    pub severity: Option<CyberSeverity>,
}

/// An App available to the local Cyber setup flow.
#[derive(Debug, Clone, Serialize)]
pub struct CyberApp {
    pub app_id: Uuid,
    pub name: String,
    pub target_ref: String,
    pub setup_status: String,
    pub domain_status: String,
    pub domain_id: Option<Uuid>,
    pub open_findings_count: i64,
}

impl CyberApp {
    fn from_api(api: cloudthinker_api::types::AppPublic) -> Self {
        Self {
            app_id: api.id,
            name: api.name,
            target_ref: api.target_ref,
            setup_status: api.setup_status.to_string(),
            domain_status: api.domain_verification.status.to_string(),
            domain_id: api.domain_verification.domain_id,
            open_findings_count: api.open_findings_count,
        }
    }
}

/// A finding read from the terminal Cyber surface.
#[derive(Debug, Clone, Serialize)]
pub struct CyberFinding {
    pub finding_id: Uuid,
    pub display_id: Option<String>,
    pub app_id: Uuid,
    pub title: String,
    pub finding_type: String,
    pub severity: String,
    pub status: String,
    pub triage_state: String,
    pub description: String,
    pub owasp: Option<String>,
    pub cwe: Option<String>,
    pub cve: Option<String>,
    pub evidence: Vec<String>,
}

impl CyberFinding {
    fn from_api(api: cloudthinker_api::types::FindingPublic) -> Self {
        Self {
            finding_id: api.id,
            display_id: api.display_id,
            app_id: api.app_id,
            title: api.title,
            finding_type: api.finding_type,
            severity: api.severity.to_string(),
            status: api.status.to_string(),
            triage_state: api
                .triage_state
                .map_or_else(|| "none".to_string(), |state| state.to_string()),
            description: api.description_md,
            owasp: api.owasp,
            cwe: api.cwe,
            cve: api.cve,
            evidence: api.evidence.into_iter().map(|item| item.file).collect(),
        }
    }
}

/// A paginated finding page.
#[derive(Debug, Clone, Serialize)]
pub struct CyberFindingPage {
    pub data: Vec<CyberFinding>,
    pub page: i64,
    pub take: i64,
    pub total: i64,
    pub pages: i64,
}

/// A backend-issued PDF download receipt.
#[derive(Debug, Clone, Serialize)]
pub struct CyberExport {
    pub download_url: String,
    pub filename: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct CyberDomain {
    pub domain_id: Uuid,
    pub domain: String,
    pub status: String,
    pub dns_record_name: Option<String>,
    pub dns_record_value: Option<String>,
}

impl CyberDomain {
    fn from_api(value: cloudthinker_api::types::AppSecDomainPublic) -> Self {
        Self {
            domain_id: value.id,
            domain: value.domain,
            status: value.status.to_string(),
            dns_record_name: value.dns_record_name,
            dns_record_value: value.dns_record_value,
        }
    }
}

impl CyberRun {
    fn from_api(api: cloudthinker_api::types::RunPublic) -> Self {
        Self {
            run_id: api.id,
            app_id: api.app_id,
            result: api.result.into(),
            execution_host: api
                .execution_host
                .map(CyberExecutionHost::from)
                .unwrap_or(CyberExecutionHost::Cloud),
            conversation_id: api.conversation_id,
            started_at: api.started_at,
            finished_at: api.finished_at,
            findings_discovered: api.findings_discovered,
            findings_resolved: api.findings_resolved,
            findings_critical: api.findings_critical,
            findings_confirmed: api.findings_confirmed,
            findings_confirmed_critical: api.findings_confirmed_critical,
        }
    }

    /// True once the run has reached a terminal state.
    pub fn is_terminal(&self) -> bool {
        !matches!(self.result, CyberRunResult::Running)
    }
}

/// One mirrored pi entry, as `cloudthinker cyber run session` reads it back. The
/// payload is pi's own entry verbatim; the CLI renders the shapes it knows and
/// leaves the rest alone.
#[derive(Debug, Clone, Serialize)]
pub struct CyberSessionEntry {
    pub seq: i64,
    pub entry_id: String,
    pub parent_id: Option<String>,
    pub entry_type: String,
    pub payload: serde_json::Value,
}

#[derive(Debug, Clone, Serialize)]
pub struct CyberRunBrief {
    pub run_id: Uuid,
    pub app_id: Uuid,
    pub app_name: String,
    pub conversation_id: Option<Uuid>,
    pub result: CyberRunResult,
    pub execution_host: CyberExecutionHost,
    pub target: String,
    pub frameworks: Vec<String>,
    pub mode: String,
    pub intensity: String,
    pub scan_mode: String,
    pub report_preferences: cloudthinker_api::types::AppSecReportPreferences,
    pub report_reference: Option<cloudthinker_api::types::LocalRunReportReferencePublic>,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub targets: Vec<CyberDiscoveryTarget>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local_workspace: Option<LocalCyberWorkspace>,
}

#[derive(Debug, Clone, Serialize)]
pub struct LocalCyberWorkspace {
    pub workspace_root: String,
    pub evidence_root: String,
    pub workflow_root: String,
    pub manifest: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct CyberDiscoveryTarget {
    pub target_id: Uuid,
    pub target_ref: String,
    pub safety_scope: cloudthinker_api::types::ScopeSpec,
    pub run_scope: cloudthinker_api::types::ScopeSpec,
}

impl From<cloudthinker_api::types::CyberDiscoveryTargetPublic> for CyberDiscoveryTarget {
    fn from(value: cloudthinker_api::types::CyberDiscoveryTargetPublic) -> Self {
        Self {
            target_id: value.target_id,
            target_ref: value.target_ref,
            safety_scope: value.safety_scope,
            run_scope: value.run_scope,
        }
    }
}

impl CyberRunBrief {
    fn from_api(api: cloudthinker_api::types::LocalRunBriefPublic) -> Self {
        Self {
            run_id: api.run_id,
            app_id: api.app_id,
            app_name: api.app_name,
            conversation_id: api.conversation_id,
            result: api.result.into(),
            execution_host: api.execution_host.into(),
            target: api.target,
            frameworks: api
                .frameworks
                .iter()
                .map(::std::string::ToString::to_string)
                .collect(),
            mode: api.mode.to_string(),
            intensity: api.intensity,
            scan_mode: api.scan_mode.to_string(),
            report_preferences: api.report_preferences,
            report_reference: api.report_reference,
            started_at: api.started_at,
            finished_at: api.finished_at,
            targets: api.targets.into_iter().map(Into::into).collect(),
            local_workspace: None,
        }
    }
}

/// One local evidence file, read as text and uploaded by path.
#[derive(Debug, Clone)]
pub struct EvidenceFile {
    pub path: String,
    pub content: Option<String>,
    pub content_base64: Option<String>,
    pub mime_type: Option<String>,
    pub size_bytes: u64,
}

/// Where the run's durable evidence tree mounts inside the agent sandbox.
#[derive(Debug, Clone, Serialize)]
pub struct EvidenceSkipped {
    pub path: String,
    pub reason: String,
}

/// What the backend actually persisted from one evidence submission.
#[derive(Debug, Clone, Serialize)]
pub struct EvidenceReceipt {
    pub written: Vec<String>,
    pub skipped: Vec<EvidenceSkipped>,
}

/// Whether this call's settle won the finalize CAS.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct SettleResult {
    pub settled: bool,
}

/// Row status in the backend-owned coverage ledger.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CoverageStatus {
    Untested,
    Assigned,
    Covered,
    Candidate,
    CandidatePromoted,
    CandidateDismissed,
    CandidateNeedsVerification,
    Blocked,
    SkippedWithReason,
}

impl CoverageStatus {
    fn to_api(self) -> cloudthinker_api::types::CoverageStatus {
        use cloudthinker_api::types::CoverageStatus as A;
        match self {
            Self::Untested => A::Untested,
            Self::Assigned => A::Assigned,
            Self::Covered => A::Covered,
            Self::Candidate => A::Candidate,
            Self::CandidatePromoted => A::CandidatePromoted,
            Self::CandidateDismissed => A::CandidateDismissed,
            Self::CandidateNeedsVerification => A::CandidateNeedsVerification,
            Self::Blocked => A::Blocked,
            Self::SkippedWithReason => A::SkippedWithReason,
        }
    }

    /// Parse the snake_case wire value a scout passes to `cyber probe ingest`,
    /// so a command never hardcodes the coverage vocabulary.
    pub fn from_wire(value: &str) -> Option<Self> {
        Some(match value {
            "untested" => Self::Untested,
            "assigned" => Self::Assigned,
            "covered" => Self::Covered,
            "candidate" => Self::Candidate,
            "candidate_promoted" => Self::CandidatePromoted,
            "candidate_dismissed" => Self::CandidateDismissed,
            "candidate_needs_verification" => Self::CandidateNeedsVerification,
            "blocked" => Self::Blocked,
            "skipped_with_reason" => Self::SkippedWithReason,
            _ => return None,
        })
    }

    pub fn requires_reason(self) -> bool {
        matches!(
            self,
            Self::Candidate | Self::Blocked | Self::SkippedWithReason
        )
    }

    fn from_api(status: cloudthinker_api::types::CoverageStatus) -> Self {
        use cloudthinker_api::types::CoverageStatus as A;
        match status {
            A::Untested => Self::Untested,
            A::Assigned => Self::Assigned,
            A::Covered => Self::Covered,
            A::Candidate => Self::Candidate,
            A::CandidatePromoted => Self::CandidatePromoted,
            A::CandidateDismissed => Self::CandidateDismissed,
            A::CandidateNeedsVerification => Self::CandidateNeedsVerification,
            A::Blocked => Self::Blocked,
            A::SkippedWithReason => Self::SkippedWithReason,
        }
    }
}

/// One check the backend wants the host to run.
#[derive(Debug, Clone, Serialize)]
pub struct PlanCheck {
    pub row_id: String,
    pub asset_type: String,
    pub locator: String,
    pub method: String,
    pub url: String,
    pub executable: bool,
    pub note: String,
}

/// The backend-issued plan: the whole bounded to-do list for one run.
#[derive(Debug, Clone, Serialize)]
pub struct WorkPlan {
    pub plan_id: String,
    pub run_id: Uuid,
    pub target_ref: String,
    pub schema_version: i64,
    pub rows: Vec<PlanCheck>,
}

impl WorkPlan {
    fn from_api(api: cloudthinker_api::types::WorkPlanPublic) -> Self {
        Self {
            plan_id: api.plan_id,
            run_id: api.run_id,
            target_ref: api.target_ref,
            schema_version: api.schema_version,
            rows: api
                .rows
                .into_iter()
                .map(|row| PlanCheck {
                    row_id: row.row_id,
                    asset_type: row.asset_type,
                    locator: row.locator,
                    method: row.method,
                    url: row.url,
                    executable: row.executable,
                    note: row.note,
                })
                .collect(),
        }
    }
}

/// The OWASP attack-surface overview an agent authors themes against.
#[derive(Debug, Clone, Serialize)]
pub struct Surface {
    pub plan_id: String,
    pub surface: serde_json::Value,
}

impl Surface {
    fn from_api(api: cloudthinker_api::types::SurfacePublic) -> Self {
        Self {
            plan_id: api.plan_id,
            surface: serde_json::Value::Object(api.surface),
        }
    }
}

/// One theme lane: the disjoint rows a single scout owns for the run.
#[derive(Debug, Clone, Serialize)]
pub struct Shard {
    pub shard_id: String,
    pub shard_key: String,
    pub focus: String,
    pub row_ids: Vec<String>,
    pub locators: Vec<String>,
    pub auth_context_ref: String,
    pub auth_context_refs: Vec<String>,
}

/// The surface plus the theme lanes the run fans scouts out to.
#[derive(Debug, Clone, Serialize)]
pub struct Partition {
    pub plan_id: String,
    pub surface: serde_json::Value,
    pub axis: String,
    pub shards: Vec<Shard>,
    pub notes: Vec<String>,
    pub unmatched: i64,
    pub unmatched_pct: i64,
}

impl Partition {
    fn from_api(api: cloudthinker_api::types::PartitionPublic) -> Self {
        Self {
            plan_id: api.plan_id,
            surface: serde_json::Value::Object(api.surface),
            axis: api.axis,
            shards: api
                .shards
                .into_iter()
                .map(|s| Shard {
                    shard_id: s.shard_id,
                    shard_key: s.shard_key,
                    focus: s.focus,
                    row_ids: s.row_ids,
                    locators: s.locators,
                    auth_context_ref: s.auth_context_ref,
                    auth_context_refs: s.auth_context_refs,
                })
                .collect(),
            notes: api.notes,
            unmatched: api.unmatched,
            unmatched_pct: api.unmatched_pct,
        }
    }
}

/// One row of the coverage ledger, as read back.
#[derive(Debug, Clone, Serialize)]
pub struct CoverageRow {
    pub row_id: String,
    pub asset_type: String,
    pub locator: String,
    pub method: String,
    pub url: String,
    pub executable: bool,
    pub status: CoverageStatus,
    pub worker: String,
    pub reason: String,
    pub evidence_ref: String,
}

/// The run-completion gate over one plan's rows.
#[derive(Debug, Clone, Serialize)]
pub struct Coverage {
    pub plan_id: Option<String>,
    pub total: i64,
    pub terminal: i64,
    pub valid: bool,
    pub blockers: Vec<String>,
    pub untested: Vec<String>,
    pub unresolved_candidates: Vec<String>,
}

/// The gate plus every row: the ingest result and the coverage read.
#[derive(Debug, Clone, Serialize)]
pub struct CoverageReport {
    pub coverage: Coverage,
    pub rows: Vec<CoverageRow>,
}

impl CoverageReport {
    fn from_api(api: cloudthinker_api::types::CoverageReportPublic) -> Self {
        Self {
            coverage: Coverage {
                plan_id: api.coverage.plan_id,
                total: api.coverage.total,
                terminal: api.coverage.terminal,
                valid: api.coverage.valid,
                blockers: api.coverage.blockers,
                untested: api.coverage.untested,
                unresolved_candidates: api.coverage.unresolved_candidates,
            },
            rows: api
                .rows
                .into_iter()
                .map(|row| CoverageRow {
                    row_id: row.row_id,
                    asset_type: row.asset_type,
                    locator: row.locator,
                    method: row.method,
                    url: row.url,
                    executable: row.executable,
                    status: CoverageStatus::from_api(row.status),
                    worker: row.worker,
                    reason: row.reason,
                    evidence_ref: row.evidence_ref,
                })
                .collect(),
        }
    }
}

/// One row's observed outcome, reported back to the backend authority.
#[derive(Debug, Clone)]
pub struct Observation {
    pub row_id: String,
    pub status: CoverageStatus,
    pub reason: String,
    pub evidence_ref: String,
    pub worker: String,
}

/// The outcome of running one check locally against the target.
#[derive(Debug, Clone, Serialize)]
pub struct ProbeOutcome {
    pub status: Option<u16>,
    pub error: Option<String>,
}

impl ProbeOutcome {
    /// A row is covered when the target answered at all; a transport failure
    /// is blocked with its reason, never silently covered.
    pub fn to_observation(&self, row_id: &str, worker: &str, evidence_ref: &str) -> Observation {
        match (self.status, &self.error) {
            (Some(_), _) => Observation {
                row_id: row_id.to_string(),
                status: CoverageStatus::Covered,
                reason: String::new(),
                evidence_ref: evidence_ref.to_string(),
                worker: worker.to_string(),
            },
            (None, error) => Observation {
                row_id: row_id.to_string(),
                status: CoverageStatus::Blocked,
                reason: error
                    .clone()
                    .unwrap_or_else(|| "request failed".to_string()),
                evidence_ref: evidence_ref.to_string(),
                worker: worker.to_string(),
            },
        }
    }
}

/// Terminal + non-terminal review states, serialized with the API's wire
/// values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewStatus {
    InReview,
    ReviewComplete,
    Filtered,
    Failed,
}

impl ReviewStatus {
    /// True once the review has reached a state that will not change on its
    /// own (`review_complete`, `filtered`, or `failed`).
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::ReviewComplete | Self::Filtered | Self::Failed)
    }
}

impl From<cloudthinker_api::types::ReviewStatus> for ReviewStatus {
    fn from(status: cloudthinker_api::types::ReviewStatus) -> Self {
        use cloudthinker_api::types::ReviewStatus as A;
        match status {
            A::InReview => Self::InReview,
            A::ReviewComplete => Self::ReviewComplete,
            A::Filtered => Self::Filtered,
            A::Failed => Self::Failed,
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

/// The authenticated client. Cheap to clone-share via `Arc`.
pub struct CtClient {
    base_url: String,
    store: Arc<dyn TokenStore>,
    refresh: RefreshCoordinator,
    // Reqwest pools connections; rebuild only when the bearer token rotates so
    // polling reuses one TLS connection across the run.
    http_cache: Mutex<Option<(String, Duration, reqwest::Client)>>,
    // Anonymous client for endpoints that carry no bearer (exchange, refresh,
    // best-effort logout).
    anon_http: reqwest::Client,
    probe_http: reqwest::Client,
    rejected_credential: Mutex<Option<[u8; 32]>>,
}

impl CtClient {
    pub fn new(base_url: impl Into<String>, store: Arc<dyn TokenStore>) -> CtResult<Self> {
        let base_url = normalize_base_url(&base_url.into())?;
        let timeout = Duration::from_secs(REQUEST_TIMEOUT_SECS);
        let anon_http = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .map_err(|e| CtError::Transport(format!("http client build: {e}")))?;
        let probe_http = reqwest::Client::builder()
            .timeout(timeout)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| CtError::Transport(format!("probe client build: {e}")))?;
        let refresh = RefreshCoordinator::new(base_url.clone(), store.clone(), anon_http.clone());
        Ok(Self {
            base_url,
            store,
            refresh,
            http_cache: Mutex::new(None),
            anon_http,
            probe_http,
            rejected_credential: Mutex::new(None),
        })
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub fn credential_provenance(&self) -> CtResult<CredentialProvenance> {
        let Some(token) = self.store.load()? else {
            return Ok(CredentialProvenance::Missing);
        };
        let rejected = self
            .rejected_credential
            .lock()
            .map_err(|_| CtError::Store("credential provenance lock poisoned".into()))?;
        if *rejected == Some(self.credential_fingerprint(&token.access_token)) {
            Ok(CredentialProvenance::Stale(self.store.source()))
        } else {
            Ok(CredentialProvenance::Present(self.store.source()))
        }
    }

    fn credential_fingerprint(&self, access: &str) -> [u8; 32] {
        Sha256::new()
            .chain_update(self.base_url.as_bytes())
            .chain_update([0])
            .chain_update(access.as_bytes())
            .finalize()
            .into()
    }

    fn record_auth_failure(&self, access: &str, error: CtError) -> CtError {
        if matches!(
            &error,
            CtError::Auth(_)
                | CtError::ObsoleteCredentials { .. }
                | CtError::Api { status: 401, .. }
        ) && let Ok(mut rejected) = self.rejected_credential.lock()
        {
            *rejected = Some(self.credential_fingerprint(access));
        }
        error
    }

    fn accept_credential(&self, access: &str) {
        if let Ok(mut rejected) = self.rejected_credential.lock()
            && *rejected == Some(self.credential_fingerprint(access))
        {
            *rejected = None;
        }
    }

    // -- authenticated calls -------------------------------------------------

    pub async fn submit_run(
        &self,
        prompt: &str,
        conversation_id: Option<Uuid>,
        selected_agent_reference: Option<&str>,
    ) -> CtResult<SubmittedRun> {
        let prompt_field = prompt
            .parse::<cloudthinker_api::types::SubmitHeadlessRunRequestPrompt>()
            .map_err(|_| CtError::Usage("prompt must be 1–50000 characters".into()))?;
        let selected_agent_reference = selected_agent_reference.map(str::trim);
        if selected_agent_reference.is_some_and(str::is_empty) {
            return Err(CtError::Usage("--agent must not be empty".into()));
        }
        let body = cloudthinker_api::types::SubmitHeadlessRunRequest {
            conversation_id,
            idempotency_key: None,
            prompt: prompt_field,
            selection: cloudthinker_api::types::SavedSelection {
                option_id: "mode:pro".parse().map_err(|error| {
                    CtError::Protocol(format!("invalid default selection: {error}"))
                })?,
                thinking_effort: None,
            },
            source_conversation_id: None,
            selected_agent_reference: selected_agent_reference.map(str::to_owned),
        };
        let submitted = self
            .authed(async |c: cloudthinker_api::Client| {
                c.cli_submit_headless_run(None, &body).await
            })
            .await?;
        Ok(SubmittedRun::from_api(submitted))
    }

    /// Resolve a `--continue` UUID. A known run maps to its conversation; a
    /// 404 means the caller supplied a conversation UUID directly.
    pub async fn resolve_conversation_id(&self, id: Uuid) -> CtResult<Uuid> {
        match self.get_run(id).await {
            Ok(run) => run
                .conversation_id
                .ok_or_else(|| CtError::Protocol(format!("run {id} has no conversation id"))),
            Err(CtError::Api { status: 404, .. }) => Ok(id),
            Err(err) => Err(err),
        }
    }

    /// List recent headless runs for the selected workspace.
    pub async fn list_runs(
        &self,
        limit: u64,
        conversation_id: Option<Uuid>,
    ) -> CtResult<Vec<RunListItem>> {
        let limit = NonZeroU64::new(limit)
            .ok_or_else(|| CtError::Usage("--limit must be between 1 and 50".into()))?;
        let runs = self
            .authed(async |c: cloudthinker_api::Client| {
                c.cli_list_headless_runs(conversation_id.as_ref(), Some(limit), None)
                    .await
            })
            .await?;
        Ok(runs.into_iter().map(RunListItem::from).collect())
    }

    /// Poll one run's current status.
    pub async fn get_run(&self, run_id: Uuid) -> CtResult<RunView> {
        let view = self
            .authed(async |c: cloudthinker_api::Client| c.cli_get_headless_run(&run_id, None).await)
            .await?;
        Ok(RunView::from_api(view))
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

    /// Launch a Cyber run that executes on the caller's machine: the row
    /// starts RUNNING with no cloud worker, and this session drives it to
    /// settlement (`cyber run bind` → `cyber run evidence` → `cyber run settle`).
    ///
    /// `mode` is derived from local context and `intensity` is the caller's
    /// choice; `scan_mode` is deliberately never sent, so the backend derives
    /// the change-focus policy (incremental, coerced to full on a first run).
    pub async fn cyber_launch_local_run(
        &self,
        app_id: Uuid,
        mode: Option<CyberMode>,
        intensity: Option<CyberIntensity>,
        run_scope: Option<cloudthinker_api::types::ScopeSpec>,
    ) -> CtResult<CyberRun> {
        let body = cloudthinker_api::types::RunLaunchRequest {
            execution_host: Some(cloudthinker_api::types::RunExecutionHost::Local),
            intensity: intensity.map(CyberIntensity::into_api),
            mode: mode.map(CyberMode::into_api),
            run_directives: None,
            run_scope,
            scan_mode: None,
            selection: None,
        };
        let run = self
            .authed(async |c: cloudthinker_api::Client| {
                c.appsec_launch_run(&app_id, None, &body).await
            })
            .await?;
        Ok(CyberRun::from_api(run))
    }

    /// Read one Cyber run's current state (lifecycle, host, binding, counts).
    pub async fn cyber_get_run(&self, run_id: Uuid) -> CtResult<CyberRun> {
        let run = self
            .authed(async |c: cloudthinker_api::Client| c.appsec_get_run(&run_id, None).await)
            .await?;
        Ok(CyberRun::from_api(run))
    }

    /// List the workspace Apps used by the setup flow.
    pub async fn cyber_list_apps(&self) -> CtResult<Vec<CyberApp>> {
        let response = self
            .authed(async |c: cloudthinker_api::Client| c.appsec_list_apps(None).await)
            .await?;
        Ok(response.data.into_iter().map(CyberApp::from_api).collect())
    }

    /// Create a minimal App when setup cannot resolve an existing one. The
    /// caller can complete optional context and auth in the browser later.
    pub async fn cyber_create_app(
        &self,
        name: &str,
        target_ref: &str,
        api_coverage: bool,
    ) -> CtResult<CyberApp> {
        let framework = if api_coverage {
            cloudthinker_api::types::OwaspFramework::OwaspApi
        } else {
            cloudthinker_api::types::OwaspFramework::OwaspWeb
        };
        let body = cloudthinker_api::types::AppCreate {
            default_intensity: None,
            default_selection: None,
            frameworks: vec![framework],
            name: name.to_string(),
            setup_step: Some(cloudthinker_api::types::AppSetupStep::Targets),
            target_ref: target_ref
                .parse()
                .map_err(|error| CtError::Usage(format!("target: {error}")))?,
        };
        let app = self
            .authed(async |c: cloudthinker_api::Client| c.appsec_create_app(None, &body).await)
            .await?;
        Ok(CyberApp::from_api(app))
    }

    /// Move a draft App to `complete` setup so a run can launch. The CLI created
    /// the App with its target, so the review step the UI shows has no more
    /// input to collect; completing it here is the one-sentence local flow.
    pub async fn cyber_complete_app_setup(&self, app_id: Uuid) -> CtResult<CyberApp> {
        let body = cloudthinker_api::types::AppUpdate {
            complete_setup: Some(true),
            ..Default::default()
        };
        let app = self
            .authed(async |c: cloudthinker_api::Client| {
                c.appsec_update_app(&app_id, None, &body).await
            })
            .await?;
        Ok(CyberApp::from_api(app))
    }

    /// Read the App's domain proof and return the server's actionable state.
    pub async fn cyber_domain_verification(
        &self,
        app_id: Uuid,
    ) -> CtResult<cloudthinker_api::types::AppDomainVerificationPublic> {
        self.authed(async |c: cloudthinker_api::Client| {
            c.appsec_get_domain_verification(&app_id, None).await
        })
        .await
    }

    pub async fn cyber_list_domains(&self) -> CtResult<Vec<CyberDomain>> {
        let response = self
            .authed(async |c: cloudthinker_api::Client| c.appsec_list_domains(None).await)
            .await?;
        Ok(response
            .data
            .into_iter()
            .map(CyberDomain::from_api)
            .collect())
    }

    pub async fn cyber_create_domain(&self, domain: &str) -> CtResult<CyberDomain> {
        let body = cloudthinker_api::types::AppSecDomainCreate {
            domain: domain
                .parse()
                .map_err(|error| CtError::Usage(format!("domain: {error}")))?,
        };
        let value = self
            .authed(async |c: cloudthinker_api::Client| c.appsec_create_domain(None, &body).await)
            .await?;
        Ok(CyberDomain::from_api(value))
    }

    pub async fn cyber_check_domain(&self, domain_id: Uuid) -> CtResult<CyberDomain> {
        let value = self
            .authed(async |c: cloudthinker_api::Client| {
                c.appsec_check_domain(&domain_id, None).await
            })
            .await?;
        Ok(CyberDomain::from_api(value))
    }

    /// List one App's findings. Pagination is explicit so scripts can safely
    /// request more than the default page without an unbounded response.
    pub async fn cyber_list_findings(
        &self,
        app_id: Uuid,
        page: u64,
        take: u64,
        filter: FindingFilter,
    ) -> CtResult<CyberFindingPage> {
        let page = NonZeroU64::new(page)
            .ok_or_else(|| CtError::Usage("--page must be at least 1".into()))?;
        let take = NonZeroU64::new(take)
            .ok_or_else(|| CtError::Usage("--take must be at least 1".into()))?;
        let status = filter
            .status
            .map(|value| vec![value.into_api()])
            .unwrap_or_default();
        let severity = filter.severity.map(CyberSeverity::into_api);
        let triage_state = filter.triage_state.map(CyberTriageState::into_api);
        let response = self
            .authed(async |c: cloudthinker_api::Client| {
                c.appsec_list_findings(
                    &app_id,
                    None,
                    Some(page),
                    None,
                    severity,
                    Some(cloudthinker_api::types::FindingSort::Severity),
                    Some(cloudthinker_api::types::SortOrder::Asc),
                    (!status.is_empty()).then_some(&status),
                    Some(take),
                    triage_state,
                    None,
                )
                .await
            })
            .await?;
        Ok(CyberFindingPage {
            data: response
                .data
                .into_iter()
                .map(CyberFinding::from_api)
                .collect(),
            page: response.meta.page,
            take: response.meta.take,
            total: response.meta.total_items,
            pages: response.meta.total_pages,
        })
    }

    /// Fetch one finding by its durable UUID.
    pub async fn cyber_get_finding(&self, finding_id: Uuid) -> CtResult<CyberFinding> {
        let finding = self
            .authed(async |c: cloudthinker_api::Client| {
                c.appsec_get_finding(&finding_id, None).await
            })
            .await?;
        Ok(CyberFinding::from_api(finding))
    }

    /// Request a filtered App findings PDF. The backend returns a short-lived
    /// download URL; the CLI never downloads or persists that artifact.
    pub async fn cyber_export_findings(&self, app_id: Uuid) -> CtResult<CyberExport> {
        let export = self
            .authed(async |c: cloudthinker_api::Client| {
                c.appsec_export_findings_pdf(
                    &app_id,
                    None,
                    None,
                    None,
                    Some(cloudthinker_api::types::FindingSort::Severity),
                    Some(cloudthinker_api::types::SortOrder::Asc),
                    None,
                    None,
                    None,
                )
                .await
            })
            .await?;
        Ok(CyberExport {
            download_url: export.download_url,
            filename: export.filename,
        })
    }

    /// Request one finding's PDF.
    pub async fn cyber_export_finding(&self, finding_id: Uuid) -> CtResult<CyberExport> {
        let export = self
            .authed(async |c: cloudthinker_api::Client| {
                c.appsec_download_finding_pdf(&finding_id, None).await
            })
            .await?;
        Ok(CyberExport {
            download_url: export.download_url,
            filename: export.filename,
        })
    }

    /// Cancel a run through the shared terminal CAS. Repeating this operation
    /// returns the already-terminal projection and is therefore idempotent.
    pub async fn cyber_cancel(&self, run_id: Uuid) -> CtResult<CyberRun> {
        let run = self
            .authed(async |c: cloudthinker_api::Client| c.appsec_cancel_run(&run_id, None).await)
            .await?;
        Ok(CyberRun::from_api(run))
    }

    /// Replay a run's mirrored local-agent transcript, oldest entry first.
    /// Pages the same append-only entries route the browser viewer reads; a run
    /// that never bound a conversation returns an empty page.
    pub async fn cyber_session_entries(
        &self,
        conversation_id: Uuid,
    ) -> CtResult<Vec<CyberSessionEntry>> {
        let mut all: Vec<CyberSessionEntry> = Vec::new();
        let mut after_seq: u64 = 0;
        for _ in 0..40 {
            let page = self
                .authed(async |c: cloudthinker_api::Client| {
                    c.agent_cli_list_agent_cli_session_entries(
                        &conversation_id,
                        Some(after_seq),
                        None,
                        None,
                    )
                    .await
                })
                .await?;
            let last_seq = u64::try_from(page.last_seq).unwrap_or(after_seq);
            let count = page.entries.len();
            all.extend(page.entries.into_iter().map(|e| CyberSessionEntry {
                seq: e.seq,
                entry_id: e.entry_id,
                parent_id: e.parent_id,
                entry_type: e.entry_type,
                payload: serde_json::Value::Object(e.payload),
            }));
            if count == 0 || last_seq <= after_seq {
                break;
            }
            after_seq = last_seq;
        }
        Ok(all)
    }

    pub async fn cyber_bind_run(
        &self,
        run_id: Uuid,
        conversation_id: Uuid,
    ) -> CtResult<CyberRunBrief> {
        let body = cloudthinker_api::types::RunBindRequest { conversation_id };
        let brief = self
            .authed(async |c: cloudthinker_api::Client| {
                c.appsec_bind_local_run(&run_id, None, &body).await
            })
            .await?;
        Ok(CyberRunBrief::from_api(brief))
    }

    /// Upload one batch of text evidence into the run's durable app-memory
    /// tree. The backend fences unsafe paths and sizes; the receipt reports
    /// what it wrote and what it skipped.
    pub async fn cyber_submit_evidence(
        &self,
        run_id: Uuid,
        files: Vec<EvidenceFile>,
    ) -> CtResult<EvidenceReceipt> {
        let body = cloudthinker_api::types::EvidenceSubmitRequest {
            files: files
                .into_iter()
                .map(|f| {
                    let path = f
                        .path
                        .parse()
                        .map_err(|error| CtError::Usage(format!("evidence path: {error}")))?;
                    let content_base64 = f
                        .content_base64
                        .map(|value| {
                            value.parse().map_err(|error| {
                                CtError::Usage(format!("evidence base64: {error}"))
                            })
                        })
                        .transpose()?;
                    let mime_type = f
                        .mime_type
                        .map(|value| {
                            value.parse().map_err(|error| {
                                CtError::Usage(format!("evidence MIME type: {error}"))
                            })
                        })
                        .transpose()?;
                    let size_bytes = i64::try_from(f.size_bytes)
                        .map_err(|_| CtError::Usage("evidence is too large".into()))?;
                    Ok::<_, CtError>(cloudthinker_api::types::EvidenceFileUpload {
                        path,
                        content: f.content,
                        content_base64,
                        mime_type,
                        size_bytes: Some(size_bytes),
                    })
                })
                .collect::<CtResult<Vec<_>>>()?,
        };
        let receipt = self
            .authed(async |c: cloudthinker_api::Client| {
                c.appsec_submit_local_evidence(&run_id, None, &body).await
            })
            .await?;
        Ok(EvidenceReceipt {
            written: receipt.written,
            skipped: receipt
                .skipped
                .into_iter()
                .map(|s| EvidenceSkipped {
                    path: s.path,
                    reason: s.reason,
                })
                .collect(),
        })
    }

    /// Settle a local run. The backend's finalize CAS decides; `settled: false`
    /// means another writer got there first (or the run already ended) — that
    /// is a no-op, not an error.
    pub async fn cyber_settle(
        &self,
        run_id: Uuid,
        agent_succeeded: bool,
        result_message: Option<String>,
    ) -> CtResult<SettleResult> {
        let message = result_message
            .map(cloudthinker_api::types::SettleLocalRunRequestResultMessage::try_from)
            .transpose()
            .map_err(|e| CtError::Usage(format!("--message: {e}")))?;
        let body = cloudthinker_api::types::SettleLocalRunRequest {
            agent_succeeded,
            result_message: message,
        };
        let result = self
            .authed(async |c: cloudthinker_api::Client| {
                c.appsec_settle_local_run(&run_id, None, &body).await
            })
            .await?;
        Ok(SettleResult {
            settled: result.settled,
        })
    }

    /// Ask the backend for this run's bounded work plan. The backend owns the
    /// rules (what is in scope, what to probe, when coverage is complete); the
    /// host executes the checks it can reach and never re-derives the rules.
    pub async fn cyber_issue_work_plan(&self, run_id: Uuid) -> CtResult<WorkPlan> {
        let plan = self
            .authed(async |c: cloudthinker_api::Client| {
                c.appsec_issue_work_plan(&run_id, None).await
            })
            .await?;
        Ok(WorkPlan::from_api(plan))
    }

    /// Report local observations for one plan. The backend marks the rows and
    /// returns the run-completion gate.
    pub async fn cyber_ingest_observations(
        &self,
        run_id: Uuid,
        plan_id: &str,
        observations: Vec<Observation>,
    ) -> CtResult<CoverageReport> {
        validate_observation_ids(&observations)?;
        if observations.is_empty() {
            return self.cyber_get_coverage(run_id).await;
        }
        let mut latest_report = None;
        for batch in observation_batches(&observations) {
            let body = cloudthinker_api::types::ObservationsIngestRequest {
                observations: batch
                    .iter()
                    .map(|o| cloudthinker_api::types::ObservationIn {
                        row_id: o.row_id.clone(),
                        status: o.status.to_api(),
                        reason: o.reason.clone(),
                        evidence_ref: o.evidence_ref.clone(),
                        worker: o.worker.clone(),
                    })
                    .collect(),
            };
            let report = self
                .authed(async |c: cloudthinker_api::Client| {
                    c.appsec_ingest_observations(&run_id, plan_id, None, &body)
                        .await
                })
                .await?;
            latest_report = Some(CoverageReport::from_api(report));
        }
        latest_report
            .ok_or_else(|| CtError::Usage("observation batching produced no request".to_string()))
    }

    /// Read this run's coverage ledger and the completion gate.
    pub async fn cyber_get_coverage(&self, run_id: Uuid) -> CtResult<CoverageReport> {
        let report = self
            .authed(async |c: cloudthinker_api::Client| {
                c.appsec_get_run_coverage(&run_id, None).await
            })
            .await?;
        Ok(CoverageReport::from_api(report))
    }

    /// Read the run's attack-surface overview. An agent reads this to author
    /// themes before it partitions the plan and fans scouts out.
    pub async fn cyber_surface(&self, run_id: Uuid) -> CtResult<Surface> {
        let surface = self
            .authed(async |c: cloudthinker_api::Client| {
                c.appsec_get_run_surface(&run_id, None).await
            })
            .await?;
        Ok(Surface::from_api(surface))
    }

    /// Carve the run's plan into theme lanes. A null themes doc returns the
    /// surface alone; a themes doc returns one shard per lane, each a disjoint
    /// row set a single scout owns. The backend owns the partition rule; the
    /// host never re-derives lanes. The themes doc is parsed here so a command
    /// never depends on `serde_json`.
    pub async fn cyber_partition(
        &self,
        run_id: Uuid,
        themes_json: Option<&str>,
        max_rows_per_shard: Option<i64>,
        only_status: Option<String>,
    ) -> CtResult<Partition> {
        let themes = match themes_json {
            Some(raw) => match serde_json::from_str::<serde_json::Value>(raw) {
                Ok(serde_json::Value::Object(map)) => Some(map),
                Ok(_) => {
                    return Err(CtError::Usage(
                        "--themes must be a JSON object with a 'themes' list".to_string(),
                    ));
                }
                Err(e) => return Err(CtError::Usage(format!("--themes is not valid JSON: {e}"))),
            },
            None => None,
        };
        let body = cloudthinker_api::types::PartitionRequest {
            themes,
            max_rows_per_shard,
            only_status,
        };
        let partition = self
            .authed(async |c: cloudthinker_api::Client| {
                c.appsec_partition_work_plan(&run_id, None, &body).await
            })
            .await?;
        Ok(Partition::from_api(partition))
    }

    /// Run one plan check against a locally reachable target. The request
    /// leaves from this machine, so `localhost` is in scope; the rule that
    /// chose the check never left the backend.
    pub async fn cyber_probe(
        &self,
        method: &str,
        url: &str,
        auth_header: Option<(&str, &str)>,
    ) -> CtResult<ProbeOutcome> {
        let method = reqwest::Method::from_bytes(method.as_bytes())
            .map_err(|_| CtError::Usage(format!("unsupported method {method}")))?;
        let mut request = self
            .probe_http
            .request(method, url)
            .timeout(Duration::from_secs(PROBE_TIMEOUT_SECS));
        // Send the configured identity so a gray/white run reaches an
        // authenticated endpoint; an invalid header value surfaces as a transport
        // error and is recorded as blocked, never a silent unauthenticated pass.
        if let Some((name, value)) = auth_header {
            request = request.header(name, value);
        }
        let response = request.send().await;
        match response {
            Ok(resp) => Ok(ProbeOutcome {
                status: Some(resp.status().as_u16()),
                error: None,
            }),
            Err(err) => Ok(ProbeOutcome {
                status: None,
                error: Some(err.to_string()),
            }),
        }
    }

    /// Resolve the live account and workspace carried by this credential.
    pub async fn whoami(&self) -> CtResult<CliIdentity> {
        let identity = self
            .authed(async |c: cloudthinker_api::Client| c.login_cli_whoami().await)
            .await?;
        Ok(self.identity_from_api(identity))
    }

    // -- unauthenticated calls -----------------------------------------------

    /// Exchange a one-time code + verifier for a token. The backend's one
    /// generic 400 collapses to a single generic auth error (it does not
    /// distinguish burned vs expired codes, CA-CLI-4); any other status
    /// surfaces as-is.
    pub async fn exchange_code(&self, code: &str, verifier: &str) -> CtResult<StoredToken> {
        let body = cloudthinker_api::types::CliTokenRequest {
            code: code
                .parse()
                .map_err(|e| CtError::Login(format!("malformed code: {e}")))?,
            code_verifier: verifier
                .parse()
                .map_err(|e| CtError::Login(format!("malformed verifier: {e}")))?,
        };
        let api = cloudthinker_api::Client::new_with_client(&self.base_url, self.anon_http.clone());
        match api.login_exchange_cli_token(&body).await {
            Ok(rv) => {
                let mut token = StoredToken::from_token(&rv.into_inner());
                if let Ok(identity) = self.whoami_with_access(&token.access_token).await {
                    token.workspace_name = Some(identity.workspace_name);
                }
                Ok(token)
            }
            Err(e) => Err(match to_ct_error(e).await {
                CtError::Api { status: 400, .. } => CtError::Auth(format!(
                    "could not complete login; run `{}` again",
                    login_command(&self.base_url)
                )),
                other => other,
            }),
        }
    }

    async fn whoami_with_access(&self, access: &str) -> CtResult<CliIdentity> {
        let api = self.api_client(access, Duration::from_secs(REQUEST_TIMEOUT_SECS))?;
        let response = match api.login_cli_whoami().await {
            Ok(response) => response,
            Err(error) => return Err(to_ct_error(error).await),
        };
        Ok(self.identity_from_api(response.into_inner()))
    }

    fn identity_from_api(&self, api: cloudthinker_api::types::CliWhoAmIResponse) -> CliIdentity {
        CliIdentity {
            host: origin_of(&self.base_url).unwrap_or_else(|_| self.base_url.clone()),
            user_id: api.user_id,
            user_email: api.user_email,
            workspace_id: api.workspace_id,
            workspace_name: api.workspace_name,
        }
    }

    /// Start an outbound-only login for machines where loopback callbacks fail.
    pub async fn start_device_authorization(&self) -> CtResult<DeviceAuthorization> {
        let api = cloudthinker_api::Client::new_with_client(&self.base_url, self.anon_http.clone());
        let response = match api.login_start_cli_device_authorization().await {
            Ok(response) => response.into_inner(),
            Err(error) => return Err(to_ct_error(error).await),
        };
        let expires_in = positive_duration(response.expires_in, "expires_in")?;
        let interval = positive_duration(response.interval, "interval")?;
        validate_verification_uri(&self.base_url, &response.verification_uri)?;
        Ok(DeviceAuthorization {
            device_code: response.device_code,
            user_code: response.user_code,
            verification_uri: response.verification_uri,
            expires_in,
            interval,
        })
    }

    /// Poll once; pacing and deadline ownership stay in the auth module.
    pub async fn poll_device_token(&self, device_code: &str) -> CtResult<DeviceTokenPoll> {
        use cloudthinker_api::Error as ApiError;
        use cloudthinker_api::types::CliDeviceTokenErrorCode as Code;

        let body = cloudthinker_api::types::CliDeviceTokenRequest {
            device_code: device_code
                .parse()
                .map_err(|e| CtError::Login(format!("malformed device code: {e}")))?,
        };
        let api = cloudthinker_api::Client::new_with_client(&self.base_url, self.anon_http.clone());
        match api.login_poll_cli_device_token(&body).await {
            Ok(response) => {
                let mut token = StoredToken::from_token(&response.into_inner());
                if let Ok(identity) = self.whoami_with_access(&token.access_token).await {
                    token.workspace_name = Some(identity.workspace_name);
                }
                Ok(DeviceTokenPoll::Token(token))
            }
            Err(ApiError::ErrorResponse(response)) => {
                let error = response.into_inner();
                let interval = optional_poll_duration(error.interval)?;
                match error.error {
                    Code::AuthorizationPending => Ok(DeviceTokenPoll::Pending(interval)),
                    Code::SlowDown => Ok(DeviceTokenPoll::SlowDown(interval)),
                    Code::AccessDenied => Err(CtError::LoginDenied),
                    Code::ExpiredToken => Err(device_code_expired(&self.base_url)),
                }
            }
            Err(ApiError::UnexpectedResponse(response))
                if crate::error::is_retryable_status(response.status().as_u16()) =>
            {
                let (_, retry_after) = classify(ApiError::UnexpectedResponse(response)).await;
                Ok(DeviceTokenPoll::Unavailable(
                    retry_after.unwrap_or_default(),
                ))
            }
            Err(ApiError::UnexpectedResponse(response)) => {
                let status = response.status().as_u16();
                Err(CtError::Api {
                    status,
                    detail: response
                        .text()
                        .await
                        .ok()
                        .as_deref()
                        .and_then(crate::error::parse_error_message),
                })
            }
            Err(ApiError::InvalidResponsePayload(_, error)) => Err(CtError::Protocol(format!(
                "unreadable device response: {error}"
            ))),
            Err(ApiError::CommunicationError(error)) => Err(CtError::Transport(error.to_string())),
            Err(ApiError::ResponseBodyError(error)) => Err(CtError::Transport(error.to_string())),
            Err(ApiError::InvalidUpgrade(error)) => Err(CtError::Transport(error.to_string())),
            Err(ApiError::InvalidRequest(message)) | Err(ApiError::Custom(message)) => {
                Err(CtError::Transport(message))
            }
        }
    }

    /// Best-effort server revoke followed by a required local clear.
    pub async fn logout(&self) -> CtResult<LogoutOutcome> {
        let lock = acquire_credential_lock(self.store.clone()).await?;
        let current = match self.store.load_locked(&lock) {
            Ok(current) => current,
            Err(CtError::ObsoleteCredentials { .. }) => None,
            Err(error) => return Err(error),
        };
        let Some(current) = current else {
            return Ok(LogoutOutcome::NothingStored);
        };
        let revoked = self.revoke(current.refresh_token).await;
        self.store.clear_locked(&lock)?;
        if let Err(cause) = revoked {
            return Err(CtError::Logout(format!(
                "server session could not be revoked ({cause}); local credential was cleared"
            )));
        }
        Ok(LogoutOutcome::Cleared)
    }

    /// Revoke every stored workspace session for this host, then clear them.
    pub async fn logout_all(&self) -> CtResult<LogoutOutcome> {
        let lock = acquire_credential_lock(self.store.clone()).await?;
        let tokens = match self.store.load_all_locked(&lock) {
            Ok(tokens) => tokens,
            Err(CtError::ObsoleteCredentials { .. }) => Vec::new(),
            Err(error) => return Err(error),
        };
        if tokens.is_empty() {
            self.store.clear_all_locked(&lock)?;
            return Ok(LogoutOutcome::NothingStored);
        }
        let mut attempted = 0_usize;
        let mut causes = Vec::new();
        for token in tokens {
            if token.refresh_token.is_some() {
                attempted += 1;
                if let Err(cause) = self.revoke(token.refresh_token).await {
                    causes.push(cause);
                }
            }
        }
        self.store.clear_all_locked(&lock)?;
        if !causes.is_empty() {
            return Err(CtError::Logout(format!(
                "server revocation failed for {} of {attempted} sessions ({}); local credentials were cleared",
                causes.len(),
                causes.join("; ")
            )));
        }
        Ok(LogoutOutcome::Cleared)
    }

    /// The current access token, refreshed proactively when it is within the
    /// skew window of expiry. A secret: callers hand it to a bearer header or
    /// to `cloudthinker auth token`'s stdout, never to a log.
    pub async fn access_token(&self) -> CtResult<String> {
        let current = self.store.load()?.ok_or_else(|| {
            CtError::Auth(format!(
                "not logged in; run `{}`",
                login_command(&self.base_url)
            ))
        })?;
        if self.store.refresh_enabled()
            && current.expires_within(RefreshCoordinator::proactive_skew())
        {
            let rotated = self
                .refresh
                .refresh(&current.access_token)
                .await
                .map_err(|error| self.record_auth_failure(&current.access_token, error))?;
            Ok(rotated.access_token)
        } else {
            Ok(current.access_token)
        }
    }

    async fn revoke(&self, refresh_token: Option<String>) -> Result<(), String> {
        let Some(refresh_token) = refresh_token else {
            return Ok(());
        };
        let body = cloudthinker_api::types::RevokeTokenRequest {
            refresh_token: Some(refresh_token),
        };
        match cloudthinker_api::Client::new_with_client(&self.base_url, self.anon_http.clone())
            .login_logout(&body)
            .await
        {
            Ok(_) => Ok(()),
            Err(error) => Err(to_ct_error(error).await.to_string()),
        }
    }

    // -- internals -----------------------------------------------------------

    /// Run `call` with a fresh bearer, retrying exactly once through a refresh
    /// on a 401. Proactive refresh happens in `access_token` before the first
    /// attempt.
    pub(crate) async fn authed<T, F>(&self, call: F) -> CtResult<T>
    where
        F: AsyncFn(
            cloudthinker_api::Client,
        )
            -> Result<cloudthinker_api::ResponseValue<T>, cloudthinker_api::Error<()>>,
    {
        self.authed_with_timeout(Duration::from_secs(REQUEST_TIMEOUT_SECS), call)
            .await
    }

    pub(crate) async fn authed_with_timeout<T, F>(&self, timeout: Duration, call: F) -> CtResult<T>
    where
        F: AsyncFn(
            cloudthinker_api::Client,
        )
            -> Result<cloudthinker_api::ResponseValue<T>, cloudthinker_api::Error<()>>,
    {
        let (access, client) = self.authorized_client(timeout).await?;
        let error = match call(client).await {
            Ok(rv) => {
                self.accept_credential(&access);
                return Ok(rv.into_inner());
            }
            Err(error) => error,
        };
        let (rotated, client) = self.retry_client(&access, error, timeout).await?;
        match call(client).await {
            Ok(rv) => {
                self.accept_credential(&rotated);
                Ok(rv.into_inner())
            }
            Err(error) => Err(self.authed_failure(&rotated, error).await),
        }
    }

    async fn authorized_client(
        &self,
        timeout: Duration,
    ) -> CtResult<(String, cloudthinker_api::Client)> {
        let access = self.access_token().await?;
        let client = self.api_client(&access, timeout)?;
        Ok((access, client))
    }

    async fn retry_client(
        &self,
        access: &str,
        error: cloudthinker_api::Error<()>,
        timeout: Duration,
    ) -> CtResult<(String, cloudthinker_api::Client)> {
        let is_unauthorized = error.status().map(|s| s.as_u16()) == Some(401);
        if !is_unauthorized || !self.store.refresh_enabled() {
            return Err(self.authed_failure(access, error).await);
        }
        let rotated = self
            .refresh
            .refresh(access)
            .await
            .map_err(|error| self.record_auth_failure(access, error))?;
        let client = self.api_client(&rotated.access_token, timeout)?;
        Ok((rotated.access_token, client))
    }

    async fn authed_failure(&self, access: &str, error: cloudthinker_api::Error<()>) -> CtError {
        self.record_auth_failure(access, reclassify(to_ct_error(error).await, &self.base_url))
    }

    /// Build (or reuse) an API client whose bearer header is `access`.
    fn api_client(&self, access: &str, timeout: Duration) -> CtResult<cloudthinker_api::Client> {
        let mut guard = self
            .http_cache
            .lock()
            .map_err(|_| CtError::Transport("http cache poisoned".into()))?;
        let cached = match guard.as_ref() {
            Some((tok, cached_timeout, client)) if tok == access && *cached_timeout == timeout => {
                Some(client.clone())
            }
            _ => None,
        };
        let http = match cached {
            Some(client) => client,
            None => {
                let client = build_authed_http(access, timeout)?;
                *guard = Some((access.to_string(), timeout, client.clone()));
                client
            }
        };
        drop(guard);
        Ok(cloudthinker_api::Client::new_with_client(
            &self.base_url,
            http,
        ))
    }
}

fn build_authed_http(access: &str, timeout: Duration) -> CtResult<reqwest::Client> {
    let mut headers = reqwest::header::HeaderMap::new();
    let value = reqwest::header::HeaderValue::from_str(&format!("Bearer {access}"))
        .map_err(|e| CtError::Auth(format!("invalid token header: {e}")))?;
    headers.insert(reqwest::header::AUTHORIZATION, value);
    reqwest::Client::builder()
        .timeout(timeout)
        .default_headers(headers)
        .build()
        .map_err(|e| CtError::Transport(format!("http client build: {e}")))
}

fn positive_duration(seconds: i64, field: &str) -> CtResult<Duration> {
    let seconds = u64::try_from(seconds)
        .ok()
        .filter(|seconds| *seconds > 0)
        .ok_or_else(|| CtError::Protocol(format!("device response has invalid {field}")))?;
    Ok(Duration::from_secs(seconds))
}

fn optional_poll_duration(seconds: Option<i64>) -> CtResult<Duration> {
    match seconds {
        Some(seconds) => positive_duration(seconds, "interval"),
        None => Ok(Duration::ZERO),
    }
}

fn validate_verification_uri(base_url: &str, verification_uri: &str) -> CtResult<()> {
    if origin_of(base_url)? != origin_of(verification_uri)? {
        return Err(CtError::Protocol(
            "device verification URL does not match the configured CloudThinker origin".into(),
        ));
    }
    Ok(())
}

/// A 401 surfacing here (rather than being cured by refresh) means the user
/// must log in again.
fn reclassify(err: CtError, base_url: &str) -> CtError {
    match err {
        CtError::Api { status: 401, .. } => {
            CtError::Auth(format!("run `{}`", login_command(base_url)))
        }
        CtError::Api {
            status: 403,
            detail,
        } => CtError::Api {
            status: 403,
            detail: Some(format!(
                "{}; ask a workspace admin for access, or run `{}` to authorize another workspace",
                detail.unwrap_or_else(|| "permission denied".into()),
                login_command(base_url)
            )),
        },
        other => other,
    }
}

pub(crate) fn device_code_expired(base_url: &str) -> CtError {
    CtError::Timeout(format!(
        "device code expired; run `{} --device-auth` again",
        login_command(base_url)
    ))
}

pub fn login_command(url: &str) -> String {
    login_command_for(url, &crate::cli_config::effective_default_url())
}

fn login_command_for(url: &str, effective_default: &str) -> String {
    let default_origin = origin_of(effective_default);
    match (origin_of(url), url::Url::parse(url)) {
        (Ok(origin), _) if default_origin.as_ref().is_ok_and(|d| *d == origin) => {
            "cloudthinker login".into()
        }
        (Ok(_), Ok(parsed)) if matches!(parsed.path(), "" | "/") => format!(
            "cloudthinker login --url {}",
            parsed.origin().ascii_serialization()
        ),
        (Ok(_), Ok(_)) => format!("cloudthinker login --url {}", url.trim_end_matches('/')),
        _ => "cloudthinker login".into(),
    }
}

/// Derive the canonical origin (`scheme://host:port`) used as the token-store
/// key, and enforce the transport contract while we're parsing anyway: only
/// `https://` is accepted, except loopback (127.0.0.1 / localhost / ::1) for
/// local dev. Keying by the full origin — not the bare host — means an HTTPS
/// token can never be replayed against a plain-HTTP listener or a different
/// port on the same host.
pub fn origin_of(base_url: &str) -> CtResult<String> {
    let url =
        url::Url::parse(base_url).map_err(|e| CtError::Usage(format!("invalid --url: {e}")))?;
    let scheme = url.scheme();
    if scheme != "https" && !(scheme == "http" && is_loopback_host(&url)) {
        return Err(CtError::Usage(
            "--url must be https:// (plain http is only allowed for loopback: 127.0.0.1, localhost, ::1)".into(),
        ));
    }
    let host = url
        .host_str()
        .ok_or_else(|| CtError::Usage("--url has no host".into()))?;
    let port = url
        .port_or_known_default()
        .ok_or_else(|| CtError::Usage("--url has no resolvable port".into()))?;
    Ok(format!("{scheme}://{host}:{port}"))
}

fn normalize_base_url(raw: &str) -> CtResult<String> {
    origin_of(raw)?;
    Ok(raw.trim_end_matches('/').to_string())
}

fn is_loopback_host(url: &url::Url) -> bool {
    match url.host() {
        Some(url::Host::Domain(d)) => d.eq_ignore_ascii_case("localhost"),
        Some(url::Host::Ipv4(addr)) => addr.is_loopback(),
        Some(url::Host::Ipv6(addr)) => addr.is_loopback(),
        None => false,
    }
}

/// Resolve the read/refresh store: env override if `CLOUDTHINKER_TOKEN` is set,
/// otherwise the credentials file. Opening the file the first time after the
/// 0.5.3 upgrade migrates a keyring login into it (`FileStore::open_default`).
pub fn resolve_store(base_url: &str, workspace: Option<&str>) -> CtResult<Arc<dyn TokenStore>> {
    if let Some(env) = EnvTokenStore::from_env() {
        if workspace.is_some() {
            return Err(CtError::Usage(format!(
                "--workspace cannot be used with {TOKEN_ENV_VAR}"
            )));
        }
        return Ok(Arc::new(env));
    }
    file_store(base_url, workspace)
}

/// The persistent store used by `login`/`logout` (never the env override — those
/// commands must write real credentials).
pub fn persistent_store(base_url: &str, workspace: Option<&str>) -> CtResult<Arc<dyn TokenStore>> {
    file_store(base_url, workspace)
}

fn file_store(base_url: &str, workspace: Option<&str>) -> CtResult<Arc<dyn TokenStore>> {
    let selector = workspace.map_or(WorkspaceSelector::Active, |value| {
        WorkspaceSelector::IdOrName(value.to_string())
    });
    Ok(Arc::new(FileStore::open_default(
        origin_of(base_url)?,
        selector,
    )?))
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::*;
    use crate::auth::refresh::PROACTIVE_REFRESH_SKEW_SECS;
    use crate::test_support::{MockTokenStore, stored, token_json};
    use wiremock::matchers::{body_json, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn local_run_brief_json_does_not_expose_server_memory_paths() {
        let brief = CyberRunBrief {
            run_id: Uuid::from_u128(1),
            app_id: Uuid::from_u128(2),
            app_name: "ct32-local".into(),
            conversation_id: Some(Uuid::from_u128(3)),
            result: CyberRunResult::Running,
            execution_host: CyberExecutionHost::Local,
            target: "http://ct32.localhost:8088".into(),
            frameworks: vec!["owasp_api".into()],
            mode: "white".into(),
            intensity: "full".into(),
            scan_mode: "full".into(),
            report_preferences: cloudthinker_api::types::AppSecReportPreferences::default(),
            report_reference: Some(cloudthinker_api::types::LocalRunReportReferencePublic {
                source_id: Uuid::from_u128(4),
                name: "house-style.pdf".into(),
                sha256: "a".repeat(64),
                content_type: "application/pdf".into(),
                download_url: "https://objects.example.test/signed".into(),
            }),
            started_at: DateTime::from_timestamp(0, 0).unwrap(),
            finished_at: None,
            targets: vec![],
            local_workspace: Some(LocalCyberWorkspace {
                workspace_root: "/private/runs/1/workspace".into(),
                evidence_root: "/private/runs/1/workspace/evidence".into(),
                workflow_root: "/private/runs/1/workspace/workflow".into(),
                manifest: "/private/runs/1/workspace/manifest.json".into(),
            }),
        };

        let json = serde_json::to_value(brief).unwrap();

        assert_eq!(json["execution_host"], "local");
        assert_eq!(
            json["local_workspace"]["workspace_root"],
            "/private/runs/1/workspace"
        );
        assert_eq!(
            json["local_workspace"]["evidence_root"],
            "/private/runs/1/workspace/evidence"
        );
        assert_eq!(
            json["local_workspace"]["workflow_root"],
            "/private/runs/1/workspace/workflow"
        );
        assert!(json["local_workspace"].get("memory_root").is_none());
        assert!(json.get("memory_root").is_none());
        assert!(json.get("memory_mount").is_none());
        assert_eq!(json["report_reference"]["sha256"], "a".repeat(64));
        assert_eq!(
            json["report_reference"]["download_url"],
            "https://objects.example.test/signed"
        );
        assert!(json["report_reference"].get("storage_key").is_none());
    }

    #[test]
    fn only_candidate_blocked_and_skipped_coverage_require_a_reason() {
        assert!(CoverageStatus::Candidate.requires_reason());
        assert!(CoverageStatus::Blocked.requires_reason());
        assert!(CoverageStatus::SkippedWithReason.requires_reason());
        assert!(!CoverageStatus::Covered.requires_reason());
    }

    fn run_status_json(status: &str, answer: Option<&str>) -> serde_json::Value {
        serde_json::json!({
            "run_id": "11111111-1111-4111-8111-111111111111",
            "conversation_id": "22222222-2222-4222-8222-222222222222",
            "status": status,
            "answer": answer,
            "message": null,
            "failure_kind": null,
            "web_url": "https://app.example.com/c/22222222-2222-4222-8222-222222222222",
            "created_at": "2026-07-20T00:00:00Z",
            "start_time": null,
            "end_time": null,
        })
    }

    fn api_error_json(code: &str, message: &str) -> serde_json::Value {
        serde_json::json!({
            "error": {
                "code": code,
                "message": message,
                "retryable": false,
                "field_errors": null,
            },
            "request_id": "request-test",
            "detail": message,
        })
    }

    fn valid_verifier() -> String {
        "a".repeat(43)
    }

    struct MultiLogoutStore {
        tokens: Mutex<Vec<StoredToken>>,
        cleared: AtomicBool,
    }

    impl MultiLogoutStore {
        fn new(tokens: Vec<StoredToken>) -> Self {
            Self {
                tokens: Mutex::new(tokens),
                cleared: AtomicBool::new(false),
            }
        }
    }

    impl TokenStore for MultiLogoutStore {
        fn load(&self) -> CtResult<Option<StoredToken>> {
            Ok(self.tokens.lock().unwrap().last().cloned())
        }

        fn load_all(&self) -> CtResult<Vec<StoredToken>> {
            Ok(self.tokens.lock().unwrap().clone())
        }

        fn save(&self, _token: &StoredToken) -> CtResult<()> {
            Ok(())
        }

        fn clear(&self) -> CtResult<()> {
            self.tokens.lock().unwrap().pop();
            Ok(())
        }

        fn clear_all(&self) -> CtResult<()> {
            self.tokens.lock().unwrap().clear();
            self.cleared.store(true, Ordering::SeqCst);
            Ok(())
        }

        fn refresh_enabled(&self) -> bool {
            true
        }
    }

    // [ISSUE-1a/1b]: the store key is the full origin, so switching scheme or
    // port never reuses another origin's token.
    #[test]
    fn origin_of_scheme_changes_the_key() {
        // Both sides are individually valid (http is allowed here only because
        // the host is loopback) yet must key to different origins.
        let https = origin_of("https://127.0.0.1:9443").unwrap();
        let http = origin_of("http://127.0.0.1:9443").unwrap();
        assert_ne!(https, http);
    }

    #[test]
    fn origin_of_port_changes_the_key() {
        let default_port = origin_of("https://app.example.com").unwrap();
        let explicit_port = origin_of("https://app.example.com:8443").unwrap();
        assert_ne!(default_port, explicit_port);
    }

    // [ISSUE-1c]: a non-loopback plain-http URL is rejected outright — an
    // HTTPS token must never be requested over an unencrypted connection to a
    // real host.
    #[test]
    fn origin_of_rejects_non_loopback_http() {
        let err = origin_of("http://app.example.com").unwrap_err();
        assert!(matches!(err, CtError::Usage(_)), "got {err:?}");
    }

    // [ISSUE-1d]: loopback stays the local-dev escape hatch for plain http.
    #[test]
    fn origin_of_allows_loopback_http() {
        assert!(origin_of("http://127.0.0.1:8000").is_ok());
        assert!(origin_of("http://localhost:8000").is_ok());
        assert!(origin_of("http://[::1]:8000").is_ok());
    }

    // CA-CLI-1 (login happy, server half): a valid code + verifier exchange
    // yields a stored token carrying the approved workspace. The loopback half
    // is covered by `auth::pkce::tests::happy_callback_yields_code`.
    #[tokio::test]
    async fn ca_cli_1_exchange_success_returns_stored_token() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/login/cli/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "fresh-access",
                "refresh_token": "fresh-refresh",
                "token_type": "bearer",
                "workspace_id": "55555555-5555-4555-8555-555555555555",
            })))
            .mount(&server)
            .await;

        let client = CtClient::new(server.uri(), Arc::new(MockTokenStore::new(None))).unwrap();
        let token = client
            .exchange_code("one-time-code", &valid_verifier())
            .await
            .unwrap();
        assert_eq!(token.access_token, "fresh-access");
        assert_eq!(token.refresh_token.as_deref(), Some("fresh-refresh"));
        assert_eq!(
            token.workspace_id,
            Some(Uuid::parse_str("55555555-5555-4555-8555-555555555555").unwrap())
        );
    }

    #[tokio::test]
    async fn base_url_trailing_slashes_normalize_to_the_bare_origin() {
        let server = MockServer::start().await;
        let store = || Arc::new(MockTokenStore::new(None));
        let bare = CtClient::new(server.uri(), store()).unwrap();
        let one = CtClient::new(format!("{}/", server.uri()), store()).unwrap();
        let many = CtClient::new(format!("{}///", server.uri()), store()).unwrap();
        assert_eq!(bare.base_url(), server.uri());
        assert_eq!(one.base_url(), server.uri());
        assert_eq!(many.base_url(), server.uri());
    }

    #[test]
    fn base_url_must_be_a_usable_origin() {
        let store = || Arc::new(MockTokenStore::new(None));
        for raw in ["", "ftp://app.example.com"] {
            let err = CtClient::new(raw, store()).err().expect(raw);
            assert!(matches!(err, CtError::Usage(_)), "{raw}: got {err:?}");
        }
    }

    #[tokio::test]
    async fn trailing_slash_base_url_requests_a_single_slash_path() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/cli/whoami"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "user_id": "22222222-2222-4222-8222-222222222222",
                "user_email": "duc@example.com",
                "workspace_id": "11111111-1111-4111-8111-111111111111",
                "workspace_name": "Production",
                "organization_id": null,
            })))
            .expect(1)
            .mount(&server)
            .await;

        let client = CtClient::new(
            format!("{}/", server.uri()),
            Arc::new(MockTokenStore::new(Some(stored("access", "r")))),
        )
        .unwrap();
        let identity = client.whoami().await.unwrap();
        assert_eq!(identity.workspace_name, "Production");
    }

    #[tokio::test]
    async fn exchange_code_surfaces_a_non_400_status() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/login/cli/token"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;

        let client = CtClient::new(server.uri(), Arc::new(MockTokenStore::new(None))).unwrap();
        let err = client
            .exchange_code("one-time-code", &valid_verifier())
            .await
            .unwrap_err();
        assert!(
            matches!(err, CtError::Api { status: 404, .. }),
            "got {err:?}"
        );
    }

    // CA-CLI-4: the backend's one generic 400 collapses to a generic auth error.
    #[tokio::test]
    async fn ca_cli_4_exchange_generic_400_maps_to_auth() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/login/cli/token"))
            .respond_with(
                ResponseTemplate::new(400)
                    .set_body_json(api_error_json("validation_error", "invalid")),
            )
            .mount(&server)
            .await;

        let client = CtClient::new(server.uri(), Arc::new(MockTokenStore::new(None))).unwrap();
        let err = client
            .exchange_code("one-time-code", &valid_verifier())
            .await
            .unwrap_err();
        assert!(matches!(err, CtError::Auth(_)), "got {err:?}");
    }

    #[tokio::test]
    async fn logout_all_revokes_every_workspace_before_clearing_local_credentials() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/login/logout"))
            .and(wiremock::matchers::header_exists("api-version"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "message": "Logged out successfully"
            })))
            .expect(2)
            .mount(&server)
            .await;
        let mut first = stored("one", "refresh-one");
        first.workspace_id = Some(Uuid::from_u128(1));
        let mut second = stored("two", "refresh-two");
        second.workspace_id = Some(Uuid::from_u128(2));
        let store = Arc::new(MultiLogoutStore::new(vec![first, second]));
        let client = CtClient::new(server.uri(), store.clone()).unwrap();

        client.logout_all().await.unwrap();

        assert!(store.cleared.load(Ordering::SeqCst));
        assert!(store.load_all().unwrap().is_empty());
    }

    #[tokio::test]
    async fn logout_all_clears_local_credentials_and_reports_failed_revocations() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/login/logout"))
            .respond_with(ResponseTemplate::new(503))
            .expect(2)
            .mount(&server)
            .await;
        let mut first = stored("one", "refresh-one");
        first.workspace_id = Some(Uuid::from_u128(1));
        let mut second = stored("two", "refresh-two");
        second.workspace_id = Some(Uuid::from_u128(2));
        let store = Arc::new(MultiLogoutStore::new(vec![first, second]));
        let client = CtClient::new(server.uri(), store.clone()).unwrap();

        let error = client.logout_all().await.unwrap_err();

        assert!(matches!(error, CtError::Logout(message) if message.contains("2 of 2")));
        assert!(store.cleared.load(Ordering::SeqCst));
        assert!(store.load_all().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_403_keeps_the_server_reason_and_leaves_the_login_usable() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/cli/whoami"))
            .respond_with(ResponseTemplate::new(403).set_body_json(serde_json::json!({
                "detail": "Unauthorized workspace access"
            })))
            .mount(&server)
            .await;
        let store = Arc::new(MockTokenStore::new(Some(stored("live", "refresh"))));
        let client = CtClient::new(server.uri(), store).unwrap();

        let error = client.whoami().await.unwrap_err();

        assert!(
            matches!(&error, CtError::Api { status: 403, detail: Some(detail) }
                if detail.starts_with("Unauthorized workspace access; ask a workspace admin")),
            "{error:?}"
        );
        assert_eq!(
            client.credential_provenance().unwrap(),
            CredentialProvenance::Present(crate::CredentialSource::Stored)
        );
    }

    #[tokio::test]
    async fn a_device_poll_server_error_waits_instead_of_ending_the_login() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/login/cli/device/token"))
            .respond_with(ResponseTemplate::new(503).insert_header("retry-after", "7"))
            .mount(&server)
            .await;
        let client = CtClient::new(server.uri(), Arc::new(MockTokenStore::new(None))).unwrap();

        let polled = client.poll_device_token(&"d".repeat(43)).await.unwrap();

        assert!(matches!(
            polled,
            DeviceTokenPoll::Unavailable(delay) if delay == Duration::from_secs(7)
        ));
    }

    #[test]
    fn login_command_names_every_host_but_the_default() {
        let login = |url| login_command_for(url, DEFAULT_BASE_URL);
        assert_eq!(login(DEFAULT_BASE_URL), "cloudthinker login");
        assert_eq!(
            login("https://app.cloudthinker.io:443/"),
            "cloudthinker login"
        );
        assert_eq!(
            login("https://dev.cloudthinker.io"),
            "cloudthinker login --url https://dev.cloudthinker.io"
        );
        assert_eq!(
            login("https://dev.cloudthinker.io:443"),
            "cloudthinker login --url https://dev.cloudthinker.io"
        );
        assert_eq!(
            login("http://127.0.0.1:8091"),
            "cloudthinker login --url http://127.0.0.1:8091"
        );
        assert_eq!(login("origin"), "cloudthinker login");
    }

    #[test]
    fn login_command_measures_against_the_remembered_origin() {
        let remembered = "https://dev.cloudthinker.io";
        assert_eq!(
            login_command_for("https://dev.cloudthinker.io:443", remembered),
            "cloudthinker login"
        );
        assert_eq!(
            login_command_for(DEFAULT_BASE_URL, remembered),
            "cloudthinker login --url https://app.cloudthinker.io"
        );
        assert_eq!(
            login_command_for("http://127.0.0.1:8091", remembered),
            "cloudthinker login --url http://127.0.0.1:8091"
        );
    }

    // CA-CLI-19: device start preserves the URL, human code, TTL, and pacing
    // contract without requiring an existing bearer token.
    #[tokio::test]
    async fn ca_cli_19_device_start_maps_authorization_contract() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/login/cli/device/start"))
            .and(wiremock::matchers::body_bytes(Vec::<u8>::new()))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "device_code": "d".repeat(43),
                "user_code": "BCDF-GHJK",
                "verification_uri": format!("{}/auth/cli", server.uri()),
                "expires_in": 600,
                "interval": 5,
            })))
            .mount(&server)
            .await;

        let client = CtClient::new(server.uri(), Arc::new(MockTokenStore::new(None))).unwrap();
        let authorization = client.start_device_authorization().await.unwrap();

        assert_eq!(authorization.user_code, "BCDF-GHJK");
        assert_eq!(authorization.expires_in, Duration::from_secs(600));
        assert_eq!(authorization.interval, Duration::from_secs(5));
    }

    // CA-CLI-20: pending and slow-down stay non-terminal and carry the next
    // server-mandated delay; the command never has to parse an error body.
    #[tokio::test]
    async fn ca_cli_20_device_poll_maps_pending_and_slow_down() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/login/cli/device/token"))
            .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
                "error": "authorization_pending",
                "interval": 5,
            })))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v1/login/cli/device/token"))
            .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
                "error": "slow_down",
                "interval": 10,
            })))
            .mount(&server)
            .await;

        let client = CtClient::new(server.uri(), Arc::new(MockTokenStore::new(None))).unwrap();
        let pending = client.poll_device_token(&"d".repeat(43)).await.unwrap();
        let slowed = client.poll_device_token(&"d".repeat(43)).await.unwrap();

        assert!(matches!(
            pending,
            DeviceTokenPoll::Pending(delay) if delay == Duration::from_secs(5)
        ));
        assert!(matches!(
            slowed,
            DeviceTokenPoll::SlowDown(delay) if delay == Duration::from_secs(10)
        ));
    }

    // CA-CLI-21: approval returns the same stored-token shape as PKCE; denial
    // remains a stable terminal login error.
    #[tokio::test]
    async fn ca_cli_21_device_poll_maps_approval_and_denial() {
        let approved_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/login/cli/device/token"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(token_json("device-access", "device-refresh")),
            )
            .mount(&approved_server)
            .await;
        let approved_client =
            CtClient::new(approved_server.uri(), Arc::new(MockTokenStore::new(None))).unwrap();
        let approved = approved_client
            .poll_device_token(&"d".repeat(43))
            .await
            .unwrap();
        assert!(matches!(
            approved,
            DeviceTokenPoll::Token(token) if token.access_token == "device-access"
        ));

        let denied_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/login/cli/device/token"))
            .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
                "error": "access_denied",
            })))
            .mount(&denied_server)
            .await;
        let denied_client =
            CtClient::new(denied_server.uri(), Arc::new(MockTokenStore::new(None))).unwrap();
        let denied = denied_client
            .poll_device_token(&"d".repeat(43))
            .await
            .unwrap_err();
        assert!(matches!(denied, CtError::LoginDenied));
    }

    // CA-CLI-14: the secret-gate 422 returns the backend `detail` verbatim, and
    // maps to the usage exit code upstream.
    #[tokio::test]
    async fn ca_cli_14_submit_secret_gate_422_returns_detail() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/cli/runs"))
            .respond_with(ResponseTemplate::new(422).set_body_json(serde_json::json!({
                "detail": "prompt contains a secret"
            })))
            .mount(&server)
            .await;

        let client = CtClient::new(
            server.uri(),
            Arc::new(MockTokenStore::new(Some(stored("access", "r")))),
        )
        .unwrap();
        let err = client
            .submit_run("here is my aws key", None, None)
            .await
            .unwrap_err();
        match err {
            CtError::Api { status, detail } => {
                assert_eq!(status, 422);
                assert_eq!(detail.as_deref(), Some("prompt contains a secret"));
            }
            other => panic!("expected Api 422, got {other:?}"),
        }
    }

    // [ISSUE-3]: a malformed *success* body (202, but not JSON at all) must not
    // be confused with the 422 secret-gate shape — there's no recognizable
    // `{"detail": ...}` here, so it classifies as a protocol fault, not a
    // usage error a script should treat as bad input.
    #[tokio::test]
    async fn ca_cli_14_malformed_success_body_is_protocol_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/cli/runs"))
            .respond_with(ResponseTemplate::new(202).set_body_string("not json at all"))
            .mount(&server)
            .await;

        let client = CtClient::new(
            server.uri(),
            Arc::new(MockTokenStore::new(Some(stored("access", "r")))),
        )
        .unwrap();
        let err = client
            .submit_run("here is my prompt", None, None)
            .await
            .unwrap_err();
        assert!(matches!(err, CtError::Protocol(_)), "got {err:?}");
    }

    #[tokio::test]
    async fn empty_prompt_is_rejected_before_any_request() {
        let server = MockServer::start().await;
        let client = CtClient::new(
            server.uri(),
            Arc::new(MockTokenStore::new(Some(stored("access", "r")))),
        )
        .unwrap();
        let err = client.submit_run("", None, None).await.unwrap_err();
        assert!(matches!(err, CtError::Usage(_)), "got {err:?}");
    }

    fn expiring_in(seconds: i64) -> StoredToken {
        let mut token = stored("live-access", "r");
        token.expires_at = Some(Utc::now() + chrono::Duration::seconds(seconds));
        token
    }

    #[tokio::test]
    async fn ca_ad_12_provenance_tracks_rejection_and_external_replacement() {
        use crate::auth::store::CredentialSource;
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/cli/whoami"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v1/login/refresh"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;
        let store = Arc::new(MockTokenStore::new(None));
        let client = CtClient::new(server.uri(), store.clone()).unwrap();
        assert_eq!(
            client.credential_provenance().unwrap(),
            CredentialProvenance::Missing
        );
        store.save(&stored("first", "refresh")).unwrap();
        assert_eq!(
            client.credential_provenance().unwrap(),
            CredentialProvenance::Present(CredentialSource::Stored)
        );
        assert!(client.whoami().await.is_err());
        assert_eq!(
            client.credential_provenance().unwrap(),
            CredentialProvenance::Stale(CredentialSource::Stored)
        );
        store.save(&stored("replacement", "refresh")).unwrap();
        assert_eq!(
            client.credential_provenance().unwrap(),
            CredentialProvenance::Present(CredentialSource::Stored)
        );
        assert!(!format!("{:?}", client.credential_provenance().unwrap()).contains("replacement"));
    }

    // `cloudthinker auth token` hands this value to another process, so an
    // access token outside the skew window is returned untouched.
    #[tokio::test]
    async fn access_token_returns_the_stored_token_when_fresh() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/login/refresh"))
            .respond_with(ResponseTemplate::new(200).set_body_json(token_json("rotated", "r2")))
            .expect(0)
            .mount(&server)
            .await;
        let store = Arc::new(MockTokenStore::new(Some(expiring_in(
            PROACTIVE_REFRESH_SKEW_SECS + 300,
        ))));

        let client = CtClient::new(server.uri(), store).unwrap();

        assert_eq!(client.access_token().await.unwrap(), "live-access");
    }

    // Inside the skew window the caller must receive the rotated token, not one
    // that expires between this print and the consumer's first request.
    #[tokio::test]
    async fn access_token_returns_the_rotated_token_inside_the_skew_window() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/login/refresh"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(token_json("rotated-access", "rotated-refresh")),
            )
            .expect(1)
            .mount(&server)
            .await;
        let store = Arc::new(MockTokenStore::new(Some(expiring_in(
            PROACTIVE_REFRESH_SKEW_SECS - 30,
        ))));

        let client = CtClient::new(server.uri(), store).unwrap();

        assert_eq!(client.access_token().await.unwrap(), "rotated-access");
    }

    // No credential is an auth error, which `auth token` reports as exit code 3.
    #[tokio::test]
    async fn access_token_without_a_credential_is_an_auth_error() {
        let server = MockServer::start().await;

        let client = CtClient::new(server.uri(), Arc::new(MockTokenStore::new(None))).unwrap();

        let err = client.access_token().await.unwrap_err();
        assert!(matches!(err, CtError::Auth(_)), "got {err:?}");
    }

    // CA-CLI-18: with a read-only (env) credential, a 401 does NOT trigger a
    // refresh — it surfaces as auth directly.
    #[tokio::test]
    async fn ca_cli_18_env_token_401_does_not_refresh() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(
                "/api/v1/cli/runs/33333333-3333-4333-8333-333333333333",
            ))
            .respond_with(ResponseTemplate::new(401).set_body_json(serde_json::json!({
                "detail": "Authentication required."
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v1/login/refresh"))
            .respond_with(ResponseTemplate::new(200).set_body_json(token_json("x", "y")))
            .expect(0)
            .mount(&server)
            .await;

        let client = CtClient::new(
            server.uri(),
            Arc::new(MockTokenStore::read_only(Some(stored("env-access", "")))),
        )
        .unwrap();
        let run_id = Uuid::parse_str("33333333-3333-4333-8333-333333333333").unwrap();
        let err = client.get_run(run_id).await.unwrap_err();
        assert!(matches!(err, CtError::Auth(_)), "got {err:?}");
    }

    #[tokio::test]
    async fn legacy_401_refreshes_once_then_retries() {
        let server = MockServer::start().await;
        // First GET 401 (once), then 200 succeeded.
        Mock::given(method("GET"))
            .and(path(
                "/api/v1/cli/runs/33333333-3333-4333-8333-333333333333",
            ))
            .respond_with(ResponseTemplate::new(401).set_body_json(serde_json::json!({
                "detail": "Authentication required."
            })))
            .up_to_n_times(1)
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(
                "/api/v1/cli/runs/33333333-3333-4333-8333-333333333333",
            ))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(run_status_json("succeeded", Some("done"))),
            )
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v1/login/refresh"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(token_json("fresh-access", "fresh-r")),
            )
            .expect(1)
            .mount(&server)
            .await;

        let client = CtClient::new(
            server.uri(),
            Arc::new(MockTokenStore::new(Some(stored("stale-access", "r")))),
        )
        .unwrap();
        let run_id = Uuid::parse_str("33333333-3333-4333-8333-333333333333").unwrap();
        let view = client.get_run(run_id).await.unwrap();
        assert_eq!(view.status, RunStatus::Succeeded);
        assert_eq!(view.answer.as_deref(), Some("done"));
    }

    #[tokio::test]
    async fn failed_retry_after_refresh_keeps_the_rotated_credential() {
        use crate::auth::store::TokenStore;

        let server = MockServer::start().await;
        let run_path = "/api/v1/cli/runs/33333333-3333-4333-8333-333333333333";
        Mock::given(method("GET"))
            .and(path(run_path))
            .respond_with(ResponseTemplate::new(401).set_body_json(serde_json::json!({
                "detail": "Authentication required."
            })))
            .up_to_n_times(1)
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(run_path))
            .respond_with(ResponseTemplate::new(503))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v1/login/refresh"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(token_json("fresh-access", "fresh-r")),
            )
            .expect(1)
            .mount(&server)
            .await;

        let store = Arc::new(MockTokenStore::new(Some(stored("stale-access", "r"))));
        let client = CtClient::new(server.uri(), store.clone()).unwrap();
        let run_id = Uuid::parse_str("33333333-3333-4333-8333-333333333333").unwrap();
        assert!(client.get_run(run_id).await.is_err());
        let saved = store.load().unwrap().unwrap();
        assert_eq!(saved.access_token, "fresh-access");
        assert_eq!(saved.refresh_token.as_deref(), Some("fresh-r"));
    }

    // CA-CLI-10 (data half): a SUCCEEDED run surfaces the extracted answer.
    #[tokio::test]
    async fn get_run_maps_succeeded_answer() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(
                "/api/v1/cli/runs/33333333-3333-4333-8333-333333333333",
            ))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(run_status_json("succeeded", Some("the answer"))),
            )
            .mount(&server)
            .await;

        let client = CtClient::new(
            server.uri(),
            Arc::new(MockTokenStore::new(Some(stored("access", "r")))),
        )
        .unwrap();
        let run_id = Uuid::parse_str("33333333-3333-4333-8333-333333333333").unwrap();
        let view = client.get_run(run_id).await.unwrap();
        assert!(view.status.is_terminal());
        assert_eq!(view.answer.as_deref(), Some("the answer"));
    }

    // CA-CLI-16 (404 half): an older detail-only response preserves its status.
    #[tokio::test]
    async fn ca_cli_16_unknown_run_is_404() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(
                "/api/v1/cli/runs/44444444-4444-4444-8444-444444444444",
            ))
            .respond_with(ResponseTemplate::new(404).set_body_json(serde_json::json!({
                "detail": "Run not found"
            })))
            .mount(&server)
            .await;

        let client = CtClient::new(
            server.uri(),
            Arc::new(MockTokenStore::new(Some(stored("access", "r")))),
        )
        .unwrap();
        let run_id = Uuid::parse_str("44444444-4444-4444-8444-444444444444").unwrap();
        let err = client.get_run(run_id).await.unwrap_err();
        match err {
            CtError::Api { status, detail } => {
                assert_eq!(status, 404);
                assert_eq!(detail.as_deref(), Some("Run not found"));
            }
            other => panic!("expected Api 404, got {other:?}"),
        }
    }

    fn review_detail_json() -> serde_json::Value {
        serde_json::json!({
            "id": "33333333-3333-4333-8333-333333333333",
            "created_at": "2026-07-20T00:00:00Z",
            "updated_at": "2026-07-20T00:00:00Z",
            "mr_iid": 42,
            "mr_state": "open",
            "provider": "gitlab",
            "repository_name": "my-repo",
            "repository_path": "group/my-repo",
            "review_status": "review_complete",
            "severity_counts": {"critical": 1, "high": 2, "medium": 0, "low": 0},
            "title": "Fix the bug",
            "verdict": "changes_requested",
            "findings_count": 2,
            "url": "https://gitlab.com/group/my-repo/-/merge_requests/42",
            "findings": [
                {
                    "id": "44444444-4444-4444-8444-444444444444",
                    "finding_index": 0,
                    "issue_title": "possible SQL injection",
                    "issue_description": "unsanitized input reaches the query",
                    "severity": "high",
                    "severity_emoji": "🟠",
                    "provider": "gitlab",
                    "file_path": "app/db.py",
                    "line_number": 10,
                    "category": "security",
                    "resolved": false,
                    "acknowledged": false,
                    "withdrawn": false,
                    "resolved_at": null,
                    "resolved_by": null,
                    "external_comment_id": null,
                    "external_note_id": null,
                    "side": null,
                    "specialist": null,
                    "suggested_fix": null,
                    "comment_posted_at": null,
                    "created_at": "2026-07-20T00:00:00Z",
                    "updated_at": "2026-07-20T00:00:00Z",
                },
                {
                    "id": "55555555-5555-4555-8555-555555555555",
                    "finding_index": 1,
                    "issue_title": "unbounded query",
                    "issue_description": "missing LIMIT clause",
                    "severity": "critical",
                    "severity_emoji": "🔴",
                    "provider": "gitlab",
                    "file_path": "app/db.py",
                    "line_number": 20,
                    "category": "performance",
                    "resolved": false,
                    "acknowledged": false,
                    "withdrawn": false,
                    "resolved_at": null,
                    "resolved_by": null,
                    "external_comment_id": null,
                    "external_note_id": null,
                    "side": null,
                    "specialist": null,
                    "suggested_fix": null,
                    "comment_posted_at": null,
                    "created_at": "2026-07-20T00:00:00Z",
                    "updated_at": "2026-07-20T00:00:00Z",
                },
            ],
        })
    }

    // CA-RV-1: a 200 lookup maps every field, and findings are re-ordered
    // worst-severity first (the fixture lists "high" before "critical").
    #[tokio::test]
    async fn ca_rv_1_lookup_200_maps_fields() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/code-review/merge-requests/lookup"))
            .and(query_param("mr_iid", "42"))
            .and(query_param("project_path", "group/my-repo"))
            .and(query_param("provider", "gitlab"))
            .respond_with(ResponseTemplate::new(200).set_body_json(review_detail_json()))
            .mount(&server)
            .await;

        let client = CtClient::new(
            server.uri(),
            Arc::new(MockTokenStore::new(Some(stored("access", "r")))),
        )
        .unwrap();
        let coords = MrCoordinates {
            provider: MrProvider::Gitlab,
            project_path: "group/my-repo".to_string(),
            mr_iid: 42,
        };
        let view = client.lookup_review(&coords).await.unwrap();

        assert_eq!(view.mr_iid, 42);
        assert_eq!(view.status, ReviewStatus::ReviewComplete);
        assert!(view.status.is_terminal());
        assert_eq!(view.verdict, ReviewVerdict::ChangesRequested);
        assert_eq!(view.findings_count, 2);
        assert_eq!(view.title, "Fix the bug");
        assert_eq!(view.provider, "gitlab");
        assert_eq!(view.repository_path.as_deref(), Some("group/my-repo"));
        assert_eq!(view.severity_counts.critical, 1);
        assert_eq!(view.severity_counts.high, 2);
        assert_eq!(view.findings.len(), 2);
        assert_eq!(view.findings[0].severity, "critical", "worst-first");
        assert_eq!(view.findings[1].severity, "high");
    }

    // CA-RV-SP4: a legacy unknown-coordinate response preserves its 404 status.
    #[tokio::test]
    async fn ca_rv_sp4_unknown_coordinates_is_404() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/code-review/merge-requests/lookup"))
            .respond_with(ResponseTemplate::new(404).set_body_json(serde_json::json!({
                "detail": "Merge request not found"
            })))
            .mount(&server)
            .await;

        let client = CtClient::new(
            server.uri(),
            Arc::new(MockTokenStore::new(Some(stored("access", "r")))),
        )
        .unwrap();
        let coords = MrCoordinates {
            provider: MrProvider::Github,
            project_path: "owner/repo".to_string(),
            mr_iid: 99,
        };
        let err = client.lookup_review(&coords).await.unwrap_err();
        assert!(
            matches!(err, CtError::Api { status: 404, .. }),
            "got {err:?}"
        );
    }

    #[tokio::test]
    async fn cyber_apps_transport_uses_the_allowlisted_appsec_route() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/appsec/apps/"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        let client = CtClient::new(
            server.uri(),
            Arc::new(MockTokenStore::new(Some(stored("access", "r")))),
        )
        .unwrap();
        let err = client.cyber_list_apps().await.unwrap_err();
        assert!(
            matches!(err, CtError::Api { status: 404, .. }),
            "got {err:?}"
        );
    }

    #[tokio::test]
    async fn cyber_bind_run_uses_the_bind_route_and_conversation_body() {
        let run_id = Uuid::parse_str("11111111-1111-4111-8111-111111111111").unwrap();
        let conversation_id = Uuid::parse_str("22222222-2222-4222-8222-222222222222").unwrap();
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path(format!("/api/v1/appsec/runs/{run_id}/bind")))
            .and(body_json(
                serde_json::json!({"conversation_id": conversation_id}),
            ))
            .respond_with(ResponseTemplate::new(409))
            .mount(&server)
            .await;
        let client = CtClient::new(
            server.uri(),
            Arc::new(MockTokenStore::new(Some(stored("access", "r")))),
        )
        .unwrap();

        let err = client
            .cyber_bind_run(run_id, conversation_id)
            .await
            .unwrap_err();

        assert!(
            matches!(err, CtError::Api { status: 409, .. }),
            "got {err:?}"
        );
    }

    #[tokio::test]
    async fn cyber_launch_local_run_forwards_scope_and_propagates_backend_rejection() {
        let app_id = Uuid::from_u128(1);
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path(format!("/api/v1/appsec/apps/{app_id}/runs")))
            .and(body_json(serde_json::json!({
                "execution_host": "local",
                "run_scope": {
                    "include": ["/health"],
                    "exclude": ["/admin"]
                }
            })))
            .respond_with(ResponseTemplate::new(422).set_body_json(serde_json::json!({
                "error": {
                    "code": "validation_error",
                    "message": "invalid run scope"
                }
            })))
            .expect(1)
            .mount(&server)
            .await;
        let client = CtClient::new(
            server.uri(),
            Arc::new(MockTokenStore::new(Some(stored("access", "r")))),
        )
        .unwrap();
        let run_scope = cloudthinker_api::types::ScopeSpec {
            browser_resource_origins: Vec::new(),
            include: vec!["/health".parse().unwrap()],
            exclude: vec!["/admin".parse().unwrap()],
        };

        let err = client
            .cyber_launch_local_run(app_id, None, None, Some(run_scope))
            .await
            .unwrap_err();

        assert!(
            matches!(err, CtError::Api { status: 422, .. }),
            "got {err:?}"
        );
    }

    #[tokio::test]
    async fn cyber_launch_local_run_omits_default_scope() {
        let app_id = Uuid::from_u128(1);
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path(format!("/api/v1/appsec/apps/{app_id}/runs")))
            .and(body_json(serde_json::json!({"execution_host": "local"})))
            .respond_with(ResponseTemplate::new(422))
            .expect(1)
            .mount(&server)
            .await;
        let client = CtClient::new(
            server.uri(),
            Arc::new(MockTokenStore::new(Some(stored("access", "r")))),
        )
        .unwrap();

        let err = client
            .cyber_launch_local_run(app_id, None, None, None)
            .await
            .unwrap_err();

        assert!(
            matches!(err, CtError::Api { status: 422, .. }),
            "got {err:?}"
        );
    }

    #[tokio::test]
    async fn cyber_complete_app_setup_patches_the_allowlisted_app_route() {
        let app_id = Uuid::parse_str("11111111-1111-4111-8111-111111111111").unwrap();
        let server = MockServer::start().await;
        Mock::given(method("PATCH"))
            .and(path(format!("/api/v1/appsec/apps/{app_id}")))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        let client = CtClient::new(
            server.uri(),
            Arc::new(MockTokenStore::new(Some(stored("access", "r")))),
        )
        .unwrap();
        let err = client.cyber_complete_app_setup(app_id).await.unwrap_err();
        assert!(
            matches!(err, CtError::Api { status: 404, .. }),
            "got {err:?}"
        );
    }

    #[test]
    fn observation_batches_cover_each_row_once_and_stay_within_the_limit() {
        let observations: Vec<Observation> = (0..501)
            .map(|index| Observation {
                row_id: format!("row-{index:04}"),
                status: CoverageStatus::Covered,
                reason: String::new(),
                evidence_ref: String::new(),
                worker: "test".into(),
            })
            .collect();
        let batches: Vec<&[Observation]> = observation_batches(&observations).collect();

        assert_eq!(
            batches.iter().map(|batch| batch.len()).collect::<Vec<_>>(),
            [500, 1]
        );
        assert!(batches.iter().all(|batch| !batch.is_empty()));
        assert_eq!(
            batches
                .iter()
                .flat_map(|batch| batch.iter().map(|observation| observation.row_id.as_str()))
                .collect::<Vec<_>>(),
            observations
                .iter()
                .map(|observation| observation.row_id.as_str())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn observation_ingest_rejects_duplicate_row_ids() {
        let observations = vec![
            Observation {
                row_id: "row-1".into(),
                status: CoverageStatus::Covered,
                reason: String::new(),
                evidence_ref: String::new(),
                worker: "test".into(),
            },
            Observation {
                row_id: "row-1".into(),
                status: CoverageStatus::Blocked,
                reason: "failed".into(),
                evidence_ref: String::new(),
                worker: "test".into(),
            },
        ];
        assert!(matches!(
            validate_observation_ids(&observations),
            Err(CtError::Usage(_))
        ));
        let empty_id = [Observation {
            row_id: String::new(),
            status: CoverageStatus::Covered,
            reason: String::new(),
            evidence_ref: String::new(),
            worker: "test".into(),
        }];
        assert!(matches!(
            validate_observation_ids(&empty_id),
            Err(CtError::Usage(_))
        ));
    }

    #[tokio::test]
    async fn cyber_observation_ingest_batches_and_returns_the_complete_last_snapshot() {
        let run_id = Uuid::from_u128(1);
        let plan_id = "plan-1";
        let server = MockServer::start().await;
        let first_rows: Vec<String> = (0..500).map(|index| format!("row-{index:04}")).collect();
        let last_rows = vec!["row-0500".to_string()];
        let request_body = |row_ids: &[String]| {
            serde_json::json!({
                "observations": row_ids.iter().map(|row_id| serde_json::json!({
                    "row_id": row_id,
                    "status": "covered",
                    "reason": "",
                    "evidence_ref": "",
                    "worker": "test",
                })).collect::<Vec<_>>()
            })
        };
        let report_body = |total: usize| {
            serde_json::json!({
                "coverage": {
                    "blockers": [],
                    "plan_id": plan_id,
                    "terminal": total,
                    "total": 501,
                    "unresolved_candidates": [],
                    "untested": [],
                    "valid": total == 501,
                },
                "rows": (0..501).map(|index| serde_json::json!({
                    "asset_type": "endpoint",
                    "evidence_ref": "",
                    "executable": true,
                    "locator": format!("GET /{index}"),
                    "method": "GET",
                    "reason": "",
                    "row_id": format!("row-{index:04}"),
                    "status": "covered",
                    "url": format!("https://example.test/{index}"),
                    "worker": "test",
                })).collect::<Vec<_>>()
            })
        };
        let endpoint = format!("/api/v1/appsec/runs/{run_id}/plan/{plan_id}/observations");
        Mock::given(method("POST"))
            .and(path(endpoint.clone()))
            .and(body_json(request_body(&first_rows)))
            .respond_with(ResponseTemplate::new(200).set_body_json(report_body(500)))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path(endpoint))
            .and(body_json(request_body(&last_rows)))
            .respond_with(ResponseTemplate::new(200).set_body_json(report_body(501)))
            .expect(1)
            .mount(&server)
            .await;

        let client = CtClient::new(
            server.uri(),
            Arc::new(MockTokenStore::new(Some(stored("access", "r")))),
        )
        .unwrap();
        let observations: Vec<Observation> = (0..501)
            .map(|index| Observation {
                row_id: format!("row-{index:04}"),
                status: CoverageStatus::Covered,
                reason: String::new(),
                evidence_ref: String::new(),
                worker: "test".into(),
            })
            .collect();
        let report = client
            .cyber_ingest_observations(run_id, plan_id, observations)
            .await
            .unwrap();

        assert_eq!(report.coverage.total, 501);
        assert_eq!(report.coverage.terminal, 501);
        assert!(report.coverage.valid);
        assert_eq!(report.rows.len(), 501);
    }

    #[tokio::test]
    async fn empty_observation_ingest_reads_coverage_without_posting_an_empty_batch() {
        let run_id = Uuid::from_u128(1);
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(format!("/api/v1/appsec/runs/{run_id}/coverage")))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "coverage": {
                    "blockers": [],
                    "plan_id": "plan-1",
                    "terminal": 0,
                    "total": 501,
                    "unresolved_candidates": [],
                    "untested": (0..501).map(|index| format!("row-{index:04}")).collect::<Vec<_>>(),
                    "valid": false,
                },
                "rows": [],
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path(format!(
                "/api/v1/appsec/runs/{run_id}/plan/plan-1/observations"
            )))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&server)
            .await;
        let client = CtClient::new(
            server.uri(),
            Arc::new(MockTokenStore::new(Some(stored("access", "r")))),
        )
        .unwrap();

        let report = client
            .cyber_ingest_observations(run_id, "plan-1", Vec::new())
            .await
            .unwrap();

        assert_eq!(report.coverage.total, 501);
        assert_eq!(report.coverage.untested.len(), 501);
        assert!(!report.coverage.valid);
    }
}
