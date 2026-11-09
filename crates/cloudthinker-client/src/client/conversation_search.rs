use std::num::NonZeroU64;

use chrono::{DateTime, Utc};
use cloudthinker_api::types as api;
use serde::Serialize;
use uuid::Uuid;

use super::CtClient;
use crate::error::{CtError, CtResult};

pub const CONVERSATION_SEARCH_DEFAULT_LIMIT: u64 = 10;
pub const CONVERSATION_SEARCH_MAX_LIMIT: u64 = 50;
pub const CONVERSATION_SEARCH_MAX_QUERY_CHARS: usize = 200;
pub const CONVERSATION_SEARCH_FUZZY_MIN_SCORE: u64 = 60;
pub const CONVERSATION_SEARCH_FUZZY_MAX_SCORE: u64 = 100;
pub const CONVERSATION_SEARCH_FUZZY_DEFAULT_SCORE: u64 = 85;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConversationSearchMatchMode {
    Literal,
    Regex,
    Fuzzy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConversationSearchTimeField {
    Created,
    Updated,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConversationSearchStatus {
    Complete,
    LimitReached,
    Partial,
}

#[derive(Debug, Clone)]
pub struct ConversationSearchQuery<'a> {
    pub query: &'a str,
    pub match_mode: ConversationSearchMatchMode,
    pub min_score: Option<u64>,
    pub limit: u64,
    pub since: Option<&'a str>,
    pub until: Option<&'a str>,
    pub time_field: ConversationSearchTimeField,
    pub conversation_types: &'a [String],
    pub cursor: Option<&'a str>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ConversationSearch {
    pub workspace_id: Uuid,
    pub query: String,
    pub match_mode: ConversationSearchMatchMode,
    pub min_score: Option<i64>,
    pub limit: i64,
    pub hits: Vec<ConversationSearchHit>,
    pub coverage: ConversationSearchCoverage,
    pub scope: ConversationSearchScope,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ConversationSearchHit {
    pub conversation_id: Uuid,
    pub title: Option<String>,
    pub reference: String,
    pub web_url: String,
    pub conversation_type: Option<String>,
    pub created_at: Option<DateTime<Utc>>,
    pub updated_at: Option<DateTime<Utc>>,
    pub evidence: ConversationSearchEvidence,
}

#[derive(Debug, Clone, Serialize)]
pub struct ConversationSearchEvidence {
    pub record_type: String,
    pub field: String,
    pub line: u64,
    pub match_start: u64,
    pub match_end: u64,
    pub snippet: String,
    pub snippet_truncated: bool,
    pub score: Option<f64>,
    pub record_id: Option<String>,
    pub role: Option<String>,
    pub tool_name: Option<String>,
    pub at: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ConversationSearchCoverage {
    pub status: ConversationSearchStatus,
    pub reasons: Vec<String>,
    pub scanned_sources: i64,
    pub next_cursor: Option<String>,
    pub cursor_expires_at: Option<DateTime<Utc>>,
    pub hint: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ConversationSearchScope {
    pub time_field: ConversationSearchTimeField,
    pub since: Option<DateTime<Utc>>,
    pub until: Option<DateTime<Utc>>,
    pub conversation_types: Vec<String>,
    pub timezone: String,
    pub as_of: Option<DateTime<Utc>>,
}

fn chat_url(base_url: &str, conversation_id: Uuid, workspace_id: Uuid) -> String {
    format!(
        "{}/chat?conversationId={conversation_id}&wsId={workspace_id}",
        base_url.trim_end_matches('/')
    )
}

fn parse_date<T: std::str::FromStr>(value: Option<&str>, flag: &str) -> CtResult<Option<T>> {
    value.map(str::parse).transpose().map_err(|_| {
        CtError::Usage(format!(
            "{flag} must be YYYY-MM-DD or a datetime with Z or an offset"
        ))
    })
}

fn search_request(
    query: &ConversationSearchQuery<'_>,
) -> CtResult<api::CliConversationSearchRequest> {
    if query.query.trim().is_empty() {
        return Err(CtError::Usage(
            "the search query must contain a non-space character".into(),
        ));
    }
    if query.query.chars().any(char::is_control) {
        return Err(CtError::Usage(
            "the search query must be one line without control characters".into(),
        ));
    }
    if query.query.chars().count() > CONVERSATION_SEARCH_MAX_QUERY_CHARS {
        return Err(CtError::Usage(format!(
            "the search query must be at most {CONVERSATION_SEARCH_MAX_QUERY_CHARS} characters"
        )));
    }
    let limit = NonZeroU64::new(query.limit)
        .filter(|limit| limit.get() <= CONVERSATION_SEARCH_MAX_LIMIT)
        .ok_or_else(|| {
            CtError::Usage(format!(
                "--limit must be between 1 and {CONVERSATION_SEARCH_MAX_LIMIT}"
            ))
        })?;
    let conversation_types = query
        .conversation_types
        .iter()
        .map(|kind| kind.trim().to_ascii_lowercase())
        .collect::<Vec<_>>();
    if conversation_types.iter().any(String::is_empty) {
        return Err(CtError::Usage("--type must not be empty".into()));
    }
    let min_score = match (query.match_mode, query.min_score) {
        (ConversationSearchMatchMode::Fuzzy, Some(score))
            if !(CONVERSATION_SEARCH_FUZZY_MIN_SCORE..=CONVERSATION_SEARCH_FUZZY_MAX_SCORE)
                .contains(&score) =>
        {
            return Err(CtError::Usage(format!(
                "--min-score must be between {CONVERSATION_SEARCH_FUZZY_MIN_SCORE} and {CONVERSATION_SEARCH_FUZZY_MAX_SCORE}"
            )));
        }
        (ConversationSearchMatchMode::Fuzzy, score) => score.map(|score| score as i64),
        (_, Some(_)) => {
            return Err(CtError::Usage("--min-score needs --fuzzy".into()));
        }
        (_, None) => None,
    };
    Ok(api::CliConversationSearchRequest {
        conversation_types,
        cursor: query.cursor.map(str::parse).transpose().map_err(|_| {
            CtError::Usage(
                "--cursor must be the next_cursor value printed by a previous search".into(),
            )
        })?,
        limit,
        match_mode: Some(match query.match_mode {
            ConversationSearchMatchMode::Literal => api::CliSearchMatchMode::Literal,
            ConversationSearchMatchMode::Regex => api::CliSearchMatchMode::Regex,
            ConversationSearchMatchMode::Fuzzy => api::CliSearchMatchMode::Fuzzy,
        }),
        min_score,
        query: query
            .query
            .parse()
            .map_err(|_| CtError::Usage("the search query is not valid".into()))?,
        since: parse_date(query.since, "--since")?,
        time_field: match query.time_field {
            ConversationSearchTimeField::Created => {
                api::CliConversationSearchRequestTimeField::Created
            }
            ConversationSearchTimeField::Updated => {
                api::CliConversationSearchRequestTimeField::Updated
            }
        },
        until: parse_date(query.until, "--until")?,
    })
}

fn search_from_wire(base_url: &str, value: api::CliConversationSearchResult) -> ConversationSearch {
    let workspace_id = value.workspace_id;
    ConversationSearch {
        workspace_id,
        query: value.query,
        match_mode: match value.match_mode {
            api::CliSearchMatchMode::Literal => ConversationSearchMatchMode::Literal,
            api::CliSearchMatchMode::Regex => ConversationSearchMatchMode::Regex,
            api::CliSearchMatchMode::Fuzzy => ConversationSearchMatchMode::Fuzzy,
        },
        min_score: value.min_score,
        limit: value.limit,
        hits: value
            .hits
            .into_iter()
            .map(|hit| ConversationSearchHit {
                web_url: chat_url(base_url, hit.conversation_id, workspace_id),
                conversation_id: hit.conversation_id,
                title: hit.title,
                reference: hit.reference,
                conversation_type: hit.conversation_type,
                created_at: hit.created_at,
                updated_at: hit.updated_at,
                evidence: ConversationSearchEvidence {
                    record_type: hit.evidence.record_type.to_string(),
                    field: hit.evidence.field.to_string(),
                    line: hit.evidence.line.get(),
                    match_start: hit.evidence.match_start,
                    match_end: hit.evidence.match_end,
                    snippet: hit.evidence.snippet,
                    snippet_truncated: hit.evidence.snippet_truncated,
                    score: hit.evidence.score,
                    record_id: hit.evidence.record_id,
                    role: hit.evidence.role,
                    tool_name: hit.evidence.tool_name,
                    at: hit.evidence.at,
                },
            })
            .collect(),
        coverage: ConversationSearchCoverage {
            status: match value.coverage.status {
                api::CliConversationSearchCoverageStatus::Complete => {
                    ConversationSearchStatus::Complete
                }
                api::CliConversationSearchCoverageStatus::LimitReached => {
                    ConversationSearchStatus::LimitReached
                }
                api::CliConversationSearchCoverageStatus::Partial => {
                    ConversationSearchStatus::Partial
                }
            },
            reasons: value.coverage.reasons,
            scanned_sources: value.coverage.scanned_sources,
            next_cursor: value.coverage.next_cursor,
            cursor_expires_at: value.coverage.cursor_expires_at,
            hint: value.coverage.hint,
        },
        scope: ConversationSearchScope {
            time_field: match value.scope.time_field {
                api::CliConversationSearchScopeTimeField::Created => {
                    ConversationSearchTimeField::Created
                }
                api::CliConversationSearchScopeTimeField::Updated => {
                    ConversationSearchTimeField::Updated
                }
            },
            since: value.scope.since,
            until: value.scope.until,
            conversation_types: value.scope.conversation_types,
            timezone: value.scope.timezone,
            as_of: value.scope.as_of,
        },
        warnings: value.warnings,
    }
}

impl CtClient {
    pub async fn search_conversations(
        &self,
        query: &ConversationSearchQuery<'_>,
    ) -> CtResult<ConversationSearch> {
        let body = search_request(query)?;
        let result = self
            .authed(async |client: cloudthinker_api::Client| {
                client.cli_search_conversations(None, &body).await
            })
            .await?;
        Ok(search_from_wire(self.base_url(), result))
    }
}
