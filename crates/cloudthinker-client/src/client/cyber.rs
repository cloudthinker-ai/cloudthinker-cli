use std::num::NonZeroU64;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::Serialize;
use uuid::Uuid;

use super::CtClient;
use crate::error::{CtError, CtResult};

const MAX_OBSERVATIONS_PER_BATCH: usize = 500;
// One plan check against the target. Shorter than an API call: a hung target
// must not stall the whole plan, and a timeout is a truthful `blocked` row.
const PROBE_TIMEOUT_SECS: u64 = 20;

pub(super) fn observation_batches<'a>(
    observations: &'a [Observation],
) -> impl Iterator<Item = &'a [Observation]> + 'a {
    observations.chunks(MAX_OBSERVATIONS_PER_BATCH)
}

pub(super) fn validate_observation_ids(observations: &[Observation]) -> CtResult<()> {
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

/// Lifecycle of an Cyber run — running | success | failed | cancelled.
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
    fn from_api(value: cloudthinker_api::types::CyberDomainPublic) -> Self {
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
    pub report_preferences: cloudthinker_api::types::CyberReportPreferences,
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

impl CtClient {
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
            advisor_enabled: None,
            execution_host: Some(cloudthinker_api::types::RunExecutionHost::Local),
            intensity: intensity.map(CyberIntensity::into_api),
            jev_enabled: None,
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
            .authed(async |c: cloudthinker_api::Client| c.appsec_list_apps(None, None).await)
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
            default_jev_enabled: None,
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
        let body = cloudthinker_api::types::CyberDomainCreate {
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
                    None,
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
}
