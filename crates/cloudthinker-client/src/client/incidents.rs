use std::num::NonZeroU64;

use chrono::{DateTime, Utc};
use cloudthinker_api::types as api;
use serde::Serialize;
use uuid::Uuid;

use super::CtClient;
use crate::error::{CtError, CtResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum IncidentStatus {
    #[serde(rename = "OPEN")]
    Open,
    #[serde(rename = "ACKNOWLEDGED")]
    Acknowledged,
    #[serde(rename = "INVESTIGATING")]
    Investigating,
    #[serde(rename = "IDENTIFIED")]
    Identified,
    #[serde(rename = "ON_HOLD")]
    OnHold,
    #[serde(rename = "NOT_FOUND")]
    NotFound,
    #[serde(rename = "FALSE_ALARM")]
    FalseAlarm,
    #[serde(rename = "RESOLVED")]
    Resolved,
    #[serde(rename = "AUTO_RESOLVED")]
    AutoResolved,
}

impl IncidentStatus {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::NotFound | Self::FalseAlarm | Self::Resolved | Self::AutoResolved
        )
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Acknowledged => "acknowledged",
            Self::Investigating => "investigating",
            Self::Identified => "identified",
            Self::OnHold => "on hold",
            Self::NotFound => "not found",
            Self::FalseAlarm => "false alarm",
            Self::Resolved => "resolved",
            Self::AutoResolved => "auto resolved",
        }
    }
}

impl From<api::IncidentStatus> for IncidentStatus {
    fn from(status: api::IncidentStatus) -> Self {
        match status {
            api::IncidentStatus::Open => Self::Open,
            api::IncidentStatus::Acknowledged => Self::Acknowledged,
            api::IncidentStatus::Investigating => Self::Investigating,
            api::IncidentStatus::Identified => Self::Identified,
            api::IncidentStatus::OnHold => Self::OnHold,
            api::IncidentStatus::NotFound => Self::NotFound,
            api::IncidentStatus::FalseAlarm => Self::FalseAlarm,
            api::IncidentStatus::Resolved => Self::Resolved,
            api::IncidentStatus::AutoResolved => Self::AutoResolved,
        }
    }
}

impl From<IncidentStatus> for api::IncidentStatus {
    fn from(status: IncidentStatus) -> Self {
        match status {
            IncidentStatus::Open => Self::Open,
            IncidentStatus::Acknowledged => Self::Acknowledged,
            IncidentStatus::Investigating => Self::Investigating,
            IncidentStatus::Identified => Self::Identified,
            IncidentStatus::OnHold => Self::OnHold,
            IncidentStatus::NotFound => Self::NotFound,
            IncidentStatus::FalseAlarm => Self::FalseAlarm,
            IncidentStatus::Resolved => Self::Resolved,
            IncidentStatus::AutoResolved => Self::AutoResolved,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct IncidentView {
    pub id: Uuid,
    pub title: String,
    pub severity: String,
    pub status: IncidentStatus,
    pub terminal: bool,
    pub created_at: DateTime<Utc>,
    pub resolved_at: Option<DateTime<Utc>>,
}

impl From<api::IncidentPublic> for IncidentView {
    fn from(incident: api::IncidentPublic) -> Self {
        let status = IncidentStatus::from(incident.status);
        Self {
            id: incident.id,
            title: incident.title.to_string(),
            severity: incident
                .severity
                .map_or_else(|| "-".to_string(), |severity| severity.to_string()),
            status,
            terminal: status.is_terminal(),
            created_at: incident.created_at,
            resolved_at: incident.resolved_at,
        }
    }
}

impl CtClient {
    pub async fn list_incidents(
        &self,
        status: Option<IncidentStatus>,
        limit: u64,
    ) -> CtResult<Vec<IncidentView>> {
        let limit = NonZeroU64::new(limit)
            .ok_or_else(|| CtError::Usage("--limit must be between 1 and 100".into()))?;
        let statuses: Option<Vec<api::IncidentStatus>> = status.map(|s| vec![s.into()]);
        let page = self
            .authed(async |c: cloudthinker_api::Client| {
                c.incidents_list_incidents(
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                    Some(limit),
                    None,
                    None,
                    None,
                    None,
                    None,
                    statuses.as_ref(),
                    None,
                    None,
                )
                .await
            })
            .await?;
        Ok(page.data.into_iter().map(IncidentView::from).collect())
    }

    pub async fn get_incident(&self, incident_id: Uuid) -> CtResult<IncidentView> {
        let incident = self
            .authed(async |c: cloudthinker_api::Client| {
                c.incidents_get_incident(&incident_id, None).await
            })
            .await?;
        Ok(IncidentView::from(incident))
    }
}
