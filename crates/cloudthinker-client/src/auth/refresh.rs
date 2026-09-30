//! Access-token refresh with process and interprocess serialization.
//!
//! LOAD-BEARING: the backend runs rotating refresh-token *family* reuse
//! detection — two processes refreshing the same token concurrently trips it and
//! revokes the whole family (a random logout). Two guards prevent that:
//!   * process-local single-flight (a `tokio::Mutex`) serialises refreshes;
//!   * an origin-scoped file lock serialises separate CLI processes;
//!   * a guarded store reload re-reads credentials under both locks — if the stored
//!     access token already differs from the one that 401'd, another process
//!     rotated it, so we adopt that token and skip the network entirely.

use std::sync::Arc;

use chrono::Duration as ChronoDuration;

use crate::auth::store::{StoredToken, TokenStore, acquire_credential_lock};
use crate::client::login_command;
use crate::error::{CtError, CtResult};
use crate::retry::with_retries;

/// Proactively refresh when the access token is within this window of expiry.
pub const PROACTIVE_REFRESH_SKEW_SECS: i64 = 60;

const REFRESH_ATTEMPTS: u32 = 3;

pub struct RefreshCoordinator {
    base_url: String,
    store: Arc<dyn TokenStore>,
    // Serialises refreshes in this process (single-flight).
    lock: tokio::sync::Mutex<()>,
    // Anonymous client: the refresh token travels in the body, no auth header.
    http: reqwest::Client,
}

impl RefreshCoordinator {
    pub fn new(base_url: String, store: Arc<dyn TokenStore>, http: reqwest::Client) -> Self {
        Self {
            base_url,
            store,
            lock: tokio::sync::Mutex::new(()),
            http,
        }
    }

    /// Refresh the access token that just failed (`stale_access`).
    ///
    /// Guards against refresh-token-family reuse detection: under the lock we
    /// re-read the store, and if it already holds a newer access token we adopt
    /// it WITHOUT a network call. Exactly one process performs the network
    /// rotation; the rest converge on its result.
    pub async fn refresh(&self, stale_access: &str) -> CtResult<StoredToken> {
        let _guard = self.lock.lock().await;
        let store_lock = acquire_credential_lock(self.store.clone()).await?;

        let login = login_command(&self.base_url);
        let current = self
            .store
            .load_locked(&store_lock)?
            .ok_or_else(|| CtError::Auth(format!("no stored credentials; run `{login}`")))?;

        // Guarded disk reload: someone else already rotated — adopt, skip network.
        if current.access_token != stale_access {
            return Ok(current);
        }

        if !self.store.refresh_enabled() {
            return Err(CtError::Auth(format!(
                "credential is read-only; run `{login}`"
            )));
        }

        let refresh_token = current
            .refresh_token
            .clone()
            .ok_or_else(|| CtError::Auth(format!("no refresh token; run `{login}`")))?;

        let body = cloudthinker_api::types::RefreshTokenRequest {
            refresh_token: Some(refresh_token),
            workspace_id: current.workspace_id,
        };
        let api = cloudthinker_api::Client::new_with_client(&self.base_url, self.http.clone());
        let token = with_retries(REFRESH_ATTEMPTS, || api.login_refresh_token(&body))
            .await
            .map_err(|error| match error {
                CtError::Api {
                    status: 400 | 401, ..
                } => CtError::Auth(format!("session expired; run `{login}`")),
                other => other,
            })?
            .into_inner();

        let mut rotated = StoredToken::from_token(&token);
        rotated.workspace_id = rotated.workspace_id.or(current.workspace_id);
        rotated.workspace_name = current.workspace_name;
        self.store.replace_locked(&store_lock, &rotated)?;
        Ok(rotated)
    }

    /// The skew window used for proactive refresh, as a `chrono::Duration`.
    pub fn proactive_skew() -> ChronoDuration {
        ChronoDuration::seconds(PROACTIVE_REFRESH_SKEW_SECS)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::store::FileStore;
    use crate::test_support::{MockTokenStore, stored, token_json};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    // CA-CLI-7: independent coordinators model separate CLI processes. Their
    // shared origin lock ensures the stale token reaches the network once.
    #[tokio::test]
    async fn ca_cli_7_concurrent_refresh_hits_network_once() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/login/refresh"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(token_json("new-access", "new-refresh")),
            )
            .expect(1)
            .mount(&server)
            .await;

        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(FileStore::new(
            dir.path().join("credentials.json"),
            "test-origin",
        ));
        store.save(&stored("stale-access", "r")).unwrap();
        let first = Arc::new(RefreshCoordinator::new(
            server.uri(),
            store.clone(),
            reqwest::Client::new(),
        ));
        let second = Arc::new(RefreshCoordinator::new(
            server.uri(),
            store.clone(),
            reqwest::Client::new(),
        ));

        let mut handles = Vec::new();
        for index in 0..8 {
            let coord = if index % 2 == 0 {
                first.clone()
            } else {
                second.clone()
            };
            handles.push(tokio::spawn(
                async move { coord.refresh("stale-access").await },
            ));
        }
        for handle in handles {
            let rotated = handle.await.unwrap().unwrap();
            assert_eq!(rotated.access_token, "new-access");
        }
        // `.expect(1)` is verified on server drop.
    }

    // CA-CLI-8: the store already holds a newer token (another process rotated);
    // refresh adopts it and makes ZERO network calls.
    #[tokio::test]
    async fn ca_cli_8_adopts_external_rotation_without_network() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/login/refresh"))
            .respond_with(ResponseTemplate::new(500))
            .expect(0)
            .mount(&server)
            .await;

        let store = Arc::new(MockTokenStore::new(Some(stored("already-rotated", "r"))));
        let coord = RefreshCoordinator::new(server.uri(), store.clone(), reqwest::Client::new());

        let adopted = coord.refresh("stale-access").await.unwrap();
        assert_eq!(adopted.access_token, "already-rotated");
        assert_eq!(store.save_count(), 0, "adoption must not re-save");
    }

    // CA-CLI-9: a legacy detail-only rejection still surfaces as an auth error
    // so the CLI tells the user to log in again.
    #[tokio::test]
    async fn ca_cli_9_refresh_rejection_maps_to_auth() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/login/refresh"))
            .respond_with(ResponseTemplate::new(401).set_body_json(serde_json::json!({
                "detail": "Authentication required."
            })))
            .mount(&server)
            .await;

        let store = Arc::new(MockTokenStore::new(Some(stored("stale-access", "r"))));
        let coord = RefreshCoordinator::new(server.uri(), store, reqwest::Client::new());

        let err = coord.refresh("stale-access").await.unwrap_err();
        assert!(matches!(err, CtError::Auth(_)), "got {err:?}");
    }

    #[tokio::test]
    async fn a_server_error_is_retried_and_never_reads_as_an_expired_session() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/login/refresh"))
            .respond_with(ResponseTemplate::new(503).insert_header("retry-after", "0"))
            .up_to_n_times(1)
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v1/login/refresh"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(token_json("new-access", "new-refresh")),
            )
            .expect(1)
            .mount(&server)
            .await;
        let store = Arc::new(MockTokenStore::new(Some(stored("stale-access", "r"))));
        let coord = RefreshCoordinator::new(server.uri(), store, reqwest::Client::new());

        let rotated = coord.refresh("stale-access").await.unwrap();

        assert_eq!(rotated.access_token, "new-access");
    }

    #[tokio::test]
    async fn a_lasting_outage_reports_the_server_error_after_bounded_attempts() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/login/refresh"))
            .respond_with(ResponseTemplate::new(502).insert_header("retry-after", "0"))
            .expect(u64::from(REFRESH_ATTEMPTS))
            .mount(&server)
            .await;
        let store = Arc::new(MockTokenStore::new(Some(stored("stale-access", "r"))));
        let coord = RefreshCoordinator::new(server.uri(), store.clone(), reqwest::Client::new());

        let err = coord.refresh("stale-access").await.unwrap_err();

        assert!(
            matches!(err, CtError::Api { status: 502, .. }),
            "got {err:?}"
        );
        assert_eq!(store.save_count(), 0);
    }

    #[tokio::test]
    async fn a_refresh_under_another_workspace_keeps_the_active_one_and_its_workspace() {
        let server = MockServer::start().await;
        let mut rotated = token_json("rotated-dev", "rotated-refresh");
        rotated["workspace_id"] = serde_json::Value::Null;
        Mock::given(method("POST"))
            .and(path("/api/v1/login/refresh"))
            .respond_with(ResponseTemplate::new(200).set_body_json(rotated))
            .mount(&server)
            .await;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("credentials.json");
        let workspace = |id: u128, name: &str, access: &str| StoredToken {
            access_token: access.into(),
            refresh_token: Some(format!("refresh-{access}")),
            expires_at: None,
            workspace_id: Some(uuid::Uuid::from_u128(id)),
            workspace_name: Some(name.into()),
        };
        let active = FileStore::new(path.clone(), "origin");
        active.save(&workspace(1, "Development", "dev")).unwrap();
        active.save(&workspace(2, "Production", "prod")).unwrap();
        let development = Arc::new(FileStore::with_selector(
            path,
            "origin",
            crate::WorkspaceSelector::IdOrName("Development".into()),
        ));
        let coord =
            RefreshCoordinator::new(server.uri(), development.clone(), reqwest::Client::new());

        let refreshed = coord.refresh("dev").await.unwrap();

        assert_eq!(refreshed.workspace_id, Some(uuid::Uuid::from_u128(1)));
        assert_eq!(
            development.load().unwrap().unwrap().access_token,
            "rotated-dev"
        );
        assert_eq!(active.load().unwrap().unwrap().access_token, "prod");
    }
}
