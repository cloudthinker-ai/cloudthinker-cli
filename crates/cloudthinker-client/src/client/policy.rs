use std::future::poll_fn;
use std::time::Duration;

use cloudthinker_api::types as api;
use serde::Serialize;

use super::CtClient;
use crate::error::{CtError, CtResult};

const POLICY_REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
pub const POLICY_DOCUMENT_MAX_BYTES: usize = 1_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyImportMode {
    Plan,
    Apply,
}

impl PolicyImportMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Plan => "plan",
            Self::Apply => "apply",
        }
    }

    fn wire(self) -> api::ImportMode {
        match self {
            Self::Plan => api::ImportMode::Plan,
            Self::Apply => api::ImportMode::Apply,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct PolicyImportOptions {
    pub mode: PolicyImportMode,
    pub prune: bool,
    pub activate: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct PolicyImportReport {
    pub mode: PolicyImportMode,
    pub applied: bool,
    pub counts: PolicyImportCounts,
    pub rules: Vec<PolicyImportRule>,
    pub sections: Vec<PolicyImportSection>,
    pub errors: Vec<PolicyImportIssue>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PolicyImportCounts {
    pub created: i64,
    pub changed: i64,
    pub removed: i64,
    pub unchanged: i64,
    pub activated: i64,
    pub invalid: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct PolicyImportRule {
    pub slug: String,
    pub change: String,
    pub effect: String,
    pub sentence: String,
    pub becomes: String,
    pub fields: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PolicyImportSection {
    pub section: String,
    pub key: String,
    pub change: String,
    pub before: Option<String>,
    pub after: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PolicyImportIssue {
    pub code: String,
    pub message: String,
    pub path: String,
    pub line: Option<i64>,
    pub slug: Option<String>,
    pub column: Option<i64>,
    pub allowed_values: Vec<String>,
}

impl From<api::PolicyImportPublic> for PolicyImportReport {
    fn from(report: api::PolicyImportPublic) -> Self {
        Self {
            mode: match report.mode {
                api::ImportMode::Plan => PolicyImportMode::Plan,
                api::ImportMode::Apply => PolicyImportMode::Apply,
            },
            applied: report.applied,
            counts: PolicyImportCounts {
                created: report.counts.created,
                changed: report.counts.changed,
                removed: report.counts.removed,
                unchanged: report.counts.unchanged,
                activated: report.counts.activated,
                invalid: report.counts.invalid,
            },
            rules: report
                .rules
                .into_iter()
                .map(|rule| PolicyImportRule {
                    slug: rule.slug,
                    change: rule.change.to_string(),
                    effect: rule.effect.to_string(),
                    sentence: rule.sentence,
                    becomes: rule.becomes.to_string(),
                    fields: rule.fields,
                })
                .collect(),
            sections: report
                .sections
                .into_iter()
                .map(|section| PolicyImportSection {
                    section: section.section,
                    key: section.key,
                    change: section.change.to_string(),
                    before: section.before,
                    after: section.after,
                })
                .collect(),
            errors: report
                .errors
                .into_iter()
                .map(|issue| PolicyImportIssue {
                    code: issue.code,
                    message: issue.message,
                    path: issue.path,
                    line: issue.line,
                    slug: issue.slug,
                    column: issue.column,
                    allowed_values: issue.allowed_values,
                })
                .collect(),
        }
    }
}

impl CtClient {
    pub async fn export_policy(&self) -> CtResult<String> {
        let response = self
            .authed_with_timeout(
                POLICY_REQUEST_TIMEOUT,
                async |client: cloudthinker_api::Client| client.policy_export_policy(None).await,
            )
            .await?;
        let mut stream = response.into_inner();
        let mut bytes = Vec::new();
        while let Some(chunk) = poll_fn(|context| stream.as_mut().poll_next(context)).await {
            let chunk = chunk.map_err(|_| {
                CtError::Transport(
                    "Policy export download did not complete; retry the export".into(),
                )
            })?;
            bytes.extend_from_slice(&chunk);
        }
        let yaml = String::from_utf8(bytes)
            .map_err(|_| CtError::Protocol("Policy export is not UTF-8 YAML".into()))?;
        if yaml.trim().is_empty() {
            return Err(CtError::Protocol("Policy export was empty".into()));
        }
        Ok(yaml)
    }

    pub async fn import_policy(
        &self,
        content: &str,
        options: PolicyImportOptions,
    ) -> CtResult<PolicyImportReport> {
        if content.len() > POLICY_DOCUMENT_MAX_BYTES {
            return Err(CtError::Usage(format!(
                "Policy document exceeds {POLICY_DOCUMENT_MAX_BYTES} bytes"
            )));
        }
        let body = api::PolicyImportBody {
            content: content.parse().map_err(|_| {
                CtError::Usage("Policy document exceeds the supported content limit".into())
            })?,
            prune: options.prune,
            activate: options.activate,
        };
        let response = self
            .authed_with_timeout(
                POLICY_REQUEST_TIMEOUT,
                async |client: cloudthinker_api::Client| {
                    client
                        .policy_import_policy(Some(options.mode.wire()), None, &body)
                        .await
                },
            )
            .await?;
        let report = PolicyImportReport::from(response);
        if report.mode != options.mode {
            return Err(CtError::Protocol(format!(
                "Policy import returned mode {} for a {} request",
                report.mode.label(),
                options.mode.label()
            )));
        }
        if report.applied && options.mode == PolicyImportMode::Plan {
            return Err(CtError::Protocol(
                "the server reported an applied Policy for a preview request; inspect the current Policy".into(),
            ));
        }
        if report.applied && (!report.errors.is_empty() || report.counts.invalid > 0) {
            return Err(CtError::Protocol(
                "the server reported an applied Policy with validation issues; inspect the current Policy".into(),
            ));
        }
        Ok(report)
    }
}
