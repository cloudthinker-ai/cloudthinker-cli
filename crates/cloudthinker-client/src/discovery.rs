use std::io::{BufRead, BufReader};
use std::path::Path;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::client::{CtClient, CyberDiscoveryTarget, EvidenceFile, EvidenceReceipt};
use crate::error::{CtError, CtResult};

const MAX_DISCOVERY_EVIDENCE_FILE_BYTES: usize = 400 * 1024;
const MAX_DISCOVERY_EVIDENCE_BATCH_BYTES: usize = 512 * 1024;
const MAX_DISCOVERY_EVIDENCE_BATCH_FILES: usize = 100;
const MAX_DISCOVERY_CANDIDATE_BYTES: u64 = 100 * 1024 * 1024;

#[derive(Debug, Serialize)]
pub struct CyberDiscoveryManifest {
    pub version: u64,
    pub run_id: Uuid,
    pub targets: Vec<CyberDiscoveryTarget>,
    pub frameworks: Vec<String>,
    pub mode: String,
    pub intensity: String,
    pub context_dir: String,
    pub workspace_dir: String,
    pub repository_roots: Vec<String>,
    pub identity_labels: Vec<String>,
    pub repository_available: bool,
}

impl CyberDiscoveryManifest {
    pub fn to_json(&self) -> CtResult<String> {
        serde_json::to_string(self)
            .map_err(|error| CtError::Usage(format!("discovery manifest: {error}")))
    }
}

#[derive(Serialize)]
struct DiscoveryIdentities {
    identities: Vec<DiscoveryIdentity>,
}

#[derive(Serialize)]
struct DiscoveryIdentity {
    label: String,
    auth_env: String,
}

#[derive(Serialize)]
struct DiscoveryAuth {
    headers: Vec<String>,
    cookie: Option<String>,
}

pub fn cyber_discovery_identity_manifest(identities: &[(String, String)]) -> CtResult<String> {
    let identities = identities
        .iter()
        .map(|(label, auth_env)| DiscoveryIdentity {
            label: label.clone(),
            auth_env: auth_env.clone(),
        })
        .collect();
    serde_json::to_string(&DiscoveryIdentities { identities })
        .map_err(|error| CtError::Usage(format!("discovery identities: {error}")))
}

pub fn cyber_discovery_auth_value(header: &str) -> CtResult<String> {
    let (name, value) = header
        .split_once(':')
        .ok_or_else(|| CtError::Usage("configured Cyber identity header is malformed".into()))?;
    let name = name.trim();
    let value = value.trim();
    if name.is_empty()
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte))
        || value.is_empty()
        || value.bytes().any(|byte| byte == b'\r' || byte == b'\n')
    {
        return Err(CtError::Usage(
            "configured Cyber identity header is malformed".into(),
        ));
    }
    serde_json::to_string(&DiscoveryAuth {
        headers: vec![format!("{name}: {value}")],
        cookie: None,
    })
    .map_err(|error| CtError::Usage(format!("discovery auth: {error}")))
}

pub fn cyber_discovery_redact(stderr: &[u8], auth_values: &[String]) -> Vec<u8> {
    let mut text = String::from_utf8_lossy(stderr).into_owned();
    for auth in auth_values {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(auth)
            && let Some(headers) = value.get("headers").and_then(serde_json::Value::as_array)
        {
            for header in headers.iter().filter_map(serde_json::Value::as_str) {
                if let Some((_, secret)) = header.split_once(':')
                    && !secret.trim().is_empty()
                {
                    text = text.replace(secret.trim(), "[redacted]");
                }
            }
        }
    }
    text.into_bytes()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CyberDiscoveryHealth {
    pub run_id: Uuid,
    pub overall_state: String,
    pub total_candidates: u64,
    pub usable_current_surface: bool,
    pub blind_spots: Vec<String>,
    pub collector_results: Vec<CyberDiscoveryCollectorHealth>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CyberDiscoveryCollectorHealth {
    pub collector: String,
    pub state: String,
    pub reason: String,
}

#[derive(Deserialize)]
struct CyberDiscoveryReport {
    run_id: Uuid,
    overall_state: String,
    total_candidates: u64,
    usable_current_surface: bool,
    blind_spots: Vec<String>,
    collector_results: Vec<CyberDiscoveryCollectorHealth>,
}

#[derive(Debug)]
pub struct CyberDiscoveryArtifacts {
    pub surfaces: Vec<EvidenceFile>,
    pub report: EvidenceFile,
}

pub fn cyber_discovery_artifacts(
    run_id: Uuid,
    discovery_dir: &Path,
) -> CtResult<(CyberDiscoveryHealth, CyberDiscoveryArtifacts)> {
    let report_path = discovery_dir.join("report.full.json");
    let report_bytes = std::fs::read(&report_path).map_err(|error| {
        CtError::Usage(format!("cannot read {}: {error}", report_path.display()))
    })?;
    if report_bytes.len() > MAX_DISCOVERY_EVIDENCE_BATCH_BYTES {
        return Err(CtError::Usage(
            "discovery report exceeds the evidence batch limit".into(),
        ));
    }
    let report: CyberDiscoveryReport = serde_json::from_slice(&report_bytes)
        .map_err(|error| CtError::Usage(format!("invalid discovery report: {error}")))?;
    if report.run_id != run_id {
        return Err(CtError::Usage(
            "discovery report belongs to a different run".into(),
        ));
    }
    if !matches!(
        report.overall_state.as_str(),
        "COMPLETED" | "PARTIAL" | "FAILED"
    ) {
        return Err(CtError::Usage(
            "discovery report has an unknown overall state".into(),
        ));
    }

    let candidates_path = discovery_dir.join("candidates.ndjson");
    let metadata = std::fs::metadata(&candidates_path).map_err(|error| {
        CtError::Usage(format!(
            "cannot read {}: {error}",
            candidates_path.display()
        ))
    })?;
    if metadata.len() > MAX_DISCOVERY_CANDIDATE_BYTES {
        return Err(CtError::Usage(
            "discovery candidates exceed the local processing limit".into(),
        ));
    }
    let candidates = std::fs::File::open(&candidates_path).map_err(|error| {
        CtError::Usage(format!(
            "cannot read {}: {error}",
            candidates_path.display()
        ))
    })?;
    let mut surfaces = Vec::new();
    let mut candidate_count = 0u64;
    let mut chunks = std::collections::BTreeMap::<String, (usize, Vec<u8>)>::new();
    for (line_index, line) in BufReader::new(candidates).lines().enumerate() {
        let line = line.map_err(|error| {
            CtError::Usage(format!("cannot read discovery candidate line: {error}"))
        })?;
        if line.trim().is_empty() {
            continue;
        }
        candidate_count += 1;
        let row: serde_json::Value = serde_json::from_str(&line).map_err(|error| {
            CtError::Usage(format!(
                "invalid discovery candidate line {}: {error}",
                line_index + 1
            ))
        })?;
        let object = row.as_object().ok_or_else(|| {
            CtError::Usage(format!(
                "discovery candidate line {} is not an object",
                line_index + 1
            ))
        })?;
        let asset_type = object
            .get("asset_type")
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                CtError::Usage(format!(
                    "discovery candidate line {} has no asset_type",
                    line_index + 1
                ))
            })?;
        if !asset_type.bytes().all(|value| {
            value.is_ascii_lowercase() || value.is_ascii_digit() || b"_-".contains(&value)
        }) {
            return Err(CtError::Usage(format!(
                "discovery candidate line {} has an unsafe asset_type",
                line_index + 1
            )));
        }
        if object
            .get("locator")
            .and_then(serde_json::Value::as_str)
            .is_none_or(str::is_empty)
            || !object
                .get("properties")
                .is_some_and(serde_json::Value::is_object)
            || !object
                .get("source")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|value| matches!(value, "documented" | "shadow"))
        {
            return Err(CtError::Usage(format!(
                "discovery candidate line {} is missing normalized surface fields",
                line_index + 1
            )));
        }
        let serialized = serde_json::to_vec(&row)
            .map_err(|error| CtError::Usage(format!("discovery candidate: {error}")))?;
        if serialized.len() + 1 > MAX_DISCOVERY_EVIDENCE_FILE_BYTES {
            return Err(CtError::Usage(format!(
                "discovery candidate line {} exceeds the surface evidence limit",
                line_index + 1
            )));
        }
        let (chunk_index, content) = chunks
            .entry(asset_type.to_string())
            .or_insert_with(|| (1, Vec::new()));
        if content.len() + serialized.len() + 1 > MAX_DISCOVERY_EVIDENCE_FILE_BYTES {
            surfaces.push(surface_evidence_file(asset_type, *chunk_index, content)?);
            *chunk_index += 1;
            content.clear();
        }
        content.extend_from_slice(&serialized);
        content.push(b'\n');
    }
    for (asset_type, (chunk_index, content)) in chunks {
        if !content.is_empty() {
            surfaces.push(surface_evidence_file(&asset_type, chunk_index, &content)?);
        }
    }
    if candidate_count != report.total_candidates {
        return Err(CtError::Usage(
            "discovery candidate count does not match the canonical report".into(),
        ));
    }

    let report_size = u64::try_from(report_bytes.len())
        .map_err(|_| CtError::Usage("discovery report is too large".into()))?;
    let report_content = String::from_utf8(report_bytes)
        .map_err(|error| CtError::Usage(format!("discovery report is not UTF-8: {error}")))?;
    let report_file = EvidenceFile {
        path: "discovery/report.full.json".to_string(),
        content: Some(report_content),
        content_base64: None,
        mime_type: Some("application/json".to_string()),
        size_bytes: report_size,
    };
    let health = CyberDiscoveryHealth {
        run_id: report.run_id,
        overall_state: report.overall_state,
        total_candidates: report.total_candidates,
        usable_current_surface: report.usable_current_surface,
        blind_spots: report.blind_spots,
        collector_results: report.collector_results,
    };
    Ok((
        health,
        CyberDiscoveryArtifacts {
            surfaces,
            report: report_file,
        },
    ))
}

impl CtClient {
    pub async fn cyber_publish_discovery(
        &self,
        run_id: Uuid,
        artifacts: CyberDiscoveryArtifacts,
    ) -> CtResult<()> {
        let mut batch = Vec::new();
        let mut batch_bytes = 0usize;
        for file in artifacts.surfaces {
            let file_bytes = file.content.as_ref().map_or(0, String::len);
            if file_bytes > MAX_DISCOVERY_EVIDENCE_BATCH_BYTES {
                return Err(CtError::Usage(format!(
                    "discovery surface `{}` exceeds the upload batch limit",
                    file.path
                )));
            }
            if !batch.is_empty()
                && (batch.len() >= MAX_DISCOVERY_EVIDENCE_BATCH_FILES
                    || batch_bytes + file_bytes > MAX_DISCOVERY_EVIDENCE_BATCH_BYTES)
            {
                self.submit_surface_batch(run_id, std::mem::take(&mut batch))
                    .await?;
                batch_bytes = 0;
            }
            batch_bytes += file_bytes;
            batch.push(file);
        }
        if !batch.is_empty() {
            self.submit_surface_batch(run_id, batch).await?;
        }
        let receipt = self
            .cyber_submit_evidence(run_id, vec![artifacts.report.clone()])
            .await?;
        ensure_complete_receipt(std::slice::from_ref(&artifacts.report), &receipt)
    }

    async fn submit_surface_batch(&self, run_id: Uuid, files: Vec<EvidenceFile>) -> CtResult<()> {
        let receipt = self.cyber_submit_evidence(run_id, files.clone()).await?;
        ensure_complete_receipt(&files, &receipt)
    }
}

fn ensure_complete_receipt(files: &[EvidenceFile], receipt: &EvidenceReceipt) -> CtResult<()> {
    let expected: std::collections::BTreeSet<_> =
        files.iter().map(|file| file.path.as_str()).collect();
    let written: std::collections::BTreeSet<_> =
        receipt.written.iter().map(String::as_str).collect();
    if !receipt.skipped.is_empty() || written != expected {
        return Err(CtError::Usage(
            "CloudThinker did not persist every discovery evidence file; the report completion marker was withheld".into(),
        ));
    }
    Ok(())
}

fn surface_evidence_file(asset_type: &str, chunk: usize, content: &[u8]) -> CtResult<EvidenceFile> {
    let content = String::from_utf8(content.to_vec())
        .map_err(|error| CtError::Usage(format!("surface evidence is not UTF-8: {error}")))?;
    let size_bytes = u64::try_from(content.len())
        .map_err(|_| CtError::Usage("surface evidence is too large".into()))?;
    Ok(EvidenceFile {
        path: format!("surface/{asset_type}-{chunk}.ndjson"),
        size_bytes,
        content: Some(content),
        content_base64: None,
        mime_type: Some("application/x-ndjson".to_string()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{MockTokenStore, stored};
    use std::sync::Arc;
    use wiremock::matchers::method;
    use wiremock::{Mock, MockServer, Request, ResponseTemplate};

    fn publication_fixture() -> CyberDiscoveryArtifacts {
        let file = |path: &str, size: usize| EvidenceFile {
            path: path.to_string(),
            content: Some("a".repeat(size)),
            content_base64: None,
            mime_type: None,
            size_bytes: u64::try_from(size).unwrap(),
        };
        CyberDiscoveryArtifacts {
            surfaces: vec![
                file("surface/endpoint-1.ndjson", 300_000),
                file("surface/endpoint-2.ndjson", 300_000),
            ],
            report: file("discovery/report.full.json", 2),
        }
    }

    #[tokio::test]
    async fn report_is_published_after_every_surface_batch() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(|request: &Request| {
                let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
                let paths: Vec<_> = body["files"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|file| file["path"].clone())
                    .collect();
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"written": paths, "skipped": []}))
            })
            .mount(&server)
            .await;
        let client = CtClient::new(
            server.uri(),
            Arc::new(MockTokenStore::read_only(Some(stored(
                "test-access",
                "test-refresh",
            )))),
        )
        .unwrap();
        client
            .cyber_publish_discovery(Uuid::from_u128(17), publication_fixture())
            .await
            .unwrap();
        let requests = server.received_requests().await.unwrap();
        let paths: Vec<String> = requests
            .iter()
            .map(|request| {
                let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
                assert_eq!(body["files"].as_array().unwrap().len(), 1);
                body["files"][0]["path"].as_str().unwrap().to_string()
            })
            .collect();
        assert_eq!(
            paths,
            [
                "surface/endpoint-1.ndjson",
                "surface/endpoint-2.ndjson",
                "discovery/report.full.json"
            ]
        );
    }

    #[tokio::test]
    async fn rejected_surface_batch_never_publishes_report() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"written": [], "skipped": []})),
            )
            .mount(&server)
            .await;
        let client = CtClient::new(
            server.uri(),
            Arc::new(MockTokenStore::read_only(Some(stored(
                "test-access",
                "test-refresh",
            )))),
        )
        .unwrap();
        assert!(
            client
                .cyber_publish_discovery(Uuid::from_u128(17), publication_fixture())
                .await
                .is_err()
        );
        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1);
        let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
        assert_eq!(body["files"][0]["path"], "surface/endpoint-1.ndjson");
    }

    #[test]
    fn discovery_artifacts_preserve_properties_and_reject_wrong_run() {
        let directory = tempfile::tempdir().unwrap();
        let run_id = Uuid::from_u128(0x11);
        std::fs::write(
            directory.path().join("report.full.json"),
            format!(
                "{{\"run_id\":\"{run_id}\",\"overall_state\":\"PARTIAL\",\"total_candidates\":1,\"usable_current_surface\":true,\"blind_spots\":[\"bounded\"],\"collector_results\":[{{\"collector\":\"context\",\"state\":\"PARTIAL\",\"reason\":\"bounded\"}}]}}"
            ),
        )
        .unwrap();
        std::fs::write(
            directory.path().join("candidates.ndjson"),
            "{\"asset_type\":\"endpoint\",\"locator\":\"GET /health\",\"properties\":{\"confidence\":\"declared\"},\"source\":\"documented\"}\n",
        )
        .unwrap();

        let (health, artifacts) = cyber_discovery_artifacts(run_id, directory.path()).unwrap();

        assert_eq!(health.overall_state, "PARTIAL");
        assert_eq!(artifacts.report.path, "discovery/report.full.json");
        assert_eq!(artifacts.surfaces[0].path, "surface/endpoint-1.ndjson");
        assert!(
            artifacts.surfaces[0]
                .content
                .as_deref()
                .is_some_and(|content| content.contains("\"properties\""))
        );
        assert!(matches!(
            cyber_discovery_artifacts(Uuid::from_u128(0x12), directory.path()),
            Err(CtError::Usage(message)) if message.contains("different run")
        ));
    }

    #[test]
    fn rejected_surface_receipt_is_not_a_publishable_completion_marker() {
        let files = vec![EvidenceFile {
            path: "surface/endpoint-1.ndjson".to_string(),
            content: Some("{}\n".to_string()),
            content_base64: None,
            mime_type: None,
            size_bytes: 3,
        }];
        let receipt = EvidenceReceipt {
            written: Vec::new(),
            skipped: Vec::new(),
        };

        assert!(matches!(
            ensure_complete_receipt(&files, &receipt),
            Err(CtError::Usage(message)) if message.contains("completion marker was withheld")
        ));
    }
}
