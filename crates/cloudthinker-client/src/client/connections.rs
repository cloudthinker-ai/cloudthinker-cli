use chrono::{DateTime, Utc};
use cloudthinker_api::types as api;
use serde::Serialize;
use uuid::Uuid;

use super::CtClient;
use crate::error::{CtError, CtResult};

const CONNECT_PREFIX_MAX_LEN: usize = 64;
const CONNECTORS_PAGE: &str = "/connectors";
const CONNECTORS_TAB: &str = "builtin-connections";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionKind {
    Builtin,
    Mcp,
}

impl ConnectionKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Builtin => "builtin",
            Self::Mcp => "mcp",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionStatus {
    Connected,
    Testing,
    Disconnecting,
    Error,
    Unknown,
    #[serde(untagged)]
    Other(String),
}

impl ConnectionStatus {
    pub fn label(&self) -> &str {
        match self {
            Self::Connected => "connected",
            Self::Testing => "testing",
            Self::Disconnecting => "disconnecting",
            Self::Error => "error",
            Self::Unknown => "unknown",
            Self::Other(raw) => raw,
        }
    }
}

impl From<api::ConnectionStatus> for ConnectionStatus {
    fn from(status: api::ConnectionStatus) -> Self {
        match status.0.as_str() {
            "connected" => Self::Connected,
            "testing" => Self::Testing,
            "disconnecting" => Self::Disconnecting,
            "error" => Self::Error,
            _ => Self::Other(status.0),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionTestPhase {
    Queued,
    StartingSandbox,
    RunningScript,
    Succeeded,
    Failed,
    Timeout,
    Cancelled,
    #[serde(untagged)]
    Other(String),
}

impl ConnectionTestPhase {
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Succeeded | Self::Failed | Self::Timeout | Self::Cancelled
        )
    }

    pub fn is_failure(&self) -> bool {
        self.is_terminal() && *self != Self::Succeeded
    }

    pub fn label(&self) -> &str {
        match self {
            Self::Queued => "queued",
            Self::StartingSandbox => "starting sandbox",
            Self::RunningScript => "running test",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Timeout => "timed out",
            Self::Cancelled => "cancelled",
            Self::Other(raw) => raw,
        }
    }
}

impl From<api::ConnectionTestPhase> for ConnectionTestPhase {
    fn from(phase: api::ConnectionTestPhase) -> Self {
        match phase.0.as_str() {
            "queued" => Self::Queued,
            "starting_sandbox" => Self::StartingSandbox,
            "running_script" => Self::RunningScript,
            "succeeded" => Self::Succeeded,
            "failed" => Self::Failed,
            "timeout" => Self::Timeout,
            "cancelled" => Self::Cancelled,
            _ => Self::Other(phase.0),
        }
    }
}

fn auth_label(auth: api::AuthType) -> String {
    match auth.0.as_str() {
        "STATIC" => "static".to_string(),
        "OAUTH" => "oauth".to_string(),
        _ => auth.0,
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ConnectionView {
    pub id: Uuid,
    pub kind: ConnectionKind,
    pub name: String,
    pub prefix: String,
    pub alias: Option<String>,
    pub status: ConnectionStatus,
    pub status_message: Option<String>,
    pub auth: Option<String>,
    pub can_retry: bool,
    pub transport: Option<String>,
    pub description: Option<String>,
    pub documentation_url: Option<String>,
    pub updated_at: Option<DateTime<Utc>>,
    #[serde(skip)]
    prefix_aliases: Vec<String>,
}

macro_rules! builtin_view {
    ($row:expr) => {{
        let row = $row;
        ConnectionView {
            id: row.id,
            kind: ConnectionKind::Builtin,
            name: row.name,
            prefix: row.prefix,
            alias: row.alias.filter(|alias| !alias.is_empty()),
            status: row
                .status
                .map_or(ConnectionStatus::Unknown, ConnectionStatus::from),
            status_message: non_empty(row.status_message),
            auth: row.auth_type.map(auth_label),
            can_retry: row.can_retry,
            transport: None,
            description: row.description.and_then(non_empty),
            documentation_url: row.documentation_url,
            updated_at: Some(row.updated_at),
            prefix_aliases: row.aliases,
        }
    }};
}

impl ConnectionView {
    fn from_mcp(row: api::McpConnectionPublic) -> Self {
        Self {
            id: row.id,
            kind: ConnectionKind::Mcp,
            name: row.name,
            prefix: row.prefix,
            alias: None,
            status: row.status.into(),
            status_message: non_empty(row.status_message),
            auth: None,
            can_retry: false,
            transport: Some(row.mcp_transport_type.0),
            description: row.description.and_then(non_empty),
            documentation_url: None,
            updated_at: None,
            prefix_aliases: Vec::new(),
        }
    }

    fn matches_alias(&self, target: &str) -> bool {
        self.alias
            .as_deref()
            .is_some_and(|alias| alias.eq_ignore_ascii_case(target))
    }

    fn matches_prefix(&self, target: &str) -> bool {
        self.prefix.eq_ignore_ascii_case(target)
            || self
                .prefix_aliases
                .iter()
                .any(|alias| alias.eq_ignore_ascii_case(target))
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ConnectionLocation {
    pub key: String,
    pub value: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ConnectionDetail {
    #[serde(flatten)]
    pub connection: ConnectionView,
    pub locations: Vec<ConnectionLocation>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ConnectionTest {
    pub id: Uuid,
    pub kind: ConnectionKind,
    pub phase: ConnectionTestPhase,
    pub terminal: bool,
    pub started_at: Option<DateTime<Utc>>,
    pub elapsed_ms: Option<i64>,
    pub last_error: Option<String>,
}

impl ConnectionTest {
    fn from_api(id: Uuid, kind: ConnectionKind, status: api::ConnectionTestStatus) -> Self {
        let phase = ConnectionTestPhase::from(status.phase);
        Self {
            id,
            kind,
            terminal: phase.is_terminal(),
            phase,
            started_at: status.started_at,
            elapsed_ms: status.elapsed_ms,
            last_error: status.last_error.and_then(non_empty),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Connections {
    pub installed: Vec<ConnectionView>,
    catalog_prefixes: Vec<String>,
}

#[derive(Debug, Clone)]
pub enum ConnectionMatch {
    One(Box<ConnectionView>),
    None,
    Many(Vec<ConnectionView>),
}

impl Connections {
    pub fn resolve(&self, target: &str) -> ConnectionMatch {
        let target = target.trim();
        let id = Uuid::parse_str(target).ok();
        let mut matched = [
            self.matching(|row| Some(row.id) == id),
            self.matching(|row| row.matches_alias(target)),
            self.matching(|row| row.matches_prefix(target)),
        ]
        .into_iter()
        .find(|rows| !rows.is_empty())
        .unwrap_or_default();
        match matched.len() {
            0 => ConnectionMatch::None,
            1 => ConnectionMatch::One(Box::new(matched.remove(0))),
            _ => ConnectionMatch::Many(matched),
        }
    }

    fn matching(&self, keep: impl Fn(&ConnectionView) -> bool) -> Vec<ConnectionView> {
        self.installed
            .iter()
            .filter(|row| keep(row))
            .cloned()
            .collect()
    }

    pub fn offers_prefix(&self, prefix: &str) -> bool {
        self.catalog_prefixes
            .iter()
            .chain(
                self.installed
                    .iter()
                    .filter(|row| row.kind == ConnectionKind::Builtin)
                    .map(|row| &row.prefix),
            )
            .any(|known| known == prefix)
    }

    pub fn builtin_ids_with_prefix(&self, prefix: &str) -> Vec<Uuid> {
        self.installed
            .iter()
            .filter(|row| row.kind == ConnectionKind::Builtin && row.prefix == prefix)
            .map(|row| row.id)
            .collect()
    }
}

pub fn validate_connect_prefix(prefix: &str) -> CtResult<()> {
    let valid = !prefix.is_empty()
        && prefix.len() <= CONNECT_PREFIX_MAX_LEN
        && prefix
            .bytes()
            .all(|byte| matches!(byte, b'a'..=b'z' | b'0'..=b'9' | b'_' | b'-'));
    if valid {
        Ok(())
    } else {
        Err(CtError::Usage(format!(
            "`{prefix}` is not a connection prefix; use lowercase letters, digits, `_` or `-` (at most {CONNECT_PREFIX_MAX_LEN})"
        )))
    }
}

fn non_empty(value: String) -> Option<String> {
    if value.trim().is_empty() {
        None
    } else {
        Some(value)
    }
}

impl CtClient {
    pub async fn list_connections(&self) -> CtResult<Connections> {
        let (builtin, mcp) = tokio::try_join!(
            self.authed(async |c: cloudthinker_api::Client| {
                c.builtin_connection_get_builtin_connections(None, None)
                    .await
            }),
            self.authed(async |c: cloudthinker_api::Client| {
                c.mcp_connection_get_mcp_connections(None).await
            }),
        )?;
        let mut installed = Vec::new();
        let mut catalog_prefixes = Vec::new();
        for row in builtin.data {
            if row.is_connected {
                installed.push(builtin_view!(row));
            } else {
                catalog_prefixes.push(row.prefix);
            }
        }
        installed.extend(mcp.data.into_iter().map(ConnectionView::from_mcp));
        Ok(Connections {
            installed,
            catalog_prefixes,
        })
    }

    pub async fn builtin_connection(&self, id: Uuid) -> CtResult<ConnectionDetail> {
        let detail = self
            .authed(async |c: cloudthinker_api::Client| {
                c.builtin_connection_get_builtin_connection(&id, None).await
            })
            .await?;
        let locations = detail
            .locations
            .iter()
            .map(|location| ConnectionLocation {
                key: location.key.clone(),
                value: location.value.clone(),
            })
            .collect();
        Ok(ConnectionDetail {
            connection: builtin_view!(detail),
            locations,
        })
    }

    pub async fn connection_test(
        &self,
        id: Uuid,
        kind: ConnectionKind,
    ) -> CtResult<ConnectionTest> {
        let status = match kind {
            ConnectionKind::Builtin => {
                self.authed(async |c: cloudthinker_api::Client| {
                    c.builtin_connection_get_connection_test_status(&id, None)
                        .await
                })
                .await?
            }
            ConnectionKind::Mcp => {
                self.authed(async |c: cloudthinker_api::Client| {
                    c.mcp_connection_get_mcp_connection_test_status(&id, None)
                        .await
                })
                .await?
            }
        };
        let mut test = ConnectionTest::from_api(id, kind, status);
        if test.phase.is_failure() && test.last_error.is_none() {
            test.last_error = match kind {
                ConnectionKind::Builtin => {
                    self.builtin_connection(id).await?.connection.status_message
                }
                ConnectionKind::Mcp => self.mcp_status_message(id).await?,
            };
        }
        Ok(test)
    }

    async fn mcp_status_message(&self, id: Uuid) -> CtResult<Option<String>> {
        let rows = self
            .authed(async |c: cloudthinker_api::Client| {
                c.mcp_connection_get_mcp_connections(None).await
            })
            .await?;
        Ok(rows
            .data
            .into_iter()
            .find(|row| row.id == id)
            .and_then(|row| non_empty(row.status_message)))
    }

    pub async fn retry_connection(&self, id: Uuid) -> CtResult<ConnectionTest> {
        let accepted = self
            .authed(async |c: cloudthinker_api::Client| {
                c.builtin_connection_retry_builtin_connection(&id, None)
                    .await
            })
            .await?;
        Ok(ConnectionTest::from_api(
            id,
            ConnectionKind::Builtin,
            accepted.test_status,
        ))
    }

    pub fn connect_url(&self, prefix: &str, workspace_id: Uuid) -> CtResult<String> {
        validate_connect_prefix(prefix)?;
        let mut url = url::Url::parse(self.base_url())
            .and_then(|base| base.join(CONNECTORS_PAGE))
            .map_err(|error| CtError::Usage(format!("invalid CloudThinker URL: {error}")))?;
        url.query_pairs_mut()
            .append_pair("tab", CONNECTORS_TAB)
            .append_pair("install", prefix)
            .append_pair("wsId", &workspace_id.to_string());
        Ok(url.into())
    }
}
