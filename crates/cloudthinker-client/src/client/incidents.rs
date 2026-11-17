use chrono::{DateTime, Utc};
use cloudthinker_spec::Method;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::CtClient;
use super::public::QueryValue;
use crate::error::{CtError, CtResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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

    fn wire(self) -> &'static str {
        match self {
            Self::Open => "OPEN",
            Self::Acknowledged => "ACKNOWLEDGED",
            Self::Investigating => "INVESTIGATING",
            Self::Identified => "IDENTIFIED",
            Self::OnHold => "ON_HOLD",
            Self::NotFound => "NOT_FOUND",
            Self::FalseAlarm => "FALSE_ALARM",
            Self::Resolved => "RESOLVED",
            Self::AutoResolved => "AUTO_RESOLVED",
        }
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

#[derive(Deserialize)]
struct PublicIncident {
    id: Uuid,
    title: String,
    severity: String,
    status: IncidentStatus,
    created_at: DateTime<Utc>,
    resolved_at: Option<DateTime<Utc>>,
}

#[derive(Deserialize)]
struct PublicIncidentPage {
    items: Vec<PublicIncident>,
}

impl From<PublicIncident> for IncidentView {
    fn from(incident: PublicIncident) -> Self {
        Self {
            id: incident.id,
            title: incident.title,
            severity: incident.severity,
            terminal: incident.status.is_terminal(),
            status: incident.status,
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
        if !(1..=100).contains(&limit) {
            return Err(CtError::Usage("--limit must be between 1 and 100".into()));
        }
        let mut query = vec![QueryValue {
            name: "limit".into(),
            value: limit.to_string(),
        }];
        if let Some(status) = status {
            query.push(QueryValue {
                name: "status".into(),
                value: status.wire().into(),
            });
        }
        let page: PublicIncidentPage = self
            .public_json(Method::Get, "/resolve/incidents", &query, None, None)
            .await?;
        Ok(page.items.into_iter().map(IncidentView::from).collect())
    }

    pub async fn get_incident(&self, incident_id: Uuid) -> CtResult<IncidentView> {
        let incident: PublicIncident = self
            .public_json(
                Method::Get,
                &format!("/resolve/incidents/{incident_id}"),
                &[],
                None,
                None,
            )
            .await?;
        Ok(IncidentView::from(incident))
    }
}
