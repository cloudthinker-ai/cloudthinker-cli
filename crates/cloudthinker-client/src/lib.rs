//! `cloudthinker-client` — the only hand-written crate that knows the wire.
//!
//! Everything the CLI needs to talk to CloudThinker: the typed `CtClient`, PKCE
//! login, token storage/refresh, and the `CtError` taxonomy. Future TUI/MCP
//! surfaces reuse this crate unchanged.

mod agent_release;
pub mod auth;
mod cli_config;
mod client;
mod cloud;
pub use cloud::{CloudExecutionInput, CloudOutcome, CloudResult};
pub mod cyber_run_guard;
mod discovery;
mod error;
mod outposts;
mod retry;
mod review_url;
mod toolpack;
pub mod worker_api;
pub use cloudthinker_api::types as worker_types;
pub use cloudthinker_api::types as cloud_types;
mod update_cache;

#[cfg(test)]
mod test_support;

pub use agent_release::{
    DEFAULT_RELEASE_BASE_URL, InstalledAgent, agent_bin_root, any_agent_installed,
    host_target_triple, install_agent, installed_agent_binary,
};
pub use auth::device::wait_for_device_token;
pub use auth::pkce::{Loopback, PkceChallenge, consent_url};
pub use auth::refresh::{PROACTIVE_REFRESH_SKEW_SECS, RefreshCoordinator};
pub use auth::store::{
    CredentialProvenance, CredentialSource, EnvTokenStore, FileStore, StoredToken, StoredWorkspace,
    TOKEN_ENV_VAR, TokenStore, WorkspaceSelector,
};
pub use cli_config::{CliConfig, cli_config_path, effective_default_url, resolve_base_url};
pub use client::{
    CliIdentity, Coverage, CoverageReport, CoverageRow, CoverageStatus, CtClient, CyberApp,
    CyberDiscoveryTarget, CyberDomain, CyberExecutionHost, CyberExport, CyberFinding,
    CyberFindingPage, CyberFindingStatus, CyberIntensity, CyberMemoryContextSource,
    CyberMemoryFile, CyberMemorySnapshot, CyberMode, CyberRun, CyberRunBrief, CyberRunResult,
    CyberSessionEntry, CyberSeverity, CyberTriageState, DEFAULT_BASE_URL, DeviceAuthorization,
    DeviceTokenPoll, EvidenceFile, EvidenceReceipt, EvidenceSkipped, FindingFilter,
    LocalCyberWorkspace, LogoutOutcome, Observation, Partition, PlanCheck, ProbeOutcome,
    ReviewFinding, ReviewSeverityCounts, ReviewStatus, ReviewVerdict, ReviewView, RunListItem,
    RunStatus, RunView, SettleResult, Shard, SubmittedRun, Surface, WorkPlan, login_command,
    origin_of, persistent_store, resolve_store,
};
pub use cyber_run_guard::{wait_for_run_stop, while_run_running};
pub use discovery::{
    CyberDiscoveryArtifacts, CyberDiscoveryCollectorHealth, CyberDiscoveryHealth,
    CyberDiscoveryManifest, cyber_discovery_artifacts, cyber_discovery_auth_value,
    cyber_discovery_identity_manifest, cyber_discovery_redact,
};
pub use error::{CtError, CtResult, is_retryable_status};
pub use review_url::{MrCoordinates, MrProvider, parse_mr_url};
pub use toolpack::{install_tool, installed_tool_binary, tool_install_dir, tools_bin_root};
pub use update_cache::{UpdateCache, update_cache_path};
