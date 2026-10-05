use std::num::NonZeroU64;
use std::time::Duration;

use cloudthinker_api::types as api;
use serde::Serialize;
use uuid::Uuid;

use crate::{CtClient, CtError, CtResult};

pub struct CloudExecutionInput {
    pub session: Uuid,
    pub script: String,
    pub connections: Vec<String>,
    pub timeout: u16,
    pub background: bool,
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum CloudResult {
    Session(api::AgentCliSessionCreated),
    Connections(api::AgentCliConnectionsContext),
    Content(api::LoadedConnectionContent),
    Read {
        conversation_id: Uuid,
        #[serde(flatten)]
        execution: api::ResponseAgentCliExecuteAgentCliRead,
    },
    Task {
        conversation_id: Uuid,
        task_id: String,
        #[serde(flatten)]
        output: api::AgentCliExecutionOutput,
    },
    Write(api::AgentCliWriteOutcome),
    WriteStatus(api::AgentCliWritePublic),
    Writes(api::AgentCliWritePage),
}

pub enum CloudOutcome {
    Success,
    Failed,
    Approval,
}

impl CloudResult {
    pub fn outcome(&self) -> CloudOutcome {
        match self {
            Self::Read {
                execution: api::ResponseAgentCliExecuteAgentCliRead::Completed(value),
                ..
            } if value.return_code != 0 => CloudOutcome::Failed,
            Self::Task { output, .. }
                if output.exit_code.is_some_and(|code| code != 0)
                    || matches!(
                        output.status,
                        api::BgTaskStatus::Error
                            | api::BgTaskStatus::Cancelled
                            | api::BgTaskStatus::Unknown
                    ) =>
            {
                CloudOutcome::Failed
            }
            Self::Write(value) => write_outcome(&value.write),
            Self::WriteStatus(value) => write_outcome(value),
            _ => CloudOutcome::Success,
        }
    }
}

fn write_outcome(write: &api::AgentCliWritePublic) -> CloudOutcome {
    if matches!(
        write.status,
        api::AgentCliWriteStatus::RequiredApproval | api::AgentCliWriteStatus::Approved
    ) && write.expires_at <= chrono::Utc::now()
    {
        return CloudOutcome::Failed;
    }
    match write.status {
        api::AgentCliWriteStatus::RequiredApproval => CloudOutcome::Approval,
        api::AgentCliWriteStatus::Declined
        | api::AgentCliWriteStatus::Denied
        | api::AgentCliWriteStatus::Failed
        | api::AgentCliWriteStatus::OutcomeUnknown => CloudOutcome::Failed,
        _ if write.return_code.is_some_and(|code| code != 0) => CloudOutcome::Failed,
        _ => CloudOutcome::Success,
    }
}

impl CtClient {
    pub async fn cloud_create_session(
        &self,
        cwd: &str,
        title: Option<&str>,
    ) -> CtResult<CloudResult> {
        let body = api::CreateAgentCliSessionRequest {
            selected_agent_reference: None,
            cwd: cwd.parse().map_err(|_| {
                CtError::Usage("Working directory must contain 1–4096 characters.".into())
            })?,
            title: title
                .map(str::parse)
                .transpose()
                .map_err(|_| CtError::Usage("Title must contain at most 255 characters.".into()))?,
            source_conversation_id: None,
            skip_sandbox_warmup: false,
        };
        self.authed(async |client| client.agent_cli_create_agent_cli_session(None, &body).await)
            .await
            .map(CloudResult::Session)
    }

    pub async fn cloud_connections(&self) -> CtResult<CloudResult> {
        self.authed_with_timeout(Duration::from_secs(180), async |client| {
            client.agent_cli_read_agent_cli_connections(None).await
        })
        .await
        .map(CloudResult::Connections)
    }

    pub async fn cloud_load_skill(
        &self,
        session: Uuid,
        prefix: &str,
        names: Vec<String>,
    ) -> CtResult<CloudResult> {
        let body = api::LoadConnectionSkillRequest {
            conversation_id: session,
            connection_prefix: prefix
                .parse()
                .map_err(|_| CtError::Usage("Invalid Connection prefix.".into()))?,
            skill_names: names,
        };
        self.authed_with_timeout(Duration::from_secs(180), async |client| {
            client.agent_cli_load_agent_cli_skill(None, &body).await
        })
        .await
        .map(CloudResult::Content)
    }

    pub async fn cloud_load_tools(
        &self,
        session: Uuid,
        prefix: &str,
        tools: Vec<String>,
    ) -> CtResult<CloudResult> {
        let body = api::LoadConnectionToolsRequest {
            conversation_id: session,
            connection_prefix: prefix
                .parse()
                .map_err(|_| CtError::Usage("Invalid Connection prefix.".into()))?,
            tools,
        };
        self.authed_with_timeout(Duration::from_secs(180), async |client| {
            client.agent_cli_load_agent_cli_tools(None, &body).await
        })
        .await
        .map(CloudResult::Content)
    }

    pub async fn cloud_read(&self, input: CloudExecutionInput) -> CtResult<CloudResult> {
        let body = api::ExecuteAgentCliReadRequest {
            conversation_id: input.session,
            connection_list: input.connections,
            script: input
                .script
                .parse()
                .map_err(|_| CtError::Usage("Script is empty.".into()))?,
            timeout: timeout(input.timeout)?,
            run_in_background: input.background,
        };
        let execution = self
            .authed_with_timeout(Duration::from_secs(180), async |client| {
                client.agent_cli_execute_agent_cli_read(None, &body).await
            })
            .await?;
        Ok(CloudResult::Read {
            conversation_id: input.session,
            execution,
        })
    }

    pub async fn cloud_task_status(
        &self,
        session: Uuid,
        task: &str,
        since: u64,
    ) -> CtResult<CloudResult> {
        let output = self
            .authed(async |client| {
                client
                    .agent_cli_read_agent_cli_execution(task, &session, Some(since), None)
                    .await
            })
            .await?;
        Ok(CloudResult::Task {
            conversation_id: session,
            task_id: task.into(),
            output,
        })
    }

    pub async fn cloud_write_status(&self, session: Uuid, write: Uuid) -> CtResult<CloudResult> {
        self.authed(async |client| {
            client
                .agent_cli_read_agent_cli_write(&write, Some(&session), None)
                .await
        })
        .await
        .map(CloudResult::WriteStatus)
    }

    pub async fn cloud_writes(&self, session: Uuid) -> CtResult<CloudResult> {
        self.authed(async |client| {
            client
                .agent_cli_list_agent_cli_session_writes(&session, None, None)
                .await
        })
        .await
        .map(CloudResult::Writes)
    }

    pub async fn cloud_run_write(&self, session: Uuid, write: Uuid) -> CtResult<CloudResult> {
        self.authed_with_timeout(Duration::from_secs(180), async |client| {
            client
                .agent_cli_run_agent_cli_write(&write, Some(&session), None)
                .await
        })
        .await
        .map(CloudResult::Write)
    }

    pub async fn cloud_request_write(
        &self,
        input: CloudExecutionInput,
        reason: &str,
        request_id: Uuid,
    ) -> CtResult<CloudResult> {
        let body = api::RequestAgentCliWriteRequest {
            conversation_id: input.session,
            connection_list: input.connections,
            script: input
                .script
                .parse()
                .map_err(|_| CtError::Usage("Script is empty.".into()))?,
            reasoning: reason
                .parse()
                .map_err(|_| CtError::Usage("Reason must contain 1–1000 characters.".into()))?,
            timeout: timeout(input.timeout)?,
            run_in_background: input.background,
            tool_call_id: request_id
                .to_string()
                .parse()
                .map_err(|_| CtError::Usage("Invalid request ID.".into()))?,
            recent_user_messages: Vec::new(),
        };
        self.authed_with_timeout(Duration::from_secs(180), async |client| {
            client.agent_cli_request_agent_cli_write(None, &body).await
        })
        .await
        .map(CloudResult::Write)
    }
}

fn timeout(seconds: u16) -> CtResult<NonZeroU64> {
    if seconds > 120 {
        return Err(CtError::Usage(
            "Timeout must be between 1 and 120 seconds.".into(),
        ));
    }
    NonZeroU64::new(u64::from(seconds))
        .ok_or_else(|| CtError::Usage("Timeout must be positive.".into()))
}
