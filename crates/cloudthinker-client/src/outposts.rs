use uuid::Uuid;

use crate::{CtClient, CtError, CtResult, worker_types as api};

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

    pub async fn archive_outpost(&self, target: Uuid) -> CtResult<()> {
        self.authed(async |c| c.executor_targets_archive_outpost(&target).await)
            .await
    }
}
