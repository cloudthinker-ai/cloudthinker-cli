use std::collections::BTreeSet;

use cloudthinker_api::types as api;
use serde::Serialize;
use uuid::Uuid;

use super::{CtClient, exceeds_chars};
use crate::error::{CtError, CtResult};

const MAX_AGENT_ROWS: usize = 1000;
const MAX_AGENT_IDENTITY_CHARS: usize = 255;
const MAX_AGENT_DESCRIPTION_CHARS: usize = 2048;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentKind {
    Default,
    Custom,
}

impl AgentKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Custom => "custom",
        }
    }
}

impl From<api::AgentType> for AgentKind {
    fn from(kind: api::AgentType) -> Self {
        match kind {
            api::AgentType::Default => Self::Default,
            api::AgentType::Custom => Self::Custom,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentChatSelection {
    Eligible,
    Inactive,
    Default,
}

impl AgentChatSelection {
    pub fn label(self) -> &'static str {
        match self {
            Self::Eligible => "eligible",
            Self::Inactive => "inactive",
            Self::Default => "default",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct AgentView {
    pub id: Uuid,
    pub alias: String,
    pub name: String,
    pub agent_type: AgentKind,
    pub is_active: bool,
    pub description: String,
    pub description_truncated: bool,
    pub chat_selection: AgentChatSelection,
}

impl AgentView {
    fn from_metadata(
        id: Uuid,
        alias: String,
        name: String,
        agent_type: api::AgentType,
        is_active: bool,
        description: String,
    ) -> CtResult<Self> {
        if exceeds_chars(&alias, MAX_AGENT_IDENTITY_CHARS)
            || exceeds_chars(&name, MAX_AGENT_IDENTITY_CHARS)
        {
            return Err(CtError::Protocol(format!(
                "agent {id} has oversized identity metadata; open Agents in the browser"
            )));
        }
        let agent_type = AgentKind::from(agent_type);
        let chat_selection = match (agent_type, is_active) {
            (AgentKind::Default, _) => AgentChatSelection::Default,
            (AgentKind::Custom, true) => AgentChatSelection::Eligible,
            (AgentKind::Custom, false) => AgentChatSelection::Inactive,
        };
        let description_truncated = exceeds_chars(&description, MAX_AGENT_DESCRIPTION_CHARS);
        let description = if description_truncated {
            description
                .chars()
                .take(MAX_AGENT_DESCRIPTION_CHARS)
                .collect()
        } else {
            description
        };
        Ok(Self {
            id,
            alias,
            name,
            agent_type,
            is_active,
            description,
            description_truncated,
            chat_selection,
        })
    }
}

#[derive(Debug, Clone)]
pub struct Agents {
    pub data: Vec<AgentView>,
}

#[derive(Debug, Clone)]
pub enum AgentMatch {
    One(Box<AgentView>),
    None,
    Many(Vec<AgentView>),
}

impl Agents {
    pub fn resolve(&self, reference: &str) -> AgentMatch {
        let reference = reference.trim();
        let id = Uuid::parse_str(reference).ok();
        let mut matched: Vec<_> = self
            .data
            .iter()
            .filter(|row| {
                id.map_or_else(
                    || row.alias.eq_ignore_ascii_case(reference),
                    |id| row.id == id,
                )
            })
            .cloned()
            .collect();
        match matched.len() {
            0 => AgentMatch::None,
            1 => AgentMatch::One(Box::new(matched.remove(0))),
            _ => AgentMatch::Many(matched),
        }
    }
}

impl CtClient {
    pub async fn list_agents(&self) -> CtResult<Agents> {
        self.whoami().await?;
        let roster = self
            .authed(async |c: cloudthinker_api::Client| c.agents_read_agents(None).await)
            .await?;
        if roster.data.len() > MAX_AGENT_ROWS {
            return Err(CtError::Protocol(format!(
                "the workspace has more than {MAX_AGENT_ROWS} agents; open Agents in the browser"
            )));
        }
        if usize::try_from(roster.count).ok() != Some(roster.data.len()) {
            return Err(CtError::Protocol(
                "the agent roster is incomplete; retry or open Agents in the browser".into(),
            ));
        }
        let mut ids = BTreeSet::new();
        let mut data = Vec::with_capacity(roster.data.len());
        for row in roster.data {
            if !ids.insert(row.id) {
                return Err(CtError::Protocol(
                    "the agent roster contains duplicate IDs; retry or open Agents in the browser"
                        .into(),
                ));
            }
            data.push(AgentView::from_metadata(
                row.id,
                row.alias,
                row.title,
                row.agent_type.unwrap_or(api::AgentType::Default),
                row.is_active,
                row.goal,
            )?);
        }
        data.sort_by_cached_key(|row| (row.alias.to_ascii_lowercase(), row.alias.clone(), row.id));
        Ok(Agents { data })
    }

    pub async fn agent(&self, reference: &str) -> CtResult<AgentView> {
        let reference = reference.trim();
        if reference.is_empty() || exceeds_chars(reference, MAX_AGENT_IDENTITY_CHARS) {
            return Err(CtError::Usage(format!(
                "use an agent UUID or alias of at most {MAX_AGENT_IDENTITY_CHARS} characters from `cloudthinker agents ls`"
            )));
        }
        let roster = self.list_agents().await?;
        let id = match roster.resolve(reference) {
            AgentMatch::One(agent) if agent.agent_type == AgentKind::Default => return Ok(*agent),
            AgentMatch::One(agent) => agent.id,
            AgentMatch::Many(agents) => {
                let ids = agents
                    .iter()
                    .map(|agent| agent.id.to_string())
                    .collect::<Vec<_>>()
                    .join(", ");
                return Err(CtError::Usage(format!(
                    "several agents share this alias; use an agent UUID: {ids}"
                )));
            }
            AgentMatch::None => Uuid::parse_str(reference).map_err(|_| CtError::Api {
                status: 404,
                detail: Some(
                    "no agent matches this reference; use `cloudthinker agents ls`".into(),
                ),
            })?,
        };
        let row = self
            .authed(async |c: cloudthinker_api::Client| {
                c.custom_agents_get_custom_agent(&id, None).await
            })
            .await?;
        if row.id != id || row.agent_type != api::AgentType::Custom {
            return Err(CtError::Protocol(
                "the custom agent response does not match the requested agent".into(),
            ));
        }
        AgentView::from_metadata(
            row.id,
            row.alias,
            row.title,
            row.agent_type,
            row.is_active,
            row.goal,
        )
    }
}
