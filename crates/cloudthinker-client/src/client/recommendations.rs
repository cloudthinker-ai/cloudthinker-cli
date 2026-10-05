use std::num::NonZeroU64;

use chrono::{DateTime, Utc};
use cloudthinker_api::types as api;
use serde::Serialize;
use uuid::Uuid;

use super::CtClient;
use crate::error::{CtError, CtResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum RecommendationStatus {
    #[serde(rename = "pending")]
    Pending,
    #[serde(rename = "in_progress")]
    InProgress,
    #[serde(rename = "implemented")]
    Implemented,
    #[serde(rename = "ignored")]
    Ignored,
}

impl RecommendationStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::InProgress => "in progress",
            Self::Implemented => "implemented",
            Self::Ignored => "ignored",
        }
    }

    fn wire(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::InProgress => "in_progress",
            Self::Implemented => "implemented",
            Self::Ignored => "ignored",
        }
    }
}

impl From<api::RecommendationStatus> for RecommendationStatus {
    fn from(status: api::RecommendationStatus) -> Self {
        match status {
            api::RecommendationStatus::Pending => Self::Pending,
            api::RecommendationStatus::InProgress => Self::InProgress,
            api::RecommendationStatus::Implemented => Self::Implemented,
            api::RecommendationStatus::Ignored => Self::Ignored,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct RecommendationView {
    pub id: Uuid,
    pub title: String,
    pub status: RecommendationStatus,
    pub potential_savings: Option<f64>,
    pub effort: String,
    pub risk: String,
    pub resource_name: Option<String>,
    pub resource_type: Option<String>,
    pub created_at: DateTime<Utc>,
}

impl From<api::RecommendationListPublic> for RecommendationView {
    fn from(recommendation: api::RecommendationListPublic) -> Self {
        Self {
            id: recommendation.id,
            title: recommendation.title.to_string(),
            status: recommendation
                .status
                .map_or(RecommendationStatus::Pending, RecommendationStatus::from),
            potential_savings: recommendation.potential_savings,
            effort: recommendation.effort.to_string(),
            risk: recommendation.risk.to_string(),
            resource_name: recommendation.resource_name,
            resource_type: recommendation.resource_type,
            created_at: recommendation.created_at,
        }
    }
}

impl CtClient {
    pub async fn list_recommendations(
        &self,
        status: Option<RecommendationStatus>,
        limit: u64,
    ) -> CtResult<Vec<RecommendationView>> {
        let limit = NonZeroU64::new(limit)
            .ok_or_else(|| CtError::Usage("--limit must be between 1 and 100".into()))?;
        let statuses: Option<Vec<String>> = status.map(|s| vec![s.wire().to_string()]);
        let page = self
            .authed(async |c: cloudthinker_api::Client| {
                c.recommendations_read_recommendations(
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
                    None,
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
        Ok(page
            .data
            .into_iter()
            .map(RecommendationView::from)
            .collect())
    }
}
