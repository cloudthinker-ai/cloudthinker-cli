use std::collections::BTreeSet;

use cloudthinker_api::types as api;
use serde::Serialize;
use uuid::Uuid;

use super::{CtClient, exceeds_chars};
use crate::error::{CtError, CtResult};

const MAX_WORKSPACE_ROWS: usize = 1000;
const MAX_ORGANIZATION_NAME_CHARS: usize = 255;
const MAX_WORKSPACE_ROLE_CHARS: usize = 64;

#[derive(Debug, Clone, Serialize)]
pub struct WorkspaceView {
    pub id: Uuid,
    pub name: String,
    pub role: String,
    pub organization_name: Option<String>,
    pub is_default: bool,
    pub selected: bool,
    pub saved_login: Option<bool>,
}

#[derive(Debug, Clone)]
pub struct Workspaces {
    pub selected_id: Uuid,
    pub data: Vec<WorkspaceView>,
}

impl Workspaces {
    fn from_rows(
        selected_id: Uuid,
        rows: Vec<api::WorkspaceWithRolePublic>,
        saved: Option<&BTreeSet<Uuid>>,
    ) -> CtResult<Self> {
        let mut data = Vec::with_capacity(rows.len());
        for row in rows {
            let name = String::from(row.name);
            let role = String::from(row.current_user_role).to_lowercase();
            if exceeds_chars(&role, MAX_WORKSPACE_ROLE_CHARS)
                || row
                    .organization_name
                    .as_deref()
                    .is_some_and(|org| exceeds_chars(org, MAX_ORGANIZATION_NAME_CHARS))
            {
                return Err(CtError::Protocol(format!(
                    "workspace {} has oversized metadata; open Workspaces in the browser",
                    row.id
                )));
            }
            data.push(WorkspaceView {
                id: row.id,
                name,
                role,
                organization_name: row.organization_name,
                is_default: row.is_default,
                selected: row.id == selected_id,
                saved_login: saved.map(|saved| saved.contains(&row.id)),
            });
        }
        data.sort_by_cached_key(|row| (row.name.to_lowercase(), row.name.clone(), row.id));
        Ok(Self { selected_id, data })
    }

    pub fn includes_selected(&self) -> bool {
        self.data.iter().any(|row| row.selected)
    }

    pub fn organization_name(&self) -> Option<&str> {
        self.data
            .iter()
            .find(|row| row.selected)
            .or_else(|| self.data.first())
            .and_then(|row| row.organization_name.as_deref())
    }
}

impl CtClient {
    pub async fn list_workspaces(&self, saved: Option<&BTreeSet<Uuid>>) -> CtResult<Workspaces> {
        let identity = self.whoami().await?;
        let mut rows: Vec<api::WorkspaceWithRolePublic> = Vec::new();
        let mut ids = BTreeSet::new();
        loop {
            let skip = rows.len() as u64;
            let page = self
                .authed(async |c: cloudthinker_api::Client| {
                    c.workspaces_get_workspaces(None, None, Some(skip)).await
                })
                .await?;
            if page.data.is_empty() {
                break;
            }
            if !page.data.iter().all(|row| ids.insert(row.id)) {
                return Err(CtError::Protocol(
                    "the workspace list changed while it was read; retry".into(),
                ));
            }
            rows.extend(page.data);
            if rows.len() > MAX_WORKSPACE_ROWS {
                return Err(CtError::Protocol(format!(
                    "you can access more than {MAX_WORKSPACE_ROWS} workspaces; open Workspaces in the browser"
                )));
            }
        }
        Workspaces::from_rows(identity.workspace_id, rows, saved)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;
    use crate::test_support::{MockTokenStore, stored};

    const SELECTED: Uuid = Uuid::from_u128(1);
    const OTHER: Uuid = Uuid::from_u128(2);

    fn workspace(id: Uuid, name: &str, role: &str) -> serde_json::Value {
        serde_json::json!({
            "id": id,
            "name": name,
            "current_user_role": role,
            "organization_name": "Acme",
            "is_default": id == SELECTED,
            "created_at": "2026-10-01T00:00:00Z",
            "updated_at": "2026-10-01T00:00:00Z"
        })
    }

    fn page(rows: Vec<serde_json::Value>) -> ResponseTemplate {
        let count = rows.len();
        ResponseTemplate::new(200).set_body_json(serde_json::json!({"data": rows, "count": count}))
    }

    async fn server_with_identity() -> MockServer {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/cli/whoami"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "user_id": Uuid::from_u128(9),
                "user_email": "user@example.com",
                "workspace_id": SELECTED,
                "workspace_name": "Prod"
            })))
            .mount(&server)
            .await;
        server
    }

    fn client(server: &MockServer) -> CtClient {
        CtClient::new(
            server.uri(),
            Arc::new(MockTokenStore::new(Some(stored("access", "refresh")))),
        )
        .unwrap()
    }

    #[tokio::test]
    async fn list_workspaces_reads_every_page_and_marks_the_selected_one() {
        let server = server_with_identity().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/workspaces/"))
            .and(query_param("skip", "0"))
            .respond_with(page(vec![workspace(SELECTED, "prod", "ADMIN")]))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/workspaces/"))
            .and(query_param("skip", "1"))
            .respond_with(page(vec![workspace(OTHER, "Dev", "AUDITOR")]))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/workspaces/"))
            .and(query_param("skip", "2"))
            .respond_with(page(vec![]))
            .expect(1)
            .mount(&server)
            .await;

        let saved = BTreeSet::from([OTHER]);
        let workspaces = client(&server).list_workspaces(Some(&saved)).await.unwrap();

        let rows: Vec<_> = workspaces
            .data
            .iter()
            .map(|row| {
                (
                    row.name.as_str(),
                    row.role.as_str(),
                    row.selected,
                    row.saved_login,
                )
            })
            .collect();
        assert_eq!(
            rows,
            [
                ("Dev", "auditor", false, Some(true)),
                ("prod", "admin", true, Some(false)),
            ]
        );
        assert!(workspaces.includes_selected());
        assert_eq!(workspaces.organization_name(), Some("Acme"));
    }

    #[tokio::test]
    async fn list_workspaces_handles_an_empty_list() {
        let server = server_with_identity().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/workspaces/"))
            .respond_with(page(vec![]))
            .expect(1)
            .mount(&server)
            .await;

        let workspaces = client(&server).list_workspaces(None).await.unwrap();

        assert!(workspaces.data.is_empty());
        assert!(!workspaces.includes_selected());
        assert_eq!(workspaces.selected_id, SELECTED);
    }

    #[tokio::test]
    async fn list_workspaces_rejects_a_repeated_row() {
        let server = server_with_identity().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/workspaces/"))
            .respond_with(page(vec![workspace(SELECTED, "prod", "ADMIN")]))
            .mount(&server)
            .await;

        let error = client(&server).list_workspaces(None).await.unwrap_err();

        assert!(matches!(error, CtError::Protocol(message) if message.contains("retry")));
        assert_eq!(server.received_requests().await.unwrap().len(), 3);
    }

    #[tokio::test]
    async fn list_workspaces_keeps_a_permission_denial_as_an_api_error() {
        let server = server_with_identity().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/workspaces/"))
            .respond_with(
                ResponseTemplate::new(403)
                    .set_body_json(serde_json::json!({"detail": "Not enough permissions"})),
            )
            .mount(&server)
            .await;

        let error = client(&server).list_workspaces(None).await.unwrap_err();

        assert!(matches!(error, CtError::Api { status: 403, .. }));
    }
}
