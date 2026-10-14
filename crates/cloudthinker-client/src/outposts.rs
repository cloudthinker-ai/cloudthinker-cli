use cloudthinker_api::{ClientInfo, Error as ApiError, ResponseValue};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{CtClient, CtError, CtResult, worker_types as api};

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct OutpostStatus {
    #[serde(flatten)]
    pub choice: api::ExecutorChoicePublic,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cli_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cli_version_status: Option<String>,
}

impl CtClient {
    pub async fn create_outpost(
        &self,
        name: &str,
        shared: bool,
    ) -> CtResult<api::OutpostCreatedPublic> {
        let identity = self.whoami().await?;
        let body = api::CreateOutpostRequest {
            name: name.parse().map_err(|_| {
                CtError::Usage("outpost name must contain 1–255 printable characters".into())
            })?,
            shared,
        };
        self.authed(async |c| {
            c.executor_targets_create_outpost(&identity.workspace_id, &body)
                .await
        })
        .await
    }

    pub async fn list_outposts(&self) -> CtResult<Vec<api::ExecutorChoicePublic>> {
        let identity = self.whoami().await?;
        self.authed(async |c| {
            c.executor_targets_list_executor_targets(&identity.workspace_id, None)
                .await
        })
        .await
    }

    pub async fn list_outpost_status(&self) -> CtResult<Vec<OutpostStatus>> {
        let identity = self.whoami().await?;
        self.authed(async |c| {
            let url = format!(
                "{}/api/v1/workspaces/{}/executor-targets",
                c.baseurl(),
                identity.workspace_id
            );
            let response = c
                .client()
                .get(url)
                .header(reqwest::header::ACCEPT, "application/json")
                .header("api-version", "0.1.0")
                .send()
                .await
                .map_err(ApiError::CommunicationError)?;
            match response.status().as_u16() {
                200 => ResponseValue::from_response(response).await,
                _ => Err(ApiError::UnexpectedResponse(response)),
            }
        })
        .await
    }

    pub async fn archive_outpost(&self, target: Uuid) -> CtResult<()> {
        self.authed(async |c| c.executor_targets_archive_outpost(&target).await)
            .await
    }
}
