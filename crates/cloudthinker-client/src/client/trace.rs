use std::num::NonZeroU64;

use chrono::{DateTime, Utc};
use cloudthinker_api::types as api;
use serde::Serialize;
use uuid::Uuid;

use super::CtClient;
use crate::error::{CtError, CtResult};

pub const TRACE_MAX_MESSAGES: u64 = 100;

#[derive(Debug, Clone, Serialize)]
pub struct ConversationTrace {
    pub conversation_id: Uuid,
    pub workspace_id: Uuid,
    pub captured_at: DateTime<Utc>,
    pub snapshot: String,
    pub messages: Vec<TraceMessage>,
    pub summary: TraceSummary,
    pub truncation: TraceTruncation,
}

#[derive(Debug, Clone, Serialize)]
pub struct TraceMessage {
    pub id: Uuid,
    pub role: String,
    pub created_at: DateTime<Utc>,
    pub text: String,
    pub text_truncated: bool,
    pub finish_reason: Option<String>,
    pub usage: TraceUsage,
    pub tools: Vec<TraceTool>,
    pub tools_omitted: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct TraceTool {
    pub id: Uuid,
    pub tool_call_id: Option<String>,
    pub position: i64,
    pub name: String,
    pub runtime_seconds: Option<f64>,
    pub completed: bool,
    pub approval_status: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TraceUsage {
    pub records: i64,
    pub token_records: i64,
    pub tokens: Option<i64>,
    pub credits: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TraceSummary {
    pub scope: String,
    pub messages: i64,
    pub tool_calls: i64,
    pub failed_messages: i64,
    pub tokens: Option<i64>,
    pub credits: Option<f64>,
    pub messages_without_usage: i64,
    pub messages_with_unknown_tokens: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct TraceTruncation {
    pub truncated: bool,
    pub has_older_messages: bool,
    pub messages_filtered: i64,
    pub messages_omitted_for_bytes: i64,
    pub tools_omitted: i64,
    pub text_fields_truncated: i64,
    pub message_limit: i64,
    pub tool_limit: i64,
    pub text_char_limit: i64,
    pub response_byte_limit: i64,
}

impl From<api::ConversationTrace> for ConversationTrace {
    fn from(value: api::ConversationTrace) -> Self {
        Self {
            conversation_id: value.conversation_id,
            workspace_id: value.workspace_id,
            captured_at: value.captured_at,
            snapshot: value.snapshot.to_string(),
            messages: value.messages.into_iter().map(TraceMessage::from).collect(),
            summary: TraceSummary::from(value.summary),
            truncation: TraceTruncation::from(value.truncation),
        }
    }
}

impl From<api::ConversationTraceMessage> for TraceMessage {
    fn from(value: api::ConversationTraceMessage) -> Self {
        Self {
            id: value.id,
            role: value.role,
            created_at: value.created_at,
            text: value.text,
            text_truncated: value.text_truncated,
            finish_reason: value.finish_reason,
            usage: TraceUsage {
                records: value.usage.records,
                token_records: value.usage.token_records,
                tokens: value.usage.tokens,
                credits: value.usage.credits,
            },
            tools: value.tools.into_iter().map(TraceTool::from).collect(),
            tools_omitted: value.tools_omitted,
        }
    }
}

impl From<api::ConversationTraceTool> for TraceTool {
    fn from(value: api::ConversationTraceTool) -> Self {
        Self {
            id: value.id,
            tool_call_id: value.tool_call_id,
            position: value.position,
            name: value.name,
            runtime_seconds: value.runtime_seconds,
            completed: value.completed,
            approval_status: value.approval_status,
        }
    }
}

impl From<api::ConversationTraceSummary> for TraceSummary {
    fn from(value: api::ConversationTraceSummary) -> Self {
        Self {
            scope: value.scope.to_string(),
            messages: value.messages,
            tool_calls: value.tool_calls,
            failed_messages: value.failed_messages,
            tokens: value.tokens,
            credits: value.credits,
            messages_without_usage: value.messages_without_usage,
            messages_with_unknown_tokens: value.messages_with_unknown_tokens,
        }
    }
}

impl From<api::ConversationTraceTruncation> for TraceTruncation {
    fn from(value: api::ConversationTraceTruncation) -> Self {
        Self {
            truncated: value.truncated,
            has_older_messages: value.has_older_messages,
            messages_filtered: value.messages_filtered,
            messages_omitted_for_bytes: value.messages_omitted_for_bytes,
            tools_omitted: value.tools_omitted,
            text_fields_truncated: value.text_fields_truncated,
            message_limit: value.message_limit,
            tool_limit: value.tool_limit,
            text_char_limit: value.text_char_limit,
            response_byte_limit: value.response_byte_limit,
        }
    }
}

impl CtClient {
    pub async fn conversation_trace(
        &self,
        conversation_id: Uuid,
        limit: u64,
    ) -> CtResult<ConversationTrace> {
        let limit = NonZeroU64::new(limit)
            .filter(|limit| limit.get() <= TRACE_MAX_MESSAGES)
            .ok_or_else(|| {
                CtError::Usage(format!(
                    "--limit must be between 1 and {TRACE_MAX_MESSAGES}"
                ))
            })?;
        let trace = self
            .authed(async |client: cloudthinker_api::Client| {
                client
                    .cli_get_conversation_trace(&conversation_id, Some(limit), None)
                    .await
            })
            .await?;
        Ok(ConversationTrace::from(trace))
    }
}
