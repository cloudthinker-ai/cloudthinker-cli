use std::num::NonZeroU64;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::CtClient;
use crate::error::{CtError, CtResult};

const PAGE_SIZE: u64 = 100;
const MAX_PAGES: u64 = 100;
const MAX_CONTEXT_SOURCES: usize = 200;
const MAX_CONTEXT_FILE_BYTES: usize = 20_000_000;
const MAX_CONTEXT_TOTAL_BYTES: usize = 50_000_000;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CyberMemoryContextSource {
    pub source_id: Uuid,
    pub source_type: String,
    pub name: String,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub local_file: Option<String>,
    pub sha256: Option<String>,
}

#[derive(Debug, Clone)]
pub struct CyberMemoryFile {
    pub relative_path: String,
    pub content_type: String,
    pub sha256: String,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CyberMemorySnapshot {
    pub app_id: Uuid,
    pub findings: Vec<cloudthinker_api::types::FindingPublic>,
    pub surface: Vec<cloudthinker_api::types::SurfaceItemPublic>,
    pub context_sources: Vec<CyberMemoryContextSource>,
    #[serde(skip)]
    pub context_files: Vec<CyberMemoryFile>,
}

impl CtClient {
    pub async fn cyber_pull_memory(
        &self,
        app_id: Uuid,
        include_context: bool,
    ) -> CtResult<CyberMemorySnapshot> {
        let findings = self.pull_findings(app_id).await?;
        let surface = self.pull_surface(app_id).await?;
        let sources = self.pull_context_sources(app_id).await?;
        let (context_sources, context_files) = if include_context {
            self.download_context_sources(app_id, sources).await?
        } else {
            (
                sources
                    .into_iter()
                    .map(|source| CyberMemoryContextSource {
                        source_id: source.id,
                        source_type: source.type_.to_string(),
                        name: source.name,
                        status: source.status,
                        created_at: source.created_at,
                        local_file: None,
                        sha256: None,
                    })
                    .collect(),
                Vec::new(),
            )
        };
        Ok(CyberMemorySnapshot {
            app_id,
            findings,
            surface,
            context_sources,
            context_files,
        })
    }

    async fn pull_findings(
        &self,
        app_id: Uuid,
    ) -> CtResult<Vec<cloudthinker_api::types::FindingPublic>> {
        let mut rows = Vec::new();
        for page_number in 1..=MAX_PAGES {
            let page = NonZeroU64::new(page_number)
                .ok_or_else(|| CtError::Protocol("invalid memory page".into()))?;
            let take = NonZeroU64::new(PAGE_SIZE)
                .ok_or_else(|| CtError::Protocol("invalid memory page size".into()))?;
            let response = self
                .authed(async |client: cloudthinker_api::Client| {
                    client
                        .appsec_list_findings(
                            &app_id,
                            None,
                            Some(page),
                            None,
                            None,
                            Some(cloudthinker_api::types::FindingSort::Severity),
                            Some(cloudthinker_api::types::SortOrder::Asc),
                            None,
                            Some(take),
                            None,
                            None,
                        )
                        .await
                })
                .await?;
            if response.meta.total_items > (MAX_PAGES * PAGE_SIZE) as i64 {
                return Err(CtError::Protocol(
                    "App memory contains more findings than this pull can safely save".into(),
                ));
            }
            let has_next = response.meta.has_next;
            rows.extend(response.data.into_iter().map(|mut finding| {
                for evidence in &mut finding.evidence {
                    evidence.url = None;
                }
                finding
            }));
            if !has_next {
                return Ok(rows);
            }
        }
        Err(CtError::Protocol(
            "App memory findings exceed the 100-page pull limit".into(),
        ))
    }

    async fn pull_surface(
        &self,
        app_id: Uuid,
    ) -> CtResult<Vec<cloudthinker_api::types::SurfaceItemPublic>> {
        let mut rows = Vec::new();
        for page_number in 1..=MAX_PAGES {
            let page = NonZeroU64::new(page_number)
                .ok_or_else(|| CtError::Protocol("invalid memory page".into()))?;
            let take = NonZeroU64::new(PAGE_SIZE)
                .ok_or_else(|| CtError::Protocol("invalid memory page size".into()))?;
            let response = self
                .authed(async |client: cloudthinker_api::Client| {
                    client
                        .appsec_list_surface(
                            &app_id,
                            Some(page),
                            None,
                            Some(cloudthinker_api::types::SurfaceSort::CreatedAt),
                            Some(cloudthinker_api::types::SortOrder::Desc),
                            None,
                            Some(take),
                            None,
                        )
                        .await
                })
                .await?;
            if response.meta.total_items > (MAX_PAGES * PAGE_SIZE) as i64 {
                return Err(CtError::Protocol(
                    "App memory contains more surface items than this pull can safely save".into(),
                ));
            }
            let has_next = response.meta.has_next;
            rows.extend(response.data);
            if !has_next {
                return Ok(rows);
            }
        }
        Err(CtError::Protocol(
            "App memory surface exceeds the 100-page pull limit".into(),
        ))
    }

    async fn pull_context_sources(
        &self,
        app_id: Uuid,
    ) -> CtResult<Vec<cloudthinker_api::types::ContextSourcePublic>> {
        let response = self
            .authed(async |client: cloudthinker_api::Client| {
                client.appsec_list_context(&app_id, None).await
            })
            .await?;
        if response.total > MAX_CONTEXT_SOURCES as i64 {
            return Err(CtError::Protocol(
                "App memory contains more context sources than this pull can safely save".into(),
            ));
        }
        let sources: Vec<_> = response
            .data
            .into_iter()
            .filter(|source| source.type_ != cloudthinker_api::types::ContextType::Environment)
            .collect();
        if sources.len() > MAX_CONTEXT_SOURCES {
            return Err(CtError::Protocol(
                "App memory contains more context sources than this pull can safely save".into(),
            ));
        }
        Ok(sources)
    }

    async fn download_context_sources(
        &self,
        app_id: Uuid,
        sources: Vec<cloudthinker_api::types::ContextSourcePublic>,
    ) -> CtResult<(Vec<CyberMemoryContextSource>, Vec<CyberMemoryFile>)> {
        let mut public_sources = Vec::with_capacity(sources.len());
        let mut files = Vec::with_capacity(sources.len());
        let mut total_bytes = 0usize;
        for source in sources {
            let content = self
                .authed(async |client: cloudthinker_api::Client| {
                    client
                        .appsec_get_context_content_url(&app_id, &source.id, None)
                        .await
                })
                .await?;
            let content_type = content.content_type;
            let content_url = url::Url::parse(&content.content_url)
                .map_err(|_| CtError::Protocol("context content URL was invalid".into()))?;
            if !matches!(content_url.scheme(), "http" | "https") {
                return Err(CtError::Protocol(
                    "context content URL used an unsupported scheme".into(),
                ));
            }
            let response = self
                .probe_http
                .get(content_url)
                .send()
                .await
                .map_err(|_| CtError::Transport("context download failed".into()))?;
            if !response.status().is_success() {
                return Err(CtError::Transport(
                    "context download returned an unsuccessful response".into(),
                ));
            }
            if response
                .content_length()
                .is_some_and(|size| size > MAX_CONTEXT_FILE_BYTES as u64)
            {
                return Err(CtError::Protocol(
                    "context file exceeds the per-file pull limit".into(),
                ));
            }
            let mut bytes = Vec::new();
            let mut response = response;
            while let Some(chunk) = response
                .chunk()
                .await
                .map_err(|_| CtError::Transport("context download failed".into()))?
            {
                if bytes.len().saturating_add(chunk.len()) > MAX_CONTEXT_FILE_BYTES
                    || total_bytes
                        .saturating_add(bytes.len())
                        .saturating_add(chunk.len())
                        > MAX_CONTEXT_TOTAL_BYTES
                {
                    return Err(CtError::Protocol(
                        "context files exceed this pull's byte limits".into(),
                    ));
                }
                bytes.extend_from_slice(&chunk);
            }
            total_bytes = total_bytes.saturating_add(bytes.len());
            let relative_path = context_file_path(source.id, &source.name);
            let sha256 = format!("{:x}", Sha256::digest(&bytes));
            public_sources.push(CyberMemoryContextSource {
                source_id: source.id,
                source_type: source.type_.to_string(),
                name: source.name,
                status: source.status,
                created_at: source.created_at,
                local_file: Some(relative_path.clone()),
                sha256: Some(sha256.clone()),
            });
            files.push(CyberMemoryFile {
                relative_path,
                content_type,
                sha256,
                bytes,
            });
        }
        Ok((public_sources, files))
    }
}

fn context_file_path(source_id: Uuid, name: &str) -> String {
    let safe_name: String = name
        .chars()
        .take(100)
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_') {
                character
            } else {
                '_'
            }
        })
        .collect();
    let safe_name = safe_name.trim_matches('.');
    let safe_name = if safe_name.is_empty() {
        "context"
    } else {
        safe_name
    };
    format!("context/files/{source_id}-{safe_name}")
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;
    use crate::test_support::{MockTokenStore, stored};

    fn page(page: u64, has_next: bool) -> serde_json::Value {
        serde_json::json!({
            "data": [],
            "meta": {
                "page": page,
                "take": PAGE_SIZE,
                "total_items": 0,
                "total_pages": 0,
                "start_index": 0,
                "end_index": 0,
                "has_next": has_next,
                "has_previous": page > 1,
                "should_show_onboarding": false
            }
        })
    }

    #[tokio::test]
    async fn memory_pull_reads_every_findings_and_surface_page() {
        let server = MockServer::start().await;
        let app_id = Uuid::from_u128(10);
        for page_number in 1..=2 {
            let has_next = page_number == 1;
            Mock::given(method("GET"))
                .and(path(format!("/api/v1/appsec/apps/{app_id}/findings")))
                .and(query_param("page", page_number.to_string()))
                .respond_with(ResponseTemplate::new(200).set_body_json(page(page_number, has_next)))
                .expect(1)
                .mount(&server)
                .await;
            let mut surface = page(page_number, has_next);
            surface["counts"] = serde_json::json!([]);
            surface["summary"] = serde_json::json!({
                "total": 0,
                "open_findings": 0,
                "tested_clean": 0,
                "untested": 0
            });
            Mock::given(method("GET"))
                .and(path(format!("/api/v1/appsec/apps/{app_id}/surface")))
                .and(query_param("page", page_number.to_string()))
                .respond_with(ResponseTemplate::new(200).set_body_json(surface))
                .expect(1)
                .mount(&server)
                .await;
        }
        Mock::given(method("GET"))
            .and(path(format!("/api/v1/appsec/apps/{app_id}/context")))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"data": [], "total": 0})),
            )
            .expect(1)
            .mount(&server)
            .await;

        let client = CtClient::new(
            server.uri(),
            Arc::new(MockTokenStore::new(Some(stored("access", "refresh")))),
        )
        .unwrap();
        let snapshot = client.cyber_pull_memory(app_id, false).await.unwrap();
        assert!(snapshot.findings.is_empty());
        assert!(snapshot.surface.is_empty());
        assert!(snapshot.context_sources.is_empty());
        assert_eq!(server.received_requests().await.unwrap().len(), 5);
    }

    #[tokio::test]
    async fn memory_pull_rejects_more_than_the_page_limit() {
        let server = MockServer::start().await;
        let app_id = Uuid::from_u128(11);
        let mut findings = page(1, false);
        findings["meta"]["total_items"] = serde_json::json!(MAX_PAGES * PAGE_SIZE + 1);
        Mock::given(method("GET"))
            .and(path(format!("/api/v1/appsec/apps/{app_id}/findings")))
            .respond_with(ResponseTemplate::new(200).set_body_json(findings))
            .expect(1)
            .mount(&server)
            .await;

        let client = CtClient::new(
            server.uri(),
            Arc::new(MockTokenStore::new(Some(stored("access", "refresh")))),
        )
        .unwrap();
        let error = client.cyber_pull_memory(app_id, false).await.unwrap_err();

        assert!(error.to_string().contains("safely save"));
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn memory_finding_snapshot_removes_temporary_evidence_urls() {
        let server = MockServer::start().await;
        let app_id = Uuid::from_u128(20);
        let finding_id = Uuid::from_u128(21);
        let run_id = Uuid::from_u128(22);
        Mock::given(method("GET"))
            .and(path(format!("/api/v1/appsec/apps/{app_id}/findings")))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [{
                    "id": finding_id,
                    "app_id": app_id,
                    "app_short_id": "app",
                    "display_id": null,
                    "title": "Finding",
                    "finding_type": "sqli",
                    "description_md": "Details",
                    "severity": "high",
                    "status": "open",
                    "triage_state": "none",
                    "manually_resolved": false,
                    "agent_confidence_score": null,
                    "owasp": null,
                    "cwe": null,
                    "cve": null,
                    "affected_surfaces": [],
                    "first_seen_run": run_id,
                    "last_confirmed_run": run_id,
                    "created_at": "2026-09-27T10:00:00Z",
                    "fix_pr": null,
                    "evidence": [{
                        "caption": "proof",
                        "file": "proof.png",
                        "kind": "screenshot",
                        "url": "https://objects.example.test/temporary-signature"
                    }],
                    "api_captures": []
                }],
                "meta": {
                    "page": 1,
                    "take": PAGE_SIZE,
                    "total_items": 1,
                    "total_pages": 1,
                    "start_index": 0,
                    "end_index": 1,
                    "has_next": false,
                    "has_previous": false,
                    "should_show_onboarding": false
                }
            })))
            .expect(1)
            .mount(&server)
            .await;

        let client = CtClient::new(
            server.uri(),
            Arc::new(MockTokenStore::new(Some(stored("access", "refresh")))),
        )
        .unwrap();
        let findings = client.pull_findings(app_id).await.unwrap();

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].evidence[0].file, "proof.png");
        assert!(findings[0].evidence[0].url.is_none());
    }

    #[test]
    fn context_file_names_cannot_escape_the_output_tree() {
        let path = context_file_path(Uuid::from_u128(11), "../../outside/secret.json");
        let parsed = std::path::Path::new(&path);
        assert!(
            parsed
                .components()
                .all(|component| matches!(component, std::path::Component::Normal(_)))
        );
        assert!(path.starts_with("context/files/"));
    }
}
