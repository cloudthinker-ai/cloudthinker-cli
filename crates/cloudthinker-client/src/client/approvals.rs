use chrono::{DateTime, Utc};
use cloudthinker_spec::Method;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::CtClient;
use super::public::QueryValue;
use crate::error::{CtError, CtResult};

pub const APPROVALS_DEFAULT_LIMIT: u64 = 20;
pub const APPROVALS_MAX_LIMIT: u64 = 100;
const APPROVALS_MAX_CURSOR_CHARS: usize = 512;
const APPROVALS_PATH: &str = "/approvals/requests";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalKind {
    AgentCliWrite,
    ChatToolCall,
    Subagent,
    RunbookExecution,
}

impl ApprovalKind {
    fn wire(self) -> &'static str {
        match self {
            Self::AgentCliWrite => "agent_cli_write",
            Self::ChatToolCall => "chat_tool_call",
            Self::Subagent => "subagent",
            Self::RunbookExecution => "runbook_execution",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::AgentCliWrite => "agent CLI write",
            Self::ChatToolCall => "chat tool call",
            Self::Subagent => "subagent",
            Self::RunbookExecution => "runbook",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalSourceStatus {
    Pending,
    Approved,
    Denied,
    Expired,
    Stale,
    Executed,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalState {
    Pending,
    Approved,
    Declined,
    Expired,
    Superseded,
}

impl ApprovalState {
    pub fn is_closed(self) -> bool {
        self != Self::Pending
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Approved => "approved",
            Self::Declined => "declined",
            Self::Expired => "expired",
            Self::Superseded => "superseded",
        }
    }
}

impl From<ApprovalSourceStatus> for ApprovalState {
    fn from(status: ApprovalSourceStatus) -> Self {
        match status {
            ApprovalSourceStatus::Pending => Self::Pending,
            ApprovalSourceStatus::Approved
            | ApprovalSourceStatus::Executed
            | ApprovalSourceStatus::Failed => Self::Approved,
            ApprovalSourceStatus::Denied => Self::Declined,
            ApprovalSourceStatus::Expired => Self::Expired,
            ApprovalSourceStatus::Stale => Self::Superseded,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Unreported;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ApprovalObject {
    #[serde(rename = "ref")]
    pub reference: Option<Uuid>,
    pub label: String,
    pub incident: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ApprovalPerson {
    pub user_id: Option<Uuid>,
    pub name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ApprovalView {
    pub id: Unreported,
    pub request_id: Uuid,
    pub kind: ApprovalKind,
    pub state: ApprovalState,
    pub source_status: ApprovalSourceStatus,
    pub title: String,
    pub object: ApprovalObject,
    pub requester: ApprovalPerson,
    pub approver: Unreported,
    pub routing: Unreported,
    pub reason: Option<String>,
    pub two_person: bool,
    pub asked_at: DateTime<Utc>,
    pub escalate_at: Unreported,
    pub expires_at: DateTime<Utc>,
    pub plan_revision: Unreported,
    pub terraform_plan_sha256: Unreported,
    pub scope: Unreported,
    pub decided_by: Option<ApprovalPerson>,
    pub decided_surface: Unreported,
    pub decided_at: Option<DateTime<Utc>>,
    pub decision: Unreported,
    pub deliveries: Unreported,
    pub advisor: Unreported,
    pub url: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ApprovalPage {
    pub items: Vec<ApprovalView>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct ApprovalListQuery {
    pub kinds: Vec<ApprovalKind>,
    pub limit: u64,
    pub cursor: Option<String>,
}

#[derive(Deserialize)]
struct PublicApprovalAction {
    summary: String,
}

#[derive(Deserialize)]
struct PublicApprovalRequest {
    id: Uuid,
    kind: ApprovalKind,
    status: ApprovalSourceStatus,
    title: String,
    action: PublicApprovalAction,
    conversation_id: Option<Uuid>,
    incident_id: Option<Uuid>,
    requested_by_user_id: Uuid,
    requester: Option<String>,
    reason: Option<String>,
    two_person: bool,
    decider: Option<String>,
    decided_at: Option<DateTime<Utc>>,
    created_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
    url: String,
}

#[derive(Deserialize)]
struct PublicApprovalPage {
    items: Vec<PublicApprovalRequest>,
    next_cursor: Option<String>,
}

impl From<PublicApprovalRequest> for ApprovalView {
    fn from(request: PublicApprovalRequest) -> Self {
        let decided_by =
            (request.decider.is_some() || request.decided_at.is_some()).then_some(ApprovalPerson {
                user_id: None,
                name: request.decider,
            });
        Self {
            id: Unreported,
            request_id: request.id,
            kind: request.kind,
            state: request.status.into(),
            source_status: request.status,
            title: request.title,
            object: ApprovalObject {
                reference: request.incident_id.or(request.conversation_id),
                label: request.action.summary,
                incident: request.incident_id.is_some(),
            },
            requester: ApprovalPerson {
                user_id: Some(request.requested_by_user_id),
                name: request.requester,
            },
            approver: Unreported,
            routing: Unreported,
            reason: request.reason,
            two_person: request.two_person,
            asked_at: request.created_at,
            escalate_at: Unreported,
            expires_at: request.expires_at,
            plan_revision: Unreported,
            terraform_plan_sha256: Unreported,
            scope: Unreported,
            decided_by,
            decided_surface: Unreported,
            decided_at: request.decided_at,
            decision: Unreported,
            deliveries: Unreported,
            advisor: Unreported,
            url: request.url,
        }
    }
}

fn list_query(query: &ApprovalListQuery) -> CtResult<Vec<QueryValue>> {
    if !(1..=APPROVALS_MAX_LIMIT).contains(&query.limit) {
        return Err(CtError::Usage(format!(
            "--limit must be between 1 and {APPROVALS_MAX_LIMIT}"
        )));
    }
    let mut values = vec![QueryValue {
        name: "limit".into(),
        value: query.limit.to_string(),
    }];
    if let Some(cursor) = &query.cursor {
        if cursor.is_empty() || cursor.chars().count() > APPROVALS_MAX_CURSOR_CHARS {
            return Err(CtError::Usage(format!(
                "--cursor must be the next_cursor value from a previous page (1 to {APPROVALS_MAX_CURSOR_CHARS} characters)"
            )));
        }
        values.push(QueryValue {
            name: "cursor".into(),
            value: cursor.clone(),
        });
    }
    let mut kinds = query.kinds.clone();
    kinds.sort_by_key(|kind| kind.wire());
    kinds.dedup();
    values.extend(kinds.into_iter().map(|kind| QueryValue {
        name: "kind".into(),
        value: kind.wire().into(),
    }));
    Ok(values)
}

impl CtClient {
    pub async fn list_approvals(&self, query: &ApprovalListQuery) -> CtResult<ApprovalPage> {
        let values = list_query(query)?;
        let page: PublicApprovalPage = self
            .public_json(Method::Get, APPROVALS_PATH, &values, None, None)
            .await?;
        Ok(ApprovalPage {
            items: page.items.into_iter().map(ApprovalView::from).collect(),
            next_cursor: page.next_cursor,
        })
    }

    pub async fn get_approval(&self, request_id: Uuid) -> CtResult<ApprovalView> {
        let request: PublicApprovalRequest = self
            .public_json(
                Method::Get,
                &format!("{APPROVALS_PATH}/{request_id}"),
                &[],
                None,
                None,
            )
            .await?;
        Ok(ApprovalView::from(request))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wire(status: &str, decider: Option<&str>, incident: bool) -> serde_json::Value {
        serde_json::json!({
            "id": "11111111-1111-4111-8111-111111111111",
            "kind": "runbook_execution",
            "status": status,
            "title": "Restart payments-api",
            "action": {"tool": null, "summary": "kubectl rollout restart deploy/payments-api"},
            "conversation_id": "22222222-2222-4222-8222-222222222222",
            "incident_id": if incident { Some("33333333-3333-4333-8333-333333333333") } else { None },
            "requested_by_user_id": "44444444-4444-4444-8444-444444444444",
            "requester": "Steve Tran",
            "reason": "Rule: Ask first",
            "two_person": true,
            "decider": decider,
            "decided_at": decider.map(|_| "2026-11-07T02:26:05Z"),
            "created_at": "2026-11-07T02:25:02Z",
            "expires_at": "2026-11-07T03:25:02Z",
            "url": "https://app.example.com/approvals/1"
        })
    }

    fn view(value: serde_json::Value) -> serde_json::Value {
        let request: PublicApprovalRequest = serde_json::from_value(value).unwrap();
        serde_json::to_value(ApprovalView::from(request)).unwrap()
    }

    #[test]
    fn a_pending_v1_request_maps_to_the_view_with_explicit_nulls() {
        assert_eq!(
            view(wire("pending", None, true)),
            serde_json::json!({
                "id": null,
                "request_id": "11111111-1111-4111-8111-111111111111",
                "kind": "runbook_execution",
                "state": "pending",
                "source_status": "pending",
                "title": "Restart payments-api",
                "object": {
                    "ref": "33333333-3333-4333-8333-333333333333",
                    "label": "kubectl rollout restart deploy/payments-api",
                    "incident": true
                },
                "requester": {
                    "user_id": "44444444-4444-4444-8444-444444444444",
                    "name": "Steve Tran"
                },
                "approver": null,
                "routing": null,
                "reason": "Rule: Ask first",
                "two_person": true,
                "asked_at": "2026-11-07T02:25:02Z",
                "escalate_at": null,
                "expires_at": "2026-11-07T03:25:02Z",
                "plan_revision": null,
                "terraform_plan_sha256": null,
                "scope": null,
                "decided_by": null,
                "decided_surface": null,
                "decided_at": null,
                "decision": null,
                "deliveries": null,
                "advisor": null,
                "url": "https://app.example.com/approvals/1"
            })
        );
    }

    #[test]
    fn every_v1_status_maps_to_one_view_state() {
        let cases = [
            ("pending", "pending"),
            ("approved", "approved"),
            ("executed", "approved"),
            ("failed", "approved"),
            ("denied", "declined"),
            ("expired", "expired"),
            ("stale", "superseded"),
        ];
        for (status, state) in cases {
            let mapped = view(wire(status, Some("Linh Tran"), false));
            assert_eq!(mapped["state"], state, "state for {status}");
            assert_eq!(mapped["source_status"], status);
        }
    }

    #[test]
    fn a_decided_request_names_its_decider_and_a_chat_request_refs_its_conversation() {
        let mapped = view(wire("denied", Some("Linh Tran"), false));
        assert_eq!(
            mapped["decided_by"],
            serde_json::json!({"user_id": null, "name": "Linh Tran"})
        );
        assert_eq!(mapped["decided_at"], "2026-11-07T02:26:05Z");
        assert_eq!(
            mapped["object"]["ref"],
            "22222222-2222-4222-8222-222222222222"
        );
        assert_eq!(mapped["object"]["incident"], false);
    }

    #[test]
    fn an_unknown_status_is_a_shape_error_not_a_guess() {
        let request =
            serde_json::from_value::<PublicApprovalRequest>(wire("withdrawn", None, false));
        assert!(request.is_err());
    }

    #[test]
    fn list_query_validates_limit_and_cursor_and_sorts_kinds() {
        let query = ApprovalListQuery {
            kinds: vec![
                ApprovalKind::Subagent,
                ApprovalKind::AgentCliWrite,
                ApprovalKind::Subagent,
            ],
            limit: 5,
            cursor: Some("abc".into()),
        };
        let values: Vec<(String, String)> = list_query(&query)
            .unwrap()
            .into_iter()
            .map(|value| (value.name, value.value))
            .collect();
        assert_eq!(
            values,
            vec![
                ("limit".into(), "5".into()),
                ("cursor".into(), "abc".into()),
                ("kind".into(), "agent_cli_write".into()),
                ("kind".into(), "subagent".into()),
            ]
        );
        for limit in [0, APPROVALS_MAX_LIMIT + 1] {
            let query = ApprovalListQuery {
                limit,
                ..ApprovalListQuery::default()
            };
            assert!(matches!(list_query(&query), Err(CtError::Usage(_))));
        }
        let query = ApprovalListQuery {
            limit: 1,
            cursor: Some("x".repeat(APPROVALS_MAX_CURSOR_CHARS + 1)),
            ..ApprovalListQuery::default()
        };
        assert!(matches!(list_query(&query), Err(CtError::Usage(_))));
    }
}
