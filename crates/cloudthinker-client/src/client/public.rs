use std::collections::HashSet;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use cloudthinker_spec::{Method, Operation, OperationTable, SpecError};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::CtClient;
use crate::auth::store::EnvTokenStore;
use crate::error::{CtError, CtResult, parse_error_message};
use crate::update_cache::{cloudthinker_home, write_file, write_json_file};
use crate::worker_api::{CappedReadError, read_capped};

const PUBLIC_BASE: &str = "/api/public";
const SPEC_PATH: &str = "/api/public/v1/openapi.json";
const TOKEN_SELF_PATH: &str = "/api/public/v1/token";
const PAT_PREFIX: &str = "ct_pat_";
const PAT_BODY_CHARS: usize = 46;
const MAX_TOKEN_WORKSPACES: usize = 100;
const SPEC_MAX_BYTES: usize = 8 * 1024 * 1024;
const RESPONSE_MAX_BYTES: usize = 16 * 1024 * 1024;
const BODY_MAX_BYTES: usize = 256 * 1024;
const SPEC_FRESH_SECS: u64 = 6 * 60 * 60;
const MAX_ITEMS: usize = 10_000;
const MAX_HEADER_BYTES: usize = 255;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpecFreshness {
    Fresh,
    Revalidated,
    Offline,
}

#[derive(Debug)]
pub struct PublicSpec {
    pub table: OperationTable,
    pub freshness: SpecFreshness,
}

#[derive(Debug, Clone)]
pub struct QueryValue {
    pub name: String,
    pub value: String,
}

#[derive(Debug, Clone)]
pub struct BodyField {
    pub key: String,
    pub value: String,
}

#[derive(Debug, Default, Clone)]
pub struct BodyInput {
    pub raw: Option<String>,
    pub fields: Vec<BodyField>,
}

#[derive(Debug, Clone)]
pub struct PublicCall {
    pub method: Method,
    pub path: String,
    pub query: Vec<QueryValue>,
    pub body: BodyInput,
    pub idempotency_key: Option<String>,
    pub if_match: Option<String>,
    pub all_pages: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(transparent)]
pub struct JsonDocument(serde_json::Value);

#[derive(Debug)]
pub struct PublicReply {
    pub body: JsonDocument,
    pub request_id: Option<String>,
    pub etag: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TokenView {
    pub token_id: Uuid,
    pub user_id: Uuid,
    pub client: String,
    /// Absent on servers that predate workspace roles; they send `scopes`.
    pub role: Option<String>,
    #[serde(default)]
    pub scopes: Vec<String>,
    pub workspace_ids: Vec<Uuid>,
    pub expires_at: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Serialize, Deserialize)]
struct SpecMeta {
    origin: String,
    etag: Option<String>,
    checked_at_unix: u64,
}

struct SpecCachePaths {
    meta: PathBuf,
    spec: PathBuf,
}

struct CachedSpec {
    meta: SpecMeta,
    spec: Vec<u8>,
}

#[derive(Deserialize)]
struct PageShape {
    items: Vec<serde_json::Value>,
    next_cursor: Option<String>,
}

#[derive(Serialize)]
struct MergedPage {
    items: Vec<serde_json::Value>,
    next_cursor: Option<String>,
}

struct Reply {
    status: u16,
    request_id: Option<String>,
    etag: Option<String>,
    location: Option<String>,
    body: Vec<u8>,
}

impl CtClient {
    pub async fn public_spec(&self) -> CtResult<PublicSpec> {
        let paths = spec_cache_paths(&self.base_url)?;
        let cached = read_cache(&paths, &self.base_url);
        let now = now_unix();
        if let Some(cache) = &cached
            && now.saturating_sub(cache.meta.checked_at_unix) < SPEC_FRESH_SECS
        {
            return Ok(PublicSpec {
                table: parse_spec(&cache.spec)?,
                freshness: SpecFreshness::Fresh,
            });
        }
        let url = format!("{}{SPEC_PATH}", self.base_url);
        let mut request = self.probe_http.get(&url);
        if let Some(etag) = cached.as_ref().and_then(|c| c.meta.etag.as_deref()) {
            request = request.header(reqwest::header::IF_NONE_MATCH, etag);
        }
        let reply = match request.send().await {
            Ok(response) => read_reply(response, SPEC_MAX_BYTES).await,
            Err(error) => Err(CtError::Transport(format!("public API spec: {error}"))),
        };
        let reply = match (reply, cached) {
            (Ok(reply), Some(mut cache)) if reply.status == 304 => {
                cache.meta.checked_at_unix = now;
                let table = parse_spec(&cache.spec)?;
                write_json_file(&paths.meta, &cache.meta)?;
                return Ok(PublicSpec {
                    table,
                    freshness: SpecFreshness::Revalidated,
                });
            }
            (Ok(reply), _) if reply.status == 200 => reply,
            (Ok(reply), _) => {
                return Err(CtError::Api {
                    status: reply.status,
                    detail: Some(format!("could not read the public API spec at {url}")),
                });
            }
            (Err(error), Some(cache)) if error.is_transport() => {
                return Ok(PublicSpec {
                    table: parse_spec(&cache.spec)?,
                    freshness: SpecFreshness::Offline,
                });
            }
            (Err(error), _) => return Err(error),
        };
        let table = parse_spec(&reply.body)?;
        write_file(&paths.spec, &reply.body)?;
        write_json_file(
            &paths.meta,
            &SpecMeta {
                origin: self.base_url.clone(),
                etag: reply.etag,
                checked_at_unix: now,
            },
        )?;
        Ok(PublicSpec {
            table,
            freshness: SpecFreshness::Fresh,
        })
    }

    pub async fn token_view(&self, token: &str) -> CtResult<TokenView> {
        let mut bearer = reqwest::header::HeaderValue::from_str(&format!("Bearer {token}"))
            .map_err(|_| CtError::Usage("the token has characters a header cannot carry".into()))?;
        bearer.set_sensitive(true);
        let response = self
            .probe_http
            .get(format!("{}{TOKEN_SELF_PATH}", self.base_url))
            .header(reqwest::header::AUTHORIZATION, bearer)
            .header(reqwest::header::ACCEPT, "application/json")
            .send()
            .await
            .map_err(|e| CtError::Transport(format!("token check: {e}")))?;
        let reply = read_reply(response, RESPONSE_MAX_BYTES).await?;
        match reply.status {
            200 => {
                let found: TokenView = serde_json::from_slice(&reply.body).map_err(|_| {
                    CtError::Protocol("the token check answered an unknown shape".into())
                })?;
                if found.workspace_ids.is_empty()
                    || found.workspace_ids.len() > MAX_TOKEN_WORKSPACES
                {
                    return Err(CtError::Protocol(format!(
                        "the token covers {} workspaces; expected 1 to {MAX_TOKEN_WORKSPACES}",
                        found.workspace_ids.len()
                    )));
                }
                Ok(found)
            }
            401 => Err(CtError::Auth(
                "the server rejected this token; it may be revoked, expired or for another host"
                    .into(),
            )),
            status => Err(CtError::Api {
                status,
                detail: parse_error_message(&String::from_utf8_lossy(&reply.body)),
            }),
        }
    }

    pub(crate) async fn public_json<T: DeserializeOwned>(
        &self,
        method: Method,
        rest: &str,
        query: &[QueryValue],
        body: Option<&[u8]>,
        idempotency_key: Option<&str>,
    ) -> CtResult<T> {
        let workspace = self.default_workspace().await?;
        let call = PublicCall {
            method,
            path: format!("/v1/workspaces/{workspace}{rest}"),
            query: Vec::new(),
            body: BodyInput::default(),
            idempotency_key: None,
            if_match: None,
            all_pages: false,
        };
        let key = match idempotency_key {
            Some(key) => Some(check_idempotency_key(key)?),
            None => method.is_write().then(|| Uuid::new_v4().to_string()),
        };
        let reply = self
            .public_send(&call, query, body, key.as_deref())
            .await
            .map_err(|error| super::reclassify(error, &self.base_url))?;
        match reply.status {
            200..=299 => serde_json::from_slice(&reply.body).map_err(|_| {
                CtError::Protocol(format!(
                    "the public API answered {} with an unknown shape",
                    call.path
                ))
            }),
            status => Err(super::reclassify(
                CtError::Api {
                    status,
                    detail: parse_error_message(&String::from_utf8_lossy(&reply.body)),
                },
                &self.base_url,
            )),
        }
    }

    pub async fn default_workspace(&self) -> CtResult<Uuid> {
        if let Some(id) = self.store.load()?.and_then(|token| token.workspace_id) {
            return Ok(id);
        }
        let access = self.access_token().await?;
        let found = self.token_view(&access).await?;
        match found.workspace_ids.as_slice() {
            [only] => Ok(*only),
            many => Err(CtError::Usage(format!(
                "this token covers {} workspaces; use a token for one workspace",
                many.len()
            ))),
        }
    }

    pub fn public_workspace(&self, explicit: Option<&str>) -> CtResult<Uuid> {
        if let Some(raw) = explicit {
            return Uuid::parse_str(raw).or_else(|_| {
                if EnvTokenStore::from_env().is_some() {
                    Err(CtError::Usage(format!(
                        "with CLOUDTHINKER_TOKEN, pass the workspace as a UUID, not '{raw}'"
                    )))
                } else {
                    self.stored_workspace()
                }
            });
        }
        self.stored_workspace()
    }

    fn stored_workspace(&self) -> CtResult<Uuid> {
        self.store
            .load()?
            .and_then(|token| token.workspace_id)
            .ok_or_else(|| CtError::Usage("pass the workspace ID with -w <workspace-id>".into()))
    }

    pub async fn public_call(&self, call: &PublicCall) -> CtResult<PublicReply> {
        let body = build_body(&call.body)?;
        if body.is_some() && call.method == Method::Get {
            return Err(CtError::Usage("a GET operation takes no body".into()));
        }
        let idempotency_key = match (&call.idempotency_key, call.method.is_write()) {
            (Some(key), true) => Some(check_idempotency_key(key)?),
            (Some(_), false) => {
                return Err(CtError::Usage("--key applies to writes only".into()));
            }
            (None, true) => Some(Uuid::new_v4().to_string()),
            (None, false) => None,
        };
        if let Some(etag) = &call.if_match {
            check_header_value("--etag", etag)?;
        }
        let mut query = call.query.clone();
        let mut merged: Vec<serde_json::Value> = Vec::new();
        let mut seen_cursors = HashSet::new();
        loop {
            let reply = self
                .public_send(call, &query, body.as_deref(), idempotency_key.as_deref())
                .await?;
            let PublicReply {
                body: JsonDocument(document),
                request_id,
                etag,
            } = interpret(reply)?;
            if !call.all_pages {
                return Ok(PublicReply {
                    body: JsonDocument(document),
                    request_id,
                    etag,
                });
            }
            let page: PageShape = serde_json::from_value(document)
                .map_err(|_| CtError::Usage("--all needs a list operation".into()))?;
            let page_fits = page.items.len() <= MAX_ITEMS - merged.len();
            let next_cursor = if page_fits {
                merged.extend(page.items);
                page.next_cursor
            } else {
                if merged.is_empty() {
                    return Err(CtError::Protocol(
                        "The first page exceeds 10,000 items. Try a smaller page size.".into(),
                    ));
                }
                query
                    .iter()
                    .find(|param| param.name == "cursor")
                    .map(|param| param.value.clone())
            };
            let cursor = match next_cursor {
                Some(cursor) if page_fits && merged.len() < MAX_ITEMS => cursor,
                next_cursor => {
                    let document = serde_json::to_value(MergedPage {
                        items: merged,
                        next_cursor,
                    })
                    .map_err(|e| CtError::Protocol(e.to_string()))?;
                    return Ok(PublicReply {
                        body: JsonDocument(document),
                        request_id,
                        etag: None,
                    });
                }
            };
            if !seen_cursors.insert(cursor.clone()) {
                return Err(CtError::Protocol(format!(
                    "the server repeated cursor {cursor}"
                )));
            }
            query.retain(|q| q.name != "cursor");
            query.push(QueryValue {
                name: "cursor".into(),
                value: cursor,
            });
        }
    }

    async fn public_send(
        &self,
        call: &PublicCall,
        query: &[QueryValue],
        body: Option<&[u8]>,
        idempotency_key: Option<&str>,
    ) -> CtResult<Reply> {
        let access = self.access_token().await?;
        let reply = self
            .public_attempt(&access, call, query, body, idempotency_key)
            .await?;
        if reply.status != 401 || !self.store.refresh_enabled() {
            return self.public_accept(&access, reply);
        }
        let rotated = self
            .refresh
            .refresh(&access)
            .await
            .map_err(|error| self.record_auth_failure(&access, error))?;
        let reply = self
            .public_attempt(&rotated.access_token, call, query, body, idempotency_key)
            .await?;
        self.public_accept(&rotated.access_token, reply)
    }

    fn public_accept(&self, access: &str, reply: Reply) -> CtResult<Reply> {
        if reply.status == 401 {
            let detail = parse_error_message(&String::from_utf8_lossy(&reply.body));
            return Err(self.record_auth_failure(
                access,
                CtError::Api {
                    status: 401,
                    detail,
                },
            ));
        }
        self.accept_credential(access);
        Ok(reply)
    }

    async fn public_attempt(
        &self,
        access: &str,
        call: &PublicCall,
        query: &[QueryValue],
        body: Option<&[u8]>,
        idempotency_key: Option<&str>,
    ) -> CtResult<Reply> {
        let method = match call.method {
            Method::Get => reqwest::Method::GET,
            Method::Post => reqwest::Method::POST,
            Method::Patch => reqwest::Method::PATCH,
            Method::Delete => reqwest::Method::DELETE,
        };
        let pairs: Vec<(&str, &str)> = query
            .iter()
            .map(|q| (q.name.as_str(), q.value.as_str()))
            .collect();
        let mut bearer = reqwest::header::HeaderValue::from_str(&format!("Bearer {access}"))
            .map_err(|e| CtError::Auth(format!("invalid token header: {e}")))?;
        bearer.set_sensitive(true);
        let mut request = self
            .probe_http
            .request(
                method,
                format!("{}{PUBLIC_BASE}{}", self.base_url, call.path),
            )
            .query(&pairs)
            .header(reqwest::header::AUTHORIZATION, bearer)
            .header(reqwest::header::ACCEPT, "application/json")
            .timeout(self.request_timeout);
        if let Some(key) = idempotency_key {
            request = request.header("Idempotency-Key", key);
        }
        if let Some(etag) = &call.if_match {
            request = request.header(reqwest::header::IF_MATCH, etag);
        }
        if let Some(bytes) = body {
            request = request
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .body(bytes.to_vec());
        }
        let response = request
            .send()
            .await
            .map_err(|e| CtError::Transport(format!("public API call: {e}")))?;
        read_reply(response, RESPONSE_MAX_BYTES).await
    }
}

impl JsonDocument {
    pub fn is_empty_object(&self) -> bool {
        self.0.as_object().is_some_and(serde_json::Map::is_empty)
    }
}

pub fn operation_summary(op: &Operation) -> String {
    op.summary
        .clone()
        .unwrap_or_else(|| format!("{} {}", op.method, op.path))
}

fn interpret(reply: Reply) -> CtResult<PublicReply> {
    let request_id = reply.request_id.clone();
    match reply.status {
        200..=299 => done(reply),
        300..=399 => Err(CtError::Protocol(format!(
            "the server answered {} (redirect to {}); redirects are not followed",
            reply.status,
            reply.location.as_deref().unwrap_or("an unknown location")
        ))),
        status => {
            let message = parse_error_message(&String::from_utf8_lossy(&reply.body));
            let detail = match (message, request_id) {
                (Some(message), Some(id)) => Some(format!("{message} (request {id})")),
                (Some(message), None) => Some(message),
                (None, Some(id)) => Some(format!("request {id}")),
                (None, None) => None,
            };
            Err(CtError::Api { status, detail })
        }
    }
}

fn done(reply: Reply) -> CtResult<PublicReply> {
    let body = if reply.body.is_empty() {
        serde_json::Value::Object(serde_json::Map::new())
    } else {
        serde_json::from_slice(&reply.body).map_err(|_| {
            CtError::Protocol("the public API answered with a body that is not JSON".into())
        })?
    };
    Ok(PublicReply {
        body: JsonDocument(body),
        request_id: reply.request_id,
        etag: reply.etag,
    })
}

async fn read_reply(response: reqwest::Response, max_bytes: usize) -> CtResult<Reply> {
    let header = |name: reqwest::header::HeaderName| {
        response
            .headers()
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string)
    };
    let status = response.status().as_u16();
    let request_id = header(reqwest::header::HeaderName::from_static("x-request-id"));
    let etag = header(reqwest::header::ETAG);
    let location = header(reqwest::header::LOCATION);
    let body = read_capped(response, max_bytes)
        .await
        .map_err(|error| match error {
            CappedReadError::TooLarge(_) => CtError::Protocol(error.to_string()),
            CappedReadError::Transport(_) => CtError::Transport(error.to_string()),
        })?;
    Ok(Reply {
        status,
        request_id,
        etag,
        location,
        body,
    })
}

fn build_body(input: &BodyInput) -> CtResult<Option<Vec<u8>>> {
    let mut document = match input.raw.as_deref() {
        None if input.fields.is_empty() => return Ok(None),
        None => serde_json::Value::Object(serde_json::Map::new()),
        Some(raw) => serde_json::from_str(raw)
            .map_err(|e| CtError::Usage(format!("--body is not valid JSON: {e}")))?,
    };
    if !input.fields.is_empty() {
        let object = document
            .as_object_mut()
            .ok_or_else(|| CtError::Usage("--set needs --body to be a JSON object".into()))?;
        for field in &input.fields {
            let value = serde_json::from_str(&field.value)
                .unwrap_or_else(|_| serde_json::Value::String(field.value.clone()));
            object.insert(field.key.clone(), value);
        }
    }
    let bytes = serde_json::to_vec(&document).map_err(|e| CtError::Usage(e.to_string()))?;
    if bytes.len() > BODY_MAX_BYTES {
        return Err(CtError::Usage(format!(
            "the body is larger than {BODY_MAX_BYTES} bytes"
        )));
    }
    Ok(Some(bytes))
}

pub(super) fn check_idempotency_key(key: &str) -> CtResult<String> {
    let valid = (1..=MAX_HEADER_BYTES).contains(&key.len())
        && key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    if valid {
        Ok(key.to_string())
    } else {
        Err(CtError::Usage(
            "--key must be 1-255 letters, digits, '-' or '_'".into(),
        ))
    }
}

fn check_header_value(flag: &str, value: &str) -> CtResult<()> {
    let valid = (1..=MAX_HEADER_BYTES).contains(&value.len())
        && value.bytes().all(|b| (0x20..0x7f).contains(&b));
    if valid {
        Ok(())
    } else {
        Err(CtError::Usage(format!(
            "{flag} must be 1-255 printable ASCII characters"
        )))
    }
}

pub fn check_personal_token(token: &str) -> CtResult<()> {
    let body = token.strip_prefix(PAT_PREFIX).unwrap_or_default();
    if body.len() == PAT_BODY_CHARS && body.bytes().all(|b| b.is_ascii_alphanumeric()) {
        Ok(())
    } else {
        Err(CtError::Usage(format!(
            "expected a CloudThinker access token that starts with {PAT_PREFIX}"
        )))
    }
}

pub fn cached_public_spec(base_url: &str) -> Option<OperationTable> {
    let cache = read_cache(&spec_cache_paths(base_url).ok()?, base_url)?;
    cloudthinker_spec::parse(&cache.spec).ok()
}

pub fn read_public_spec_file(path: &std::path::Path) -> CtResult<OperationTable> {
    let bytes = std::fs::read(path)
        .map_err(|e| CtError::Usage(format!("could not read {}: {e}", path.display())))?;
    parse_spec(&bytes)
}

fn parse_spec(bytes: &[u8]) -> CtResult<OperationTable> {
    cloudthinker_spec::parse(bytes).map_err(|error| match error {
        SpecError::Json(_) | SpecError::Invalid(_) => {
            CtError::Protocol(format!(
                "refusing the public API spec: {error}; this CLI may be older than the server, so run `cloudthinker update`"
            ))
        }
    })
}

fn spec_cache_paths(origin: &str) -> CtResult<SpecCachePaths> {
    let digest = Sha256::digest(origin.as_bytes());
    let name: String = digest.iter().take(12).map(|b| format!("{b:02x}")).collect();
    let folder = cloudthinker_home()?.join("public-api");
    Ok(SpecCachePaths {
        meta: folder.join(format!("{name}.json")),
        spec: folder.join(format!("{name}.spec.json")),
    })
}

fn read_cache(paths: &SpecCachePaths, origin: &str) -> Option<CachedSpec> {
    let meta: SpecMeta = serde_json::from_slice(&std::fs::read(&paths.meta).ok()?).ok()?;
    if meta.origin != origin {
        return None;
    }
    let spec = std::fs::read(&paths.spec).ok()?;
    Some(CachedSpec { meta, spec })
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod pagination_tests {
    use std::sync::Arc;
    use std::time::Instant;

    use serde_json::json;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

    use super::*;
    use crate::test_support::{MockTokenStore, stored};

    #[derive(Clone)]
    struct Pages {
        count: usize,
        size: usize,
        cycle: bool,
        empty_first: bool,
    }

    fn cursor(index: usize) -> String {
        format!("{}-{index:08}", "opaque".repeat(30))
    }

    impl Respond for Pages {
        fn respond(&self, request: &Request) -> ResponseTemplate {
            let index = request
                .url
                .query_pairs()
                .find(|(key, _)| key == "cursor")
                .and_then(|(_, value)| value.rsplit('-').next()?.parse::<usize>().ok())
                .unwrap_or(0);
            let next = if self.cycle && index == self.count - 1 {
                Some(cursor(1))
            } else if index + 1 < self.count {
                Some(cursor(index + 1))
            } else {
                None
            };
            let items = if self.empty_first && index == 0 {
                Vec::new()
            } else {
                (index * self.size..(index + 1) * self.size).collect::<Vec<_>>()
            };
            ResponseTemplate::new(200)
                .insert_header("X-Request-Id", format!("pagination-fixture-{index}"))
                .insert_header("ETag", "fixture-etag")
                .set_body_json(json!({"items": items, "next_cursor": next}))
        }
    }

    async fn fixture(pages: Pages) -> (MockServer, CtClient, PublicCall) {
        let server = MockServer::builder()
            .disable_request_recording()
            .start()
            .await;
        Mock::given(method("GET"))
            .and(path("/api/public/v1/workspaces/test/items"))
            .respond_with(pages)
            .mount(&server)
            .await;
        let client = CtClient::new(
            server.uri(),
            Arc::new(MockTokenStore::read_only(Some(stored("fixture", "unused")))),
        )
        .unwrap();
        let call = PublicCall {
            method: Method::Get,
            path: "/v1/workspaces/test/items".into(),
            query: Vec::new(),
            body: BodyInput::default(),
            idempotency_key: None,
            if_match: None,
            all_pages: true,
        };
        (server, client, call)
    }

    #[tokio::test]
    async fn pagination_preserves_order_and_empty_pages() {
        let (_server, client, mut call) = fixture(Pages {
            count: 4,
            size: 3,
            cycle: false,
            empty_first: true,
        })
        .await;
        let reply = client.public_call(&call).await.unwrap();
        assert_eq!(
            reply.body.0,
            json!({"items": (3..12).collect::<Vec<_>>(), "next_cursor": null})
        );
        assert_eq!(reply.request_id.as_deref(), Some("pagination-fixture-3"));
        assert_eq!(reply.etag, None);
        call.all_pages = false;
        let reply = client.public_call(&call).await.unwrap();
        assert_eq!(reply.body.0, json!({"items": [], "next_cursor": cursor(1)}));
        assert_eq!(reply.request_id.as_deref(), Some("pagination-fixture-0"));
        assert_eq!(reply.etag.as_deref(), Some("fixture-etag"));
    }

    #[tokio::test]
    async fn pagination_rejects_a_non_adjacent_cursor_cycle() {
        let (_server, client, call) = fixture(Pages {
            count: 4,
            size: 1,
            cycle: true,
            empty_first: false,
        })
        .await;
        let error = client.public_call(&call).await.unwrap_err();
        assert!(
            matches!(error, CtError::Protocol(message) if message == format!("the server repeated cursor {}", cursor(1)))
        );
    }

    #[tokio::test]
    async fn pagination_retains_the_item_limit_and_resume_cursor() {
        let (_server, client, call) = fixture(Pages {
            count: 102,
            size: 100,
            cycle: false,
            empty_first: false,
        })
        .await;
        let reply = client.public_call(&call).await.unwrap();
        assert_eq!(
            reply.body.0,
            json!({"items": (0..10_000).collect::<Vec<_>>(), "next_cursor": cursor(100)})
        );
    }

    #[tokio::test]
    async fn pagination_resumes_at_the_first_whole_page_over_the_limit() {
        let (_server, client, mut call) = fixture(Pages {
            count: 103,
            size: 99,
            cycle: false,
            empty_first: false,
        })
        .await;
        let reply = client.public_call(&call).await.unwrap();
        assert_eq!(
            reply.body.0,
            json!({"items": (0..9_999).collect::<Vec<_>>(), "next_cursor": cursor(101)})
        );

        call.query.push(QueryValue {
            name: "cursor".into(),
            value: reply.body.0["next_cursor"].as_str().unwrap().into(),
        });
        let reply = client.public_call(&call).await.unwrap();
        assert_eq!(
            reply.body.0,
            json!({"items": (9_999..10_197).collect::<Vec<_>>(), "next_cursor": null})
        );
    }

    #[tokio::test]
    #[ignore = "release-mode HTTP boundary benchmark; run explicitly with --ignored --nocapture"]
    async fn pagination_benchmark() {
        for (count, size) in [(100, 100), (1000, 10), (10_000, 1)] {
            let (_server, client, call) = fixture(Pages {
                count,
                size,
                cycle: false,
                empty_first: false,
            })
            .await;
            let expected =
                json!({"items": (0..count * size).collect::<Vec<_>>(), "next_cursor": null});
            let mut times = Vec::new();
            for run in 0..6 {
                let started = Instant::now();
                let reply = client.public_call(&call).await.unwrap();
                let elapsed = started.elapsed();
                assert_eq!(reply.body.0, expected);
                if run > 0 {
                    times.push(elapsed.as_secs_f64() * 1000.0);
                }
            }
            times.sort_by(f64::total_cmp);
            println!(
                "pages={count} items_per_page={size} median_ms={:.3} samples_ms={times:?}",
                times[2]
            );
        }
    }
}
